package dev.axl47.tiro

import android.inputmethodservice.InputMethodService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.View
import android.view.WindowInsets
import android.view.inputmethod.EditorInfo
import android.widget.TextView
import java.util.concurrent.Executors

/**
 * The keyboard: hold the mic and speak, or tap it to start and tap again to
 * finish, as on the Mac. The audio goes to the paired Mac, and the cleaned text
 * is typed into whatever field is focused.
 */
class TiroKeyboard : InputMethodService() {
    private enum class State { Idle, Holding, Locked, Working }

    private val work = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())
    private lateinit var recorder: Recorder
    private var state = State.Idle
    private var pressedAt = 0L
    private var mic: TextView? = null
    private var status: TextView? = null

    override fun onCreate() {
        super.onCreate()
        recorder = Recorder(this)
    }

    override fun onCreateInputView(): View {
        val view = layoutInflater.inflate(R.layout.keyboard, null)
        mic = view.findViewById<TextView>(R.id.mic).apply {
            // Screen readers click rather than touch: a click starts or finishes.
            setOnClickListener {
                when (state) {
                    State.Idle -> begin().also { if (state == State.Holding) state = State.Locked }
                    State.Locked, State.Holding -> finish()
                    State.Working -> Unit
                }
            }
            // Touch is hold-to-talk with tap-to-lock, which a click can't express.
            @Suppress("ClickableViewAccessibility")
            setOnTouchListener { _, event ->
                when (event.actionMasked) {
                    MotionEvent.ACTION_DOWN -> pressed()
                    MotionEvent.ACTION_UP -> released()
                    MotionEvent.ACTION_CANCEL -> if (state == State.Holding) cancel()
                }
                true
            }
        }
        status = view.findViewById(R.id.status)
        view.findViewById<View>(R.id.switch_keyboard).setOnClickListener {
            if (!switchToPreviousInputMethod()) {
                getSystemService(android.view.inputmethod.InputMethodManager::class.java)
                    .showInputMethodPicker()
            }
        }
        view.findViewById<View>(R.id.backspace).setOnClickListener {
            sendDownUpKeyEvents(KeyEvent.KEYCODE_DEL)
        }
        view.findViewById<View>(R.id.enter).setOnClickListener { enter() }
        // Android 15+ draws the keyboard under the navigation bar, whose back and
        // keyboard-switch buttons would otherwise sit on top of these keys.
        val bottom = view.paddingBottom
        view.setOnApplyWindowInsetsListener { padded, insets ->
            val navigation = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                insets.getInsets(WindowInsets.Type.navigationBars()).bottom
            } else {
                @Suppress("DEPRECATION")
                insets.systemWindowInsetBottom
            }
            padded.setPadding(padded.paddingLeft, padded.paddingTop, padded.paddingRight, bottom + navigation)
            insets
        }
        render(idleStatus())
        return view
    }

    override fun onStartInputView(info: EditorInfo?, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        if (state == State.Idle) render(idleStatus())
    }

    override fun onFinishInputView(finishingInput: Boolean) {
        if (state == State.Holding || state == State.Locked) cancel()
        super.onFinishInputView(finishingInput)
    }

    override fun onDestroy() {
        recorder.cancel()
        work.shutdownNow()
        super.onDestroy()
    }

    private fun idleStatus(): String = when {
        PairingStore.load(this) == null -> getString(R.string.status_unpaired)
        !recorder.hasPermission && !BuildConfig.DEBUG -> getString(R.string.status_no_microphone)
        else -> getString(R.string.status_idle)
    }

    private fun pressed() {
        when (state) {
            State.Idle -> begin()
            State.Locked -> finish()
            State.Holding, State.Working -> Unit
        }
    }

    private fun released() {
        if (state != State.Holding) return
        if (SystemClock.uptimeMillis() - pressedAt >= HOLD_MS) {
            finish()
        } else {
            state = State.Locked
            render(getString(R.string.status_locked))
        }
    }

    private fun begin() {
        if (PairingStore.load(this) == null) return render(getString(R.string.status_unpaired))
        if (!recorder.start()) return render(getString(R.string.status_no_microphone))
        pressedAt = SystemClock.uptimeMillis()
        state = State.Holding
        render(getString(R.string.status_listening))
    }

    private fun cancel() {
        recorder.cancel()
        state = State.Idle
        render(idleStatus())
    }

    private fun finish() {
        val pairing = PairingStore.load(this)
        val wav = recorder.stop()
        if (pairing == null || wav == null) {
            state = State.Idle
            return render(getString(R.string.status_nothing))
        }
        state = State.Working
        render(getString(R.string.status_working))
        val released = SystemClock.elapsedRealtime()
        work.execute {
            val outcome = runCatching { Dictation(MacClient(pairing)).run(wav) }
            val elapsed = SystemClock.elapsedRealtime() - released
            main.post { deliver(outcome, elapsed) }
        }
    }

    private fun deliver(outcome: Result<Dictation.Result>, elapsedMs: Long) {
        state = State.Idle
        val result = outcome.getOrElse { error ->
            return render(error.message ?: getString(R.string.status_failed))
        }
        if (result.text.isBlank()) return render(getString(R.string.status_nothing))
        val input = currentInputConnection ?: return render(getString(R.string.status_no_field))
        input.commitText(TextJoin.joined(input.getTextBeforeCursor(1, 0), result.text), 1)
        render(getString(R.string.status_done, elapsedMs / 1000.0))
    }

    private fun enter() {
        val info = currentInputEditorInfo
        val action = info?.imeOptions?.and(EditorInfo.IME_MASK_ACTION) ?: EditorInfo.IME_ACTION_NONE
        val multiLine = (info?.inputType ?: 0) and android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE != 0
        if (!multiLine && action != EditorInfo.IME_ACTION_NONE && action != EditorInfo.IME_ACTION_UNSPECIFIED) {
            currentInputConnection?.performEditorAction(action)
        } else {
            sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER)
        }
    }

    private fun render(message: String) {
        status?.text = message
        mic?.isActivated = state == State.Holding || state == State.Locked
        mic?.isEnabled = state != State.Working
        mic?.text = getString(
            when (state) {
                State.Holding, State.Locked -> R.string.mic_listening
                State.Working -> R.string.mic_working
                State.Idle -> R.string.mic_idle
            },
        )
    }

    private companion object {
        /** A press shorter than this is a tap: it locks the recording on. */
        const val HOLD_MS = 350L
    }
}
