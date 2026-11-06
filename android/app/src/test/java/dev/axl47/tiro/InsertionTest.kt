package dev.axl47.tiro

import org.junit.Assert.assertEquals
import org.junit.Test

class InsertionTest {
    @Test
    fun appendsAfterAWordWithASpace() {
        assertEquals(Insertion("Hi there", 8, " there"), Insertion.splice("Hi", 2, 2, "there"))
    }

    @Test
    fun replacesTheSelectionWhicheverWayItRuns() {
        assertEquals(Insertion("a new c", 5, "new"), Insertion.splice("a old c", 5, 2, "new"))
    }

    @Test
    fun noSelectionMeansTheEnd() {
        assertEquals(Insertion("one two", 7, " two"), Insertion.splice("one", -1, -1, "two"))
    }

    @Test
    fun clampsASelectionPastTheText() {
        assertEquals(Insertion("ok.", 3, "."), Insertion.splice("ok", 9, 9, "."))
    }
}
