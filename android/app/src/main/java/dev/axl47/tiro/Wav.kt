package dev.axl47.tiro

import java.nio.ByteBuffer
import java.nio.ByteOrder

/** 16-bit mono PCM in a WAV container, which the Mac's `/transcriptions` takes. */
object Wav {
    fun pcm16Mono(pcm: ByteArray, sampleRate: Int): ByteArray =
        ByteBuffer.allocate(44 + pcm.size).order(ByteOrder.LITTLE_ENDIAN).apply {
            put("RIFF".toByteArray()); putInt(36 + pcm.size); put("WAVE".toByteArray())
            put("fmt ".toByteArray()); putInt(16); putShort(1); putShort(1)
            putInt(sampleRate); putInt(sampleRate * 2); putShort(2); putShort(16)
            put("data".toByteArray()); putInt(pcm.size); put(pcm)
        }.array()
}

/** Where dictated text meets what is already in the field. */
object TextJoin {
    /** [text] with a leading space when it would otherwise run into a word. */
    fun joined(before: CharSequence?, text: String): String {
        val last = before?.lastOrNull() ?: return text
        val first = text.firstOrNull() ?: return text
        return if (last.isWhitespace() || first in ".,!?;:)]}'\"" || last in "([{/") text else " $text"
    }
}
