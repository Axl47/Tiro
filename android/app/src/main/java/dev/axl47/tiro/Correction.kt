package dev.axl47.tiro

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/** What became of the Mac's History [entry]: `edited`, `unchanged`, or `unreadable` (no [text]). */
data class Correction(val entry: Long, val status: String, val text: String?) {
    fun json(): JSONObject = JSONObject().put("entry", entry).put("status", status).apply {
        if (text != null) put("text", text)
    }

    companion object {
        fun of(entry: Long, span: FieldSpan) =
            Correction(entry, if (span.edited) "edited" else "unchanged", span.best)

        fun from(json: JSONObject) = Correction(
            json.getLong("entry"),
            json.getString("status"),
            if (json.has("text")) json.getString("text") else null,
        )
    }
}

/**
 * Corrections waiting for the Mac, kept across restarts so one settled while
 * the Mac was away still reaches it. Sent after each correction and each
 * dictation; the newest [MAX] are kept.
 */
class CorrectionOutbox(context: Context) {
    private val store = context.getSharedPreferences("corrections", Context.MODE_PRIVATE)

    @Synchronized
    fun add(correction: Correction) {
        val waiting = load().filter { it.entry != correction.entry } + correction
        save(waiting.takeLast(MAX))
    }

    /** Sends what waits, oldest first; stops at the first Mac it can't reach. Blocking. */
    fun flush(client: MacClient) {
        while (true) {
            val next = synchronized(this) { load().firstOrNull() } ?: return
            try {
                client.correct(next)
            } catch (failure: MacClient.Failure) {
                // Unreachable or not ready: try again later. Anything else is final.
                if (failure.code == MacClient.UNREACHABLE || failure.code == "tiro-not-ready") return
            }
            synchronized(this) { save(load().filter { it != next }) }
        }
    }

    private fun load(): List<Correction> = runCatching {
        val array = JSONArray(store.getString(KEY, "[]"))
        (0 until array.length()).map { Correction.from(array.getJSONObject(it)) }
    }.getOrDefault(emptyList())

    private fun save(waiting: List<Correction>) {
        store.edit().putString(KEY, JSONArray(waiting.map(Correction::json)).toString()).apply()
    }

    private companion object {
        const val KEY = "waiting"
        const val MAX = 50
    }
}
