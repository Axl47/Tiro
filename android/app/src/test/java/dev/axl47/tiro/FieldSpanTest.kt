package dev.axl47.tiro

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FieldSpanTest {
    /** "Hi. " then the dictation "see you at five" in place. */
    private fun dictated(): FieldSpan = FieldSpan(" see you at five", "Hi. see you at five", 3, 19)

    @Test
    fun a_fix_inside_the_span_is_kept() {
        val span = dictated()
        assertTrue(span.read("Hi. see you at six"))
        assertEquals("see you at six", span.best)
        assertTrue(span.edited)
    }

    @Test
    fun text_typed_before_moves_the_span() {
        val span = dictated()
        assertTrue(span.read("Hello. see you at five"))
        assertTrue(span.read("Hello. See you at five"))
        assertEquals("See you at five", span.best)
        assertFalse(span.edited)
    }

    @Test
    fun a_new_sentence_after_stays_out_but_punctuation_joins() {
        val span = dictated()
        assertTrue(span.read("Hi. see you at five."))
        assertEquals("see you at five.", span.best)
        assertTrue(span.read("Hi. see you at five. Bring snacks"))
        assertEquals("see you at five.", span.best)
    }

    @Test
    fun sending_keeps_the_last_reading() {
        val span = dictated()
        assertTrue(span.read("Hi. see you at 5"))
        assertFalse(span.read(""))
        assertEquals("see you at 5", span.best)
    }

    @Test
    fun unrelated_text_is_not_a_label() {
        val span = dictated()
        assertTrue(span.read("Hi. completely different words here"))
        assertEquals("see you at five", span.best)
    }

    @Test
    fun a_rewritten_word_stays_in_the_span() {
        val span = dictated()
        assertTrue(span.read("Hi. we'll meet you at five"))
        assertEquals("we'll meet you at five", span.best)
    }
}
