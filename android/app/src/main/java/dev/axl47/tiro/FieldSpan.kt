package dev.axl47.tiro

/**
 * One dictation in a field, followed through the field's later texts until it
 * goes away (the message was sent) or the watch ends: the label for what the
 * dictation should have said. Only the span is ever kept, never the text
 * around it.
 *
 * Each reading is compared with the last one: what changed lies between their
 * common start and common end. A change before the span moves it, one inside
 * it (or overlapping it) resizes it, and one after it changes nothing. Text
 * typed right at its end joins it only when it continues the last word, as on
 * the Mac (`src/history/corrections.rs`): a new sentence the recording never
 * said stays out.
 */
class FieldSpan(private val pasted: String, text: String, start: Int, end: Int) {
    private var last = text
    private var start = start.coerceIn(0, text.length)
    private var end = end.coerceIn(this.start, text.length)

    /** The last reading that still looked like the dictation. */
    var best: String = pasted.trim()
        private set

    /** Takes a new reading; false once the span is gone. */
    fun read(text: String): Boolean {
        if (text == last) return true
        val old = last
        last = text
        val shared = minOf(old.length, text.length)
        var head = 0
        while (head < shared && old[head] == text[head]) head++
        var tail = 0
        while (tail < shared - head && old[old.length - 1 - tail] == text[text.length - 1 - tail]) tail++
        val oldEnd = old.length - tail
        val newEnd = text.length - tail
        val delta = text.length - old.length
        val inserted = text.substring(head, newEnd)
        when {
            // Wholly before the span, an insertion at its start included.
            oldEnd <= start -> {
                start += delta
                end += delta
            }
            // Wholly after it, unless it is typed at its end and continues its last word.
            head > end || (head == end && !(oldEnd == end && continues(inserted))) -> Unit
            else -> {
                end = maxOf(end + delta, newEnd).coerceAtMost(text.length)
                start = minOf(start, head)
            }
        }
        start = start.coerceIn(0, text.length)
        end = end.coerceIn(start, text.length)
        val span = text.substring(start, end).trim()
        if (span.isEmpty()) return false
        if (resembles(pasted, span)) best = span
        return true
    }

    /** Whether [best] differs from what was typed, beyond spacing and case of its first letter. */
    val edited: Boolean get() = normal(best) != normal(pasted)

    private fun continues(inserted: String) = inserted.isNotEmpty() && !inserted.first().isWhitespace()

    companion object {
        /** A reading sharing less than this with the dictation is other text. */
        private const val MIN_SIMILARITY = 0.5f
        private const val MAX_COMPARED_WORDS = 2_000

        private fun normal(text: String): List<String> {
            val words = text.split(Regex("\\s+")).filter { it.isNotEmpty() }.toMutableList()
            if (words.isNotEmpty()) words[0] = words[0].replaceFirstChar { it.lowercaseChar() }
            return words
        }

        /** Word-level Dice similarity over the longest common subsequence, as the Mac measures it. */
        fun resembles(pasted: String, reading: String): Boolean {
            val left = pasted.lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }
            val right = reading.lowercase().split(Regex("\\s+")).filter { it.isNotEmpty() }
            if (left.isEmpty() || right.isEmpty()) return false
            if (left.size + right.size > MAX_COMPARED_WORDS) return true
            val row = IntArray(right.size + 1)
            for (word in left) {
                var diagonal = 0
                for (index in right.indices) {
                    val above = row[index + 1]
                    row[index + 1] = if (word == right[index]) diagonal + 1 else maxOf(above, row[index])
                    diagonal = above
                }
            }
            return 2f * row[right.size] / (left.size + right.size) >= MIN_SIMILARITY
        }
    }
}
