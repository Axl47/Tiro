package dev.axl47.tiro

import android.accessibilityservice.AccessibilityService
import android.content.ClipData
import android.content.ClipboardManager
import android.media.AudioManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.os.VibrationEffect
import android.os.Vibrator
import android.util.Log
import android.view.KeyEvent
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import java.util.concurrent.Executors

/**
 * Dictation from any keyboard: while a text field and a keyboard are on screen,
 * hold volume-down to talk and let go to finish, as with the Mac's shortcut, or
 * double-tap it to talk hands-free and press it once more to finish. A single
 * quick press still lowers the volume, once the double-tap window has passed;
 * presses that follow it within a second lower it at once, so tapping to turn
 * the volume down never starts a dictation. Away from a text field the key is
 * never touched. The cleaned text is typed
 * into the field that was focused when the key first went down.
 *
 * Then, while the Mac asks for it (Labs › Save corrections), it follows what
 * you make of that text ([FieldSpan]) until you send it, leave the field or
 * the keyboard, or dictate again, and tells the Mac ([CorrectionOutbox]).
 */
class VolumeKeyDictation : AccessibilityService() {
    /** Tapped: one quick press, waiting to see if a second makes a double tap. */
    private enum class State { Idle, Pressed, Tapped, Recording, Toggled, Working }

    private val work = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private lateinit var dictation: LiveDictation
    private lateinit var pill: DictationPill
    private var state = State.Idle
    private var target: AccessibilityNodeInfo? = null
    /** Whether the press now down is ours: its repeats and release are too. */
    private var ownsPress = false
    /** Until when volume-down presses go straight to the system as volume changes. */
    private var volumeStreakEnds = 0L
    private lateinit var outbox: CorrectionOutbox
    private var watch: Watch? = null
    private val watchExpired = Runnable { endWatch() }
    private val holdReached = Runnable { begin(State.Recording) }
    private val tapExpired = Runnable { lowerVolume() }

    override fun onServiceConnected() {
        pill = DictationPill(this)
        outbox = CorrectionOutbox(this)
        dictation = LiveDictation(this) { text ->
            main.post { if (listening()) pill.live(text) }
        }
    }

    /** Only events about a followed dictation matter. */
    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        val followed = watch ?: return
        when (event?.eventType) {
            AccessibilityEvent.TYPE_VIEW_TEXT_CHANGED -> {
                val text = followed.field.takeIf { it.refresh() }?.let(::fieldText)
                if (text == null || !followed.span.read(text)) endWatch()
            }
            AccessibilityEvent.TYPE_VIEW_FOCUSED -> if (event.source != followed.field) endWatch()
            AccessibilityEvent.TYPE_WINDOWS_CHANGED ->
                if (windows.none { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }) endWatch()
        }
    }

    override fun onInterrupt() {
        if (state == State.Pressed || state == State.Tapped || listening()) cancel()
    }

    override fun onDestroy() {
        endWatch()
        if (::dictation.isInitialized) dictation.cancel()
        if (::pill.isInitialized) pill.remove()
        work.shutdownNow()
        super.onDestroy()
    }

    /** True consumes the key; every press this returns false for is the system's. */
    override fun onKeyEvent(event: KeyEvent): Boolean {
        if (event.keyCode != KeyEvent.KEYCODE_VOLUME_DOWN) return false
        return when (event.action) {
            KeyEvent.ACTION_DOWN -> down(event)
            KeyEvent.ACTION_UP -> up()
            else -> false
        }
    }

    private fun down(event: KeyEvent): Boolean {
        // Held keys repeat; a press already taken keeps its repeats.
        if (event.repeatCount > 0) return ownsPress
        // Presses right after a volume change are more of it, not a new double tap.
        val now = SystemClock.uptimeMillis()
        if (state == State.Idle && now < volumeStreakEnds) {
            volumeStreakEnds = now + VOLUME_STREAK_MS
            ownsPress = false
            return false
        }
        ownsPress = when (state) {
            State.Idle -> {
                target = if (PairingStore.load(this) != null) typingField() else null
                if (target != null) {
                    state = State.Pressed
                    main.postDelayed(holdReached, HOLD_MS)
                }
                target != null
            }
            State.Tapped -> {
                main.removeCallbacks(tapExpired)
                begin(State.Toggled)
                true
            }
            State.Toggled -> {
                finish()
                true
            }
            State.Pressed, State.Recording, State.Working -> false
        }
        return ownsPress
    }

    private fun up(): Boolean {
        if (!ownsPress) return false
        ownsPress = false
        when (state) {
            State.Pressed -> {
                // A quick press: a second one soon makes a double tap, else it
                // was a volume change.
                main.removeCallbacks(holdReached)
                state = State.Tapped
                main.postDelayed(tapExpired, DOUBLE_TAP_MS)
            }
            State.Recording -> finish()
            // The press that started or finished a hands-free take.
            State.Idle, State.Tapped, State.Toggled, State.Working -> Unit
        }
        return true
    }

    /** Gives a lone quick press its volume change back. */
    private fun lowerVolume() {
        state = State.Idle
        target = null
        volumeStreakEnds = SystemClock.uptimeMillis() + VOLUME_STREAK_MS
        getSystemService(AudioManager::class.java).adjustSuggestedStreamVolume(
            AudioManager.ADJUST_LOWER,
            AudioManager.USE_DEFAULT_STREAM_TYPE,
            AudioManager.FLAG_SHOW_UI,
        )
    }

    private fun listening() = state == State.Recording || state == State.Toggled

    /** The focused, editable, non-password field, only while a keyboard shows. */
    private fun typingField(): AccessibilityNodeInfo? {
        if (windows.none { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }) return null
        val field = findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return null
        return field.takeIf { it.isEditable && !it.isPassword }
    }

    /** Starts a take: [State.Recording] ends on release, [State.Toggled] on the next press. */
    private fun begin(mode: State) {
        endWatch()
        val pairing = PairingStore.load(this)
        if (pairing == null || !dictation.start(pairing)) {
            state = State.Idle
            target = null
            return pill.flash(getString(R.string.status_no_microphone))
        }
        state = mode
        tick()
        val message = if (mode == State.Toggled) R.string.status_volume_toggled else R.string.status_listening
        pill.listening(getString(message)) { dictation.level }
    }

    private fun cancel() {
        main.removeCallbacks(holdReached)
        main.removeCallbacks(tapExpired)
        dictation.cancel()
        state = State.Idle
        target = null
        pill.remove()
    }

    private fun finish() {
        val pairing = PairingStore.load(this)
        val take = dictation.stop()
        if (pairing == null || take == null) {
            state = State.Idle
            target = null
            return pill.flash(getString(R.string.status_nothing))
        }
        state = State.Working
        tick()
        pill.working(getString(R.string.status_working))
        val released = SystemClock.elapsedRealtime()
        work.execute {
            val outcome = runCatching { MacFinder.withMac(this, pairing) { take.text(MacClient(it)) } }
            val elapsed = SystemClock.elapsedRealtime() - released
            main.post { deliver(outcome, elapsed) }
        }
    }

    private fun deliver(outcome: Result<Dictated>, elapsedMs: Long) {
        state = State.Idle
        val field = target?.takeIf { it.refresh() } ?: typingField()
        target = null
        val dictated = outcome.getOrElse { error ->
            Log.i(TAG, "dictation failed after $elapsedMs ms: ${error.message}")
            return pill.flash(error.message ?: getString(R.string.status_failed))
        }
        val text = dictated.text
        if (text.isBlank()) return pill.flash(getString(R.string.status_nothing))
        val insertion = field?.let { insert(it, text) }
        if (field == null || insertion == null) {
            // The take isn't lost: it's on the clipboard to paste.
            copy(text)
            return pill.flash(getString(R.string.status_copied))
        }
        Log.i(TAG, "typed ${text.length} chars in $elapsedMs ms")
        pill.flash(getString(R.string.status_done, elapsedMs / 1000.0))
        if (dictated.entry != null && dictated.follow) follow(field, insertion, dictated.entry)
        flushCorrections()
    }

    /** Follows the dictation just typed into [field] as History [entry]. */
    private fun follow(field: AccessibilityNodeInfo, insertion: Insertion, entry: Long) {
        val start = insertion.caret - insertion.inserted.length
        watch = Watch(field, entry, FieldSpan(insertion.inserted, insertion.text, start, insertion.caret))
        main.postDelayed(watchExpired, WATCH_MS)
    }

    /** Settles the followed dictation, if any, with its last good reading. */
    private fun endWatch() {
        val ended = watch ?: return
        watch = null
        main.removeCallbacks(watchExpired)
        Log.i(TAG, "dictation followed to its end; edited=${ended.span.edited}")
        outbox.add(Correction.of(ended.entry, ended.span))
        flushCorrections()
    }

    private fun flushCorrections() {
        val pairing = PairingStore.load(this) ?: return
        work.execute { runCatching { outbox.flush(MacClient(pairing)) } }
    }

    /**
     * The field's own text: empty while it shows its placeholder. WhatsApp
     * reports its "Message" placeholder as text, with neither the hint flag
     * nor a hint, but also with no cursor (selection -1), while a field that
     * holds text, even with the cursor at its start, reports where the cursor
     * is (measured on the S24+, 2026-11-11). Text equal to the field's hint
     * counts as empty too. The no-cursor rule is kept to one short line, since
     * mistaking real text for a placeholder would type over it.
     */
    private fun fieldText(field: AccessibilityNodeInfo): String {
        val text = field.text?.toString().orEmpty()
        val placeholder = field.isShowingHintText ||
            (text.isNotEmpty() && text == field.hintText?.toString()) ||
            (field.textSelectionStart < 0 && field.textSelectionEnd < 0 &&
                text.length <= MAX_PLACEHOLDER && '\n' !in text)
        return if (placeholder) "" else text
    }

    /** Sets the field's text with the dictation in place, else pastes it; null if neither worked. */
    private fun insert(field: AccessibilityNodeInfo, text: String): Insertion? {
        val reported = field.text?.toString().orEmpty()
        val current = fieldText(field)
        if (current.isEmpty() && reported.isNotEmpty()) {
            Log.i(TAG, "placeholder: hint flag=${field.isShowingHintText}, selection=${field.textSelectionStart}")
        }
        val insertion = Insertion.splice(current, field.textSelectionStart, field.textSelectionEnd, text)
        val set = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, insertion.text)
        }
        if (field.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, set)) {
            field.performAction(
                AccessibilityNodeInfo.ACTION_SET_SELECTION,
                Bundle().apply {
                    putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, insertion.caret)
                    putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, insertion.caret)
                },
            )
            return insertion
        }
        copy(insertion.inserted)
        return insertion.takeIf { field.performAction(AccessibilityNodeInfo.ACTION_PASTE) }
    }

    /** A dictation being followed: typed into [field], saved on the Mac as [entry]. */
    private class Watch(val field: AccessibilityNodeInfo, val entry: Long, val span: FieldSpan)

    private fun copy(text: String) {
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("Tiro", text))
    }

    private fun tick() {
        getSystemService(Vibrator::class.java)?.vibrate(VibrationEffect.createPredefined(VibrationEffect.EFFECT_TICK))
    }

    private companion object {
        const val TAG = "TiroVolumeKey"

        /** A press shorter than this is a volume change. */
        const val HOLD_MS = 300L

        /** A second press this soon after a quick one's release is a double tap. */
        const val DOUBLE_TAP_MS = 300L

        /** After a volume change, presses this close to the last one change the volume too. */
        const val VOLUME_STREAK_MS = 1_000L

        /** Real messages were sent up to 13 minutes after one of their dictations (the Mac's figure). */
        const val WATCH_MS = 15 * 60 * 1_000L

        /** The longest text a field without a cursor can show as its placeholder. */
        const val MAX_PLACEHOLDER = 40
    }
}
