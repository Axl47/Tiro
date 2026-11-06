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
 * hold volume-down to talk and let go to finish, as with the Mac's shortcut. A
 * quick press still lowers the volume, and away from a text field the key is
 * never touched. The cleaned text is typed into the field that was focused
 * when the key went down.
 */
class VolumeKeyDictation : AccessibilityService() {
    private enum class State { Idle, Pressed, Recording, Working }

    private val work = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private lateinit var dictation: LiveDictation
    private lateinit var pill: DictationPill
    private var state = State.Idle
    private var target: AccessibilityNodeInfo? = null
    private val holdReached = Runnable { begin() }

    override fun onServiceConnected() {
        pill = DictationPill(this)
        dictation = LiveDictation(this) { text ->
            main.post { if (state == State.Recording) pill.live(text) }
        }
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) = Unit

    override fun onInterrupt() {
        if (state == State.Pressed || state == State.Recording) cancel()
    }

    override fun onDestroy() {
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
        if (event.repeatCount > 0) return state == State.Pressed || state == State.Recording
        if (state != State.Idle || PairingStore.load(this) == null) return false
        target = typingField() ?: return false
        state = State.Pressed
        main.postDelayed(holdReached, HOLD_MS)
        return true
    }

    private fun up(): Boolean = when (state) {
        State.Pressed -> {
            // A tap: give the volume change back.
            main.removeCallbacks(holdReached)
            state = State.Idle
            target = null
            getSystemService(AudioManager::class.java).adjustSuggestedStreamVolume(
                AudioManager.ADJUST_LOWER,
                AudioManager.USE_DEFAULT_STREAM_TYPE,
                AudioManager.FLAG_SHOW_UI,
            )
            true
        }
        State.Recording -> {
            finish()
            true
        }
        State.Idle, State.Working -> false
    }

    /** The focused, editable, non-password field, only while a keyboard shows. */
    private fun typingField(): AccessibilityNodeInfo? {
        if (windows.none { it.type == AccessibilityWindowInfo.TYPE_INPUT_METHOD }) return null
        val field = findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return null
        return field.takeIf { it.isEditable && !it.isPassword }
    }

    private fun begin() {
        val pairing = PairingStore.load(this)
        if (pairing == null || !dictation.start(pairing)) {
            state = State.Idle
            target = null
            return pill.flash(getString(R.string.status_no_microphone))
        }
        state = State.Recording
        tick()
        pill.listening(getString(R.string.status_listening)) { dictation.level }
    }

    private fun cancel() {
        main.removeCallbacks(holdReached)
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
            val outcome = runCatching { take.text(MacClient(pairing)) }
            val elapsed = SystemClock.elapsedRealtime() - released
            main.post { deliver(outcome, elapsed) }
        }
    }

    private fun deliver(outcome: Result<String>, elapsedMs: Long) {
        state = State.Idle
        val field = target?.takeIf { it.refresh() } ?: typingField()
        target = null
        val text = outcome.getOrElse { error ->
            Log.i(TAG, "dictation failed after $elapsedMs ms: ${error.message}")
            return pill.flash(error.message ?: getString(R.string.status_failed))
        }
        if (text.isBlank()) return pill.flash(getString(R.string.status_nothing))
        if (field == null || !insert(field, text)) {
            // The take isn't lost: it's on the clipboard to paste.
            copy(text)
            return pill.flash(getString(R.string.status_copied))
        }
        Log.i(TAG, "typed ${text.length} chars in $elapsedMs ms")
        pill.flash(getString(R.string.status_done, elapsedMs / 1000.0))
    }

    /** Sets the field's text with the dictation in place, else pastes it. */
    private fun insert(field: AccessibilityNodeInfo, text: String): Boolean {
        val current = if (field.isShowingHintText) "" else field.text?.toString().orEmpty()
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
            return true
        }
        copy(insertion.inserted)
        return field.performAction(AccessibilityNodeInfo.ACTION_PASTE)
    }

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
    }
}
