package dev.axl47.tiro

import android.os.SystemClock

/** One dictation's trip through the Mac: Parakeet, then Tiro's cleanup stage. */
class Dictation(private val client: MacClient) {
    data class Result(val text: String, val raw: String, val transcribeMs: Long, val cleanupMs: Long)

    fun run(wav: ByteArray): Result {
        val started = SystemClock.elapsedRealtime()
        val raw = client.transcribe(wav).trim()
        val transcribed = SystemClock.elapsedRealtime()
        // The Mac falls back to its own deterministic cleanup when the model
        // can't run; only an unreachable Mac lands here, and the raw text is
        // still better than losing the dictation.
        val text = if (raw.isEmpty()) "" else runCatching { client.cleanup(raw) }.getOrDefault(raw)
        return Result(text, raw, transcribed - started, SystemClock.elapsedRealtime() - transcribed)
    }
}
