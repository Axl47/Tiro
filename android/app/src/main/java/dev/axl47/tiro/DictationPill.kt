package dev.axl47.tiro

import android.annotation.SuppressLint
import android.content.Context
import android.os.Build
import android.graphics.PixelFormat
import android.graphics.drawable.GradientDrawable
import android.os.Handler
import android.os.Looper
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.WindowInsets
import android.view.WindowManager
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The small status pill near the top of the screen while a volume-key
 * dictation runs, like the Mac's HUD: touches pass through it, it never takes
 * focus, and it's gone between dictations.
 */
class DictationPill(private val context: Context) {
    private val windows = context.getSystemService(WindowManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private val dot = View(context).apply {
        background = GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            setColor(context.getColor(R.color.accent))
        }
    }
    private val label = TextView(context).apply {
        setTextColor(context.getColor(R.color.pill_text))
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
        // Live text keeps its newest words in view.
        isSingleLine = true
        ellipsize = android.text.TextUtils.TruncateAt.START
        maxWidth = (context.resources.displayMetrics.widthPixels * 0.8f).toInt()
        accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
    }
    private val view = LinearLayout(context).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(14), dp(9), dp(16), dp(9))
        background = GradientDrawable().apply {
            cornerRadius = dp(999).toFloat()
            setColor(context.getColor(R.color.pill_background))
        }
        addView(dot, LinearLayout.LayoutParams(dp(10), dp(10)).apply { marginEnd = dp(14) })
        addView(label)
    }
    private var shown = false
    private var level: (() -> Float)? = null
    private val hide = Runnable { remove() }
    private val meter = object : Runnable {
        override fun run() {
            val source = level ?: return
            // The dot swells with the voice, so you can see it's hearing you.
            val scale = 1f + source().coerceIn(0f, 1f) * 1.4f
            dot.scaleX = dot.scaleX * 0.5f + scale * 0.5f
            dot.scaleY = dot.scaleX
            main.postDelayed(this, 50)
        }
    }

    /** Recording: the dot follows [level]. */
    fun listening(message: String, level: () -> Float) {
        show(message)
        dot.alpha = 1f
        this.level = level
        main.post(meter)
    }

    /** Provisional text while listening; the meter keeps running. */
    fun live(text: String) {
        if (shown && level != null) label.text = text
    }

    fun working(message: String) {
        show(message)
        dot.alpha = 0.4f
    }

    /** A last word, gone after a moment. */
    fun flash(message: String) {
        show(message)
        dot.alpha = 0f
        main.postDelayed(hide, FLASH_MS)
    }

    fun remove() {
        stopMeter()
        main.removeCallbacks(hide)
        if (shown) runCatching { windows.removeView(view) }
        shown = false
    }

    private fun show(message: String) {
        stopMeter()
        main.removeCallbacks(hide)
        label.text = message
        if (shown) return
        val params = WindowManager.LayoutParams(
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.WRAP_CONTENT,
            WindowManager.LayoutParams.TYPE_ACCESSIBILITY_OVERLAY,
            WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or
                WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or
                WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
            PixelFormat.TRANSLUCENT,
        ).apply {
            gravity = Gravity.TOP or Gravity.CENTER_HORIZONTAL
            y = statusBarHeight() + dp(8)
        }
        windows.addView(view, params)
        shown = true
    }

    private fun statusBarHeight(): Int {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            return windows.currentWindowMetrics.windowInsets
                .getInsetsIgnoringVisibility(WindowInsets.Type.statusBars() or WindowInsets.Type.displayCutout()).top
        }
        @SuppressLint("DiscouragedApi", "InternalInsetResource")
        val id = context.resources.getIdentifier("status_bar_height", "dimen", "android")
        return if (id != 0) context.resources.getDimensionPixelSize(id) else dp(24)
    }

    private fun stopMeter() {
        level = null
        main.removeCallbacks(meter)
        dot.scaleX = 1f
        dot.scaleY = 1f
    }

    private fun dp(value: Int): Int = (value * context.resources.displayMetrics.density).toInt()

    private companion object {
        const val FLASH_MS = 2_000L
    }
}
