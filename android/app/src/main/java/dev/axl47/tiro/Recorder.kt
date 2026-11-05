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
 * Debug builds play `files/fixture.wav` instead when it exists, as the Mac's
 * `HEX_AUDIO_FIXTURE` does, so the whole path can be tested on an emulator
 * without recording anyone:
 * `adb shell run-as dev.axl47.tiro cp /data/local/tmp/clip.wav files/fixture.wav`.
 */
class Recorder(private val context: Context) {
    private var record: AudioRecord? = null
    private var reader: Thread? = null
    private val pcm = ByteArrayOutputStream()
    private var sampleRate = PREFERRED_RATE
    private var fixture: ByteArray? = null

    val hasPermission: Boolean
        get() = context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) ==
            PackageManager.PERMISSION_GRANTED

    /** False when the microphone isn't available or allowed. */
    @SuppressLint("MissingPermission")
    fun start(): Boolean {
        cancel()
        if (BuildConfig.DEBUG) {
            val file = File(context.filesDir, "fixture.wav")
            if (file.exists()) {
                Log.w(TAG, "fixture.wav replaces the microphone")
                fixture = file.readBytes()
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
                if (read < 0) break
            }
        }
        return true
    }

    /** The recording as WAV, or null when nothing was recorded. */
    fun stop(): ByteArray? {
        fixture?.let { fixture = null; return it }
        val recording = record ?: return null
        record = null
        reader?.join(500)
        reader = null
        runCatching { recording.stop() }
        recording.release()
        val audio = synchronized(pcm) { pcm.toByteArray() }
        return if (audio.isEmpty()) null else Wav.pcm16Mono(audio, sampleRate)
    }

    fun cancel() {
        fixture = null
        val recording = record ?: return
        record = null
        reader?.join(500)
        reader = null
        runCatching { recording.stop() }
        recording.release()
    }

    private companion object {
        const val TAG = "TiroRecorder"
        const val PREFERRED_RATE = 16_000
        const val CHANNEL = AudioFormat.CHANNEL_IN_MONO
        const val ENCODING = AudioFormat.ENCODING_PCM_16BIT
        /** Ten minutes at 16 kHz; longer belongs in more than one dictation. */
        const val MAX_BYTES = 16_000 * 2 * 600
    }
}
