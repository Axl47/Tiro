package dev.axl47.tiro

/**
 * Where a dictation goes in a field's text: the field's new [text], its
 * [caret], and the [inserted] text itself (spacing included).
 */
data class Insertion(val text: String, val caret: Int, val inserted: String) {
    companion object {
        /**
         * [current] with [dictated] put over its selection ([start] to [end],
         * either order; negative means no selection, so the end), spaced from
         * the word before it as the keyboard spaces it.
         */
        fun splice(current: String, start: Int, end: Int, dictated: String): Insertion {
            val (from, to) = if (start < 0 || end < 0) {
                current.length to current.length
            } else {
                minOf(start, end).coerceIn(0, current.length) to maxOf(start, end).coerceIn(0, current.length)
            }
            val before = current.substring(0, from)
            val joined = TextJoin.joined(before, dictated)
            return Insertion(before + joined + current.substring(to), from + joined.length, joined)
        }
    }
}
