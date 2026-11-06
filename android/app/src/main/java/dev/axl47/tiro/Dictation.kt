package dev.axl47.tiro

/** One dictation's trip through the Mac: Parakeet, then Tiro's cleanup stage. */
class Dictation(private val client: MacClient) {
    /** The cleaned text to type; empty when nothing was heard. */
    fun run(wav: ByteArray): String = client.dictate(wav) ?: inTwoRequests(wav)

    /** For a Mac from before `/dictate`. */
    private fun inTwoRequests(wav: ByteArray): String {
        val raw = client.transcribe(wav).trim()
        // The Mac falls back to its own deterministic cleanup when the model
        // can't run; only an unreachable Mac lands here, and the raw text is
        // still better than losing the dictation.
        return if (raw.isEmpty()) "" else runCatching { client.cleanup(raw) }.getOrDefault(raw)
    }
}
