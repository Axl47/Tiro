package dev.axl47.tiro

/**
 * A dictation's cleaned [text] (empty when nothing was heard), the Mac's
 * History [entry] for it, and whether the Mac wants to hear what became of it
 * ([follow], Labs › Save corrections).
 */
data class Dictated(val text: String, val entry: Long? = null, val follow: Boolean = false) {
    companion object {
        /** From the Mac's answer to `/dictate` or a stream's final event. */
        fun from(answer: org.json.JSONObject) = Dictated(
            answer.getString("text"),
            if (answer.has("entry")) answer.getLong("entry") else null,
            answer.optBoolean("corrections"),
        )
    }
}

/** One dictation's trip through the Mac: Parakeet, then Tiro's cleanup stage. */
class Dictation(private val client: MacClient) {
    fun run(wav: ByteArray): Dictated = client.dictate(wav) ?: Dictated(inTwoRequests(wav))

    /** For a Mac from before `/dictate`. */
    private fun inTwoRequests(wav: ByteArray): String {
        val raw = client.transcribe(wav).trim()
        // The Mac falls back to its own deterministic cleanup when the model
        // can't run; only an unreachable Mac lands here, and the raw text is
        // still better than losing the dictation.
        return if (raw.isEmpty()) "" else runCatching { client.cleanup(raw) }.getOrDefault(raw)
    }
}
