package dev.axl47.tiro

import android.Manifest
import android.accessibilityservice.AccessibilityServiceInfo
import android.app.Activity
import android.app.AlertDialog
import android.content.ClipboardManager
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.view.View
import android.view.accessibility.AccessibilityManager
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.TextView
import java.util.concurrent.Executors

/** Setup: pair with a Mac, allow the microphone, turn on the volume key or the keyboard, try it. */
class MainActivity : Activity() {
    private val work = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        findViewById<Button>(R.id.pair_clipboard).setOnClickListener { pairFromClipboard() }
        findViewById<Button>(R.id.test).setOnClickListener { testConnection() }
        findViewById<Button>(R.id.unpair).setOnClickListener {
            PairingStore.clear(this)
            refresh()
        }
        findViewById<Button>(R.id.microphone).setOnClickListener {
            requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), 1)
        }
        findViewById<Button>(R.id.enable_keyboard).setOnClickListener {
            startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS))
        }
        findViewById<Button>(R.id.enable_volume_key).setOnClickListener {
            startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS))
        }
        findViewById<Button>(R.id.choose_keyboard).setOnClickListener {
            getSystemService(InputMethodManager::class.java).showInputMethodPicker()
        }
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        refresh()
    }

    override fun onDestroy() {
        work.shutdownNow()
        super.onDestroy()
    }

    private fun handle(intent: Intent?) {
        val link = intent?.data?.toString() ?: return
        intent.data = null
        offer(link)
    }

    private fun pairFromClipboard() {
        val clip = getSystemService(ClipboardManager::class.java).primaryClip
        offer(clip?.getItemAt(0)?.coerceToText(this)?.toString().orEmpty())
    }

    /**
     * Any app can open a `tiro://` link, so pairing always asks first: a
     * silent re-pair would send your speech to someone else's computer.
     */
    private fun offer(link: String) {
        val pairing = Pairing.parse(link) ?: return show(getString(R.string.pair_invalid))
        AlertDialog.Builder(this)
            .setTitle(R.string.pair_confirm_title)
            .setMessage(getString(R.string.pair_confirm_body, pairing.host, pairing.port, pairing.name))
            .setPositiveButton(R.string.pair_confirm) { _, _ ->
                PairingStore.save(this, pairing)
                refresh()
                testConnection()
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun testConnection() {
        val pairing = PairingStore.load(this) ?: return show(getString(R.string.status_unpaired))
        show(getString(R.string.testing))
        work.execute {
            val started = SystemClock.elapsedRealtime()
            val outcome = runCatching { MacClient(pairing).health() }
            val elapsed = SystemClock.elapsedRealtime() - started
            main.post {
                show(
                    outcome.fold(
                        { version -> getString(R.string.connected, version, elapsed) },
                        { error -> error.message ?: getString(R.string.status_failed) },
                    ),
                )
            }
        }
    }

    private fun refresh() {
        val pairing = PairingStore.load(this)
        findViewById<TextView>(R.id.pairing).text = pairing?.let {
            getString(R.string.paired, it.host, it.port, it.name)
        } ?: getString(R.string.unpaired_help)
        findViewById<View>(R.id.test).isEnabled = pairing != null
        findViewById<View>(R.id.unpair).isEnabled = pairing != null
        val microphone = checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
            android.content.pm.PackageManager.PERMISSION_GRANTED
        findViewById<Button>(R.id.microphone).apply {
            isEnabled = !microphone
            text = getString(if (microphone) R.string.microphone_allowed else R.string.microphone_allow)
        }
        val enabled = getSystemService(InputMethodManager::class.java).enabledInputMethodList
            .any { it.packageName == packageName }
        findViewById<Button>(R.id.enable_keyboard).text =
            getString(if (enabled) R.string.keyboard_enabled else R.string.keyboard_enable)
        val volumeKey = getSystemService(AccessibilityManager::class.java)
            .getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_ALL_MASK)
            .any { it.resolveInfo.serviceInfo.packageName == packageName }
        findViewById<Button>(R.id.enable_volume_key).text =
            getString(if (volumeKey) R.string.volume_key_enabled else R.string.volume_key_enable)
    }

    private fun show(message: String) {
        findViewById<TextView>(R.id.message).text = message
    }
}
