package dev.axl47.tiro

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.concurrent.thread

/**
 * Records one dictation from the microphone as 16-bit mono WAV.
 *
 * Debug builds play `files/fixture.wav` (16 kHz mono) instead when it exists,
 * at real-time pace, as the Mac's `HEX_AUDIO_FIXTURE` does, so the whole path,
 * streaming included, can be tested without recording anyone:
 * `adb shell run-as dev.axl47.tiro cp /data/local/tmp/clip.wav files/fixture.wav`.
 */
class Recorder(private val context: Context) {
    private var record: AudioRecord? = null
    private var reader: Thread? = null
    private val pcm = ByteArrayOutputStream()
    private var fixture: ByteArray? = null
    private var player: Thread? = null

    /** The rate of the recording in progress, or of the last one. */
    var sampleRate = PREFERRED_RATE
        private set

    /** The latest chunk's peak, 0 to 1, for a level meter. */
    @Volatile var level = 0f
        private set

    val hasPermission: Boolean
        get() = context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    /**
     * False when the microphone isn't available or allowed. [onChunk] gets the
     * 16-bit PCM as it records, on the recorder's thread; it must not block.
     */
    @SuppressLint("MissingPermission")
    fun start(onChunk: (ByteArray, Int) -> Unit = { _, _ -> }): Boolean {
        cancel()
        if (BuildConfig.DEBUG) {
            val file = File(context.filesDir, "fixture.wav")
            if (file.exists()) {
                Log.w(TAG, "fixture.wav replaces the microphone")
                play(file.readBytes(), onChunk)
                return true
            }
        }
        if (!hasPermission) return false
        sampleRate = listOf(PREFERRED_RATE, 48_000).firstOrNull {
            AudioRecord.getMinBufferSize(it, CHANNEL, ENCODING) > 0
        } ?: return false
        val buffer = maxOf(AudioRecord.getMinBufferSize(sampleRate, CHANNEL, ENCODING), sampleRate / 5 * 2)
        val recording = runCatching {
            AudioRecord(MediaRecorder.AudioSource.VOICE_RECOGNITION, sampleRate, CHANNEL, ENCODING, buffer)
        }.getOrNull()?.takeIf { it.state == AudioRecord.STATE_INITIALIZED } ?: return false
        pcm.reset()
        recording.startRecording()
        record = recording
        reader = thread(name = "tiro-recorder") {
            val chunk = ByteArray(sampleRate / 10 * 2)
            while (record === recording) {
                val read = recording.read(chunk, 0, chunk.size)
                if (read > 0 && pcm.size() < MAX_BYTES) synchronized(pcm) { pcm.write(chunk, 0, read) }
                if (read > 0) {
                    level = peak(chunk, read)
                    onChunk(chunk, read)
                }
                if (read < 0) break
            }
        }
        return true
    }

    /** The recording as WAV, or null when nothing was recorded. */
    fun stop(): ByteArray? {
        fixture?.let {
            stopPlaying()
            return it
        }
        val recording = record ?: return null
        record = null
        level = 0f
        reader?.join(500)
        reader = null
        runCatching { recording.stop() }
        recording.release()
        val audio = synchronized(pcm) { pcm.toByteArray() }
        return if (audio.isEmpty()) null else Wav.pcm16Mono(audio, sampleRate)
    }

    fun cancel() {
        stopPlaying()
        level = 0f
        val recording = record ?: return
        record = null
        reader?.join(500)
        reader = null
        runCatching { recording.stop() }
        recording.release()
    }

    /** Debug: hands out [wav]'s samples as if recording them. */
    private fun play(wav: ByteArray, onChunk: (ByteArray, Int) -> Unit) {
        fixture = wav
        sampleRate = PREFERRED_RATE
        val playing = Thread {
            val chunk = ByteArray(PREFERRED_RATE / 10 * 2)
            var at = WAV_HEADER
            while (fixture === wav && at < wav.size) {
                val length = minOf(chunk.size, wav.size - at)
                wav.copyInto(chunk, 0, at, at + length)
                level = peak(chunk, length)
                onChunk(chunk, length)
                at += length
                runCatching { Thread.sleep(100) }.onFailure { return@Thread }
            }
        }.apply { name = "tiro-fixture" }
        player = playing
        playing.start()
    }

    private fun stopPlaying() {
        fixture = null
        player?.interrupt()
        player = null
    }

    private fun peak(chunk: ByteArray, length: Int): Float {
        var peak = 0
        for (i in 0 until length - 1 step 2) {
            val sample = (chunk[i].toInt() and 0xff) or (chunk[i + 1].toInt() shl 8)
            peak = maxOf(peak, kotlin.math.abs(sample.toShort().toInt()))
        }
        return peak / 32_768f
    }

    private companion object {
        const val TAG = "TiroRecorder"
        const val PREFERRED_RATE = 16_000
        const val WAV_HEADER = 44
        const val CHANNEL = AudioFormat.CHANNEL_IN_MONO
        const val ENCODING = AudioFormat.ENCODING_PCM_16BIT
        /** Ten minutes at 16 kHz; longer belongs in more than one dictation. */
        const val MAX_BYTES = 16_000 * 2 * 600
    }
}
