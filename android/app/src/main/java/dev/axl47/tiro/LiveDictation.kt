package dev.axl47.tiro

import android.content.Context

/**
 * One dictation from start to text: records, streams to the Mac while it
 * records, and when the stream can't deliver (an older Mac, another stream
 * running, a dropped connection) sends the whole clip instead. Used by the
 * volume key and the keyboard alike. [onLive] gets provisional text for the
 * screen only, on a background thread; it is never typed.
 */
class LiveDictation(context: Context, private val onLive: (String) -> Unit) {
    private val recorder = Recorder(context)
    private var stream: LiveStream? = null

    val hasPermission: Boolean get() = recorder.hasPermission

    /** The microphone's latest peak, 0 to 1. */
    val level: Float get() = recorder.level

    /** Starts recording for [pairing]'s Mac; false when the microphone isn't available. */
    fun start(pairing: Pairing): Boolean {
        val live = LiveStream(pairing, onLive)
        if (!recorder.start { pcm, length -> live.audio(pcm, length) }) {
            live.cancel()
            return false
        }
        // Streams take 16 kHz only; a phone that can't record at it sends the clip.
        stream = if (recorder.sampleRate == STREAM_RATE) live else null.also { live.cancel() }
        return true
    }

    /** Stops recording. Null when nothing was recorded; else call [Take.text] off the main thread. */
    fun stop(): Take? {
        val wav = recorder.stop()
        val live = stream
        stream = null
        if (wav == null) {
            live?.cancel()
            return null
        }
        live?.end()
        return Take(wav, live)
    }

    fun cancel() {
        recorder.cancel()
        stream?.cancel()
        stream = null
    }

    class Take(private val wav: ByteArray, private val stream: LiveStream?) {
        /** The cleaned text, empty when nothing was heard. Blocking. */
        fun text(client: MacClient): String = stream?.awaitText() ?: Dictation(client).run(wav)
    }

    private companion object {
        const val STREAM_RATE = 16_000
    }
}
