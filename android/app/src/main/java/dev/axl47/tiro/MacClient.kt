package dev.axl47.tiro

import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.X509TrustManager

/** SHA-256 of a certificate's DER bytes, as `hex lan pair` prints it. */
object Fingerprint {
    fun of(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
}

/**
 * Trusts exactly the paired Mac's certificate. Its self-signed certificate
 * can't chain to a public root, and the pin is a stronger check than one would.
 */
class PinnedTrust(private val fingerprint: String) : X509TrustManager {
    override fun checkServerTrusted(chain: Array<out X509Certificate>, authType: String) {
        val leaf = chain.firstOrNull() ?: throw CertificateException("no certificate")
        if (Fingerprint.of(leaf.encoded) != fingerprint) {
            throw CertificateException("not the paired Mac")
        }
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>, authType: String) =
        throw CertificateException("this client accepts no client certificates")

    override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
}

/** Tiro's LAN routes on the paired Mac (`src/lan.rs`). Blocking: call off the main thread. */
class MacClient(private val pairing: Pairing) {
    /** A message fit to show in the keyboard's status line. */
    class Failure(message: String) : Exception(message)

    private val sockets: SSLSocketFactory by lazy {
        SSLContext.getInstance("TLS").apply {
            init(null, arrayOf(PinnedTrust(pairing.fingerprint)), null)
        }.socketFactory
    }

    /** Tiro's version on the Mac. */
    fun health(): String = JSONObject(request("GET", "/health", null, null)).getString("version")

    fun transcribe(wav: ByteArray): String =
        JSONObject(request("POST", "/transcriptions", "audio/wav", wav)).getString("transcript")

    fun cleanup(text: String): String {
        val body = JSONObject().put("text", text).toString().toByteArray()
        return JSONObject(request("POST", "/cleanup", "application/json", body)).getString("text")
    }

    private fun request(method: String, path: String, type: String?, body: ByteArray?): String {
        val connection = URL("https", pairing.host, pairing.port, path).openConnection() as HttpsURLConnection
        try {
            connection.sslSocketFactory = sockets
            // The pin identifies the Mac; its address may be a bare IP.
            connection.hostnameVerifier = javax.net.ssl.HostnameVerifier { _, _ -> true }
            connection.connectTimeout = 3_000
            connection.readTimeout = 60_000
            connection.requestMethod = method
            connection.setRequestProperty("Authorization", "Bearer ${pairing.token}")
            // The Mac answers one request per connection.
            connection.setRequestProperty("Connection", "close")
            if (body != null) {
                connection.doOutput = true
                connection.setRequestProperty("Content-Type", type)
                connection.setFixedLengthStreamingMode(body.size)
                connection.outputStream.use { it.write(body) }
            }
            val status = connection.responseCode
            val stream = if (status in 200..299) connection.inputStream else connection.errorStream
            val text = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
            if (status !in 200..299) throw Failure(describe(status, text))
            return text
        } catch (failure: Failure) {
            throw failure
        } catch (error: javax.net.ssl.SSLHandshakeException) {
            throw Failure("That isn't the paired Mac. Pair again.")
        } catch (error: java.io.IOException) {
            throw Failure("Can't reach your Mac at ${pairing.host}. Is Tiro running on the same network?")
        } finally {
            connection.disconnect()
        }
    }

    private fun describe(status: Int, body: String): String {
        val code = runCatching { JSONObject(body).getString("code") }.getOrNull()
        return when {
            status == HttpURLConnection.HTTP_UNAUTHORIZED -> "This phone isn't paired anymore. Pair again."
            code == "model-not-ready" -> "Your Mac's speech model isn't ready yet."
            code == "tiro-not-ready" -> "Tiro on your Mac is still starting."
            code == "queue-full" -> "Your Mac is busy. Try again."
            else -> "Your Mac answered ${code ?: status}."
        }
    }
}
