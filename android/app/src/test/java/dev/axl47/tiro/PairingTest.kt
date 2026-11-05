package dev.axl47.tiro

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class PairingTest {
    private val fingerprint = "e19685e74acc0e63454211606a8e63c90b70f69f3ca9e39ca60631ea6c4f58c3"
    private val token = "2de5f8022e2c668192d59e514df676c23a32e0c8840129955ee4ba8031c4d66e"

    @Test
    fun parsesWhatTheMacPrints() {
        val link = "tiro://pair?host=10.0.2.2&port=47811&token=$token&fp=$fingerprint&name=Pixel+9"
        assertEquals(Pairing("10.0.2.2", 47811, token, fingerprint, "Pixel 9"), Pairing.parse(link))
    }

    @Test
    fun refusesAnythingIncompleteOrForeign() {
        val good = "host=10.0.2.2&port=47811&token=$token&fp=$fingerprint"
        for (link in listOf(
            "https://pair?$good",
            "tiro://other?$good",
            "tiro://pair?port=47811&token=$token&fp=$fingerprint",
            "tiro://pair?host=10.0.2.2&port=99999&token=$token&fp=$fingerprint",
            "tiro://pair?host=10.0.2.2&port=47811&token=short&fp=$fingerprint",
            "tiro://pair?host=10.0.2.2&port=47811&token=$token&fp=nothex",
            "not a link",
        )) {
            assertNull(link, Pairing.parse(link))
        }
    }
}
