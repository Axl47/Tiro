package dev.axl47.tiro

import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.net.InetSocketAddress
import java.nio.ByteBuffer
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLSocket
import org.json.JSONObject

/**
 * One dictation streamed to the Mac while it records (`POST /dictate/stream`,
 * `src/lan/stream.rs`), on one pinned TLS socket: audio frames go up as they
 * are recorded, live text and then the cleaned final come back. Connecting
 * starts at once, on its own thread; audio recorded before the socket is up
 * waits in order behind it.
 */
class LiveStream(private val pairing: Pairing, private val onLive: (String) -> Unit) {
    private val writer = Executors.newSingleThreadExecutor()
    private val final = CompletableFuture<String>()

    @Volatile private var socket: SSLSocket? = null

    init {
        writer.execute(::connect)
    }

    /** Sends [length] bytes of 16 kHz 16-bit PCM. Doesn't block. */
    fun audio(pcm: ByteArray, length: Int) {
        if (final.isDone) return
        val copy = pcm.copyOf(length)
        queue { frame('A', copy) }
    }

    /** No more audio: the Mac transcribes the rest. Doesn't block. */
    fun end() {
        queue { frame('F', ByteArray(0)) }
    }

    /**
     * The cleaned text, waiting for it after [end]; null when the stream
     * failed, so the caller can send the whole clip instead.
     */
    fun awaitText(): String? = try {
        final.get(FINAL_TIMEOUT_S, TimeUnit.SECONDS)
    } catch (_: Exception) {
        null
    } finally {
        close()
    }

    fun cancel() {
        queue { frame('C', ByteArray(0)) }
        queue(::close)
    }

    private fun connect() {
        try {
            val connecting = MacClient.sockets(pairing.fingerprint).createSocket() as SSLSocket
            connecting.connect(InetSocketAddress(pairing.host, pairing.port), CONNECT_TIMEOUT_MS)
            connecting.startHandshake()
            connecting.outputStream.apply {
                write("POST /dictate/stream HTTP/1.1\r\nAuthorization: Bearer ${pairing.token}\r\n\r\n".toByteArray())
                flush()
            }
            socket = connecting
            Thread({ read(connecting) }, "tiro-stream").start()
        } catch (error: IOException) {
            fail(error)
        }
    }

    private fun read(connection: SSLSocket) {
        try {
            val input = BufferedReader(InputStreamReader(connection.inputStream, Charsets.UTF_8))
            val status = input.readLine() ?: throw IOException("closed")
            // A Mac without the route, or one that can't stream right now
            // (`409 stream-busy`), refuses before any audio.
            if (status.split(' ').getOrNull(1) != "200") throw IOException("refused: $status")
            while (!input.readLine().isNullOrEmpty()) Unit
            while (true) {
                val event = JSONObject(input.readLine() ?: throw IOException("closed"))
                when (event.getString("type")) {
                    "partial" -> onLive(event.getString("text"))
                    "final" -> {
                        final.complete(event.getString("text"))
                        return
                    }
                    else -> throw IOException("stream error: ${event.optString("code")}")
                }
            }
        } catch (error: Exception) {
            fail(error)
        }
    }

    /** Runs [task] on the writer, in order; after [close], drops it. */
    private fun queue(task: () -> Unit) {
        try {
            writer.execute(task)
        } catch (_: java.util.concurrent.RejectedExecutionException) {
        }
    }

    private fun frame(kind: Char, payload: ByteArray) {
        val connection = socket ?: return
        try {
            val head = ByteBuffer.allocate(5).put(kind.code.toByte()).putInt(payload.size).array()
            connection.outputStream.apply {
                write(head + payload)
                flush()
            }
        } catch (error: IOException) {
            fail(error)
        }
    }

    private fun fail(error: Exception) {
        if (final.completeExceptionally(error)) android.util.Log.i(TAG, "stream failed: ${error.message}")
        close()
    }

    private fun close() {
        runCatching { socket?.close() }
        writer.shutdown()
    }

    private companion object {
        const val TAG = "TiroStream"
        const val CONNECT_TIMEOUT_MS = 3_000

        /** The tail and cleanup take about a second; this is for a busy Mac. */
        const val FINAL_TIMEOUT_S = 30L
    }
}
