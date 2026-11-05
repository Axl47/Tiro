package dev.axl47.tiro

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import java.nio.ByteBuffer
import java.nio.ByteOrder

class WavTest {
    @Test
    fun writesAPlayableHeader() {
        val pcm = byteArrayOf(1, 2, 3, 4)
        val wav = Wav.pcm16Mono(pcm, 16_000)
        val header = ByteBuffer.wrap(wav).order(ByteOrder.LITTLE_ENDIAN)
        assertEquals("RIFF", String(wav, 0, 4))
        assertEquals(40, header.getInt(4))
        assertEquals("WAVE", String(wav, 8, 4))
        assertEquals(1, header.getShort(22).toInt())
        assertEquals(16_000, header.getInt(24))
        assertEquals(32_000, header.getInt(28))
        assertEquals(16, header.getShort(34).toInt())
        assertEquals(4, header.getInt(40))
        assertArrayEquals(pcm, wav.copyOfRange(44, 48))
    }
}

class TextJoinTest {
    @Test
    fun spacesOnlyWhereWordsWouldTouch() {
        assertEquals("Run the tests.", TextJoin.joined(null, "Run the tests."))
        assertEquals("Run the tests.", TextJoin.joined("", "Run the tests."))
        assertEquals(" Run the tests.", TextJoin.joined("Okay.", "Run the tests."))
        assertEquals("Run the tests.", TextJoin.joined("Okay. ", "Run the tests."))
        assertEquals(", then deploy.", TextJoin.joined("Build it", ", then deploy."))
        assertEquals("src/main", TextJoin.joined("(", "src/main"))
    }
}

class FingerprintTest {
    @Test
    fun matchesTheMacsFormat() {
        // `printf hello | shasum -a 256`
        assertEquals(
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
            Fingerprint.of("hello".toByteArray()),
        )
    }
}
