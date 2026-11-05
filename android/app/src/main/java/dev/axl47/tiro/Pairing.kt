package dev.axl47.tiro

import android.content.Context
import java.net.URI
import java.net.URLDecoder

/**
 * The Mac this phone dictates through: what `hex lan pair` puts in its QR code.
 * [fingerprint] is the SHA-256 of the Mac's TLS certificate, which is pinned:
 * nothing else on the network can stand in for it.
 */
data class Pairing(
    val host: String,
    val port: Int,
    val token: String,
    val fingerprint: String,
    val name: String,
) {
    companion object {
        private val HEX_64 = Regex("[0-9a-f]{64}")

        /** `tiro://pair?host=…&port=…&token=…&fp=…&name=…`, or null if any part is wrong. */
        fun parse(link: String): Pairing? {
            val uri = runCatching { URI(link.trim()) }.getOrNull() ?: return null
            if (uri.scheme != "tiro" || uri.host != "pair") return null
            val query = uri.rawQuery.orEmpty().split('&').mapNotNull { part ->
                val pieces = part.split('=', limit = 2)
                if (pieces.size != 2) return@mapNotNull null
                URLDecoder.decode(pieces[0], "UTF-8") to URLDecoder.decode(pieces[1], "UTF-8")
            }.toMap()
            return Pairing(
                host = query["host"]?.takeIf { it.isNotBlank() } ?: return null,
                port = query["port"]?.toIntOrNull()?.takeIf { it in 1..65535 } ?: return null,
                token = query["token"]?.takeIf { it.length >= 32 } ?: return null,
                fingerprint = query["fp"]?.lowercase()?.takeIf { HEX_64.matches(it) } ?: return null,
                name = query["name"]?.takeIf { it.isNotBlank() } ?: "Android",
            )
        }
    }
}

/** The one pairing, in this app's private preferences. */
object PairingStore {
    private const val FILE = "pairing"

    fun load(context: Context): Pairing? {
        val prefs = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        return Pairing(
            host = prefs.getString("host", null) ?: return null,
            port = prefs.getInt("port", 0).takeIf { it > 0 } ?: return null,
            token = prefs.getString("token", null) ?: return null,
            fingerprint = prefs.getString("fingerprint", null) ?: return null,
            name = prefs.getString("name", null) ?: "Android",
        )
    }

    fun save(context: Context, pairing: Pairing) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit()
            .putString("host", pairing.host)
            .putInt("port", pairing.port)
            .putString("token", pairing.token)
            .putString("fingerprint", pairing.fingerprint)
            .putString("name", pairing.name)
            .apply()
    }

    fun clear(context: Context) {
        context.getSharedPreferences(FILE, Context.MODE_PRIVATE).edit().clear().apply()
    }
}
