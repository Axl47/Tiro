import Foundation

/// Splits `text` so each piece fits `budget` tokens, preferring sentence ends.
/// A dictation with no punctuation at all still splits, at word boundaries.
func chunks(
    of text: String,
    budget: Int,
    tokenCount: (String) async -> Int
) async -> [String] {
    guard await tokenCount(text) > budget else { return [text] }

    var pieces: [String] = []
    var current = ""
    for sentence in sentences(of: text) {
        for part in await fitted(sentence, budget: budget, tokenCount: tokenCount) {
            let candidate = current.isEmpty ? part : "\(current) \(part)"
            let fits = await tokenCount(candidate) <= budget
            if current.isEmpty || fits {
                current = candidate
            } else {
                pieces.append(current)
                current = part
            }
        }
    }
    if !current.isEmpty { pieces.append(current) }
    return pieces
}

private func sentences(of text: String) -> [String] {
    var found: [String] = []
    text.enumerateSubstrings(in: text.startIndex..., options: .bySentences) { sentence, _, _, _ in
        if let sentence = sentence?.trimmingCharacters(in: .whitespacesAndNewlines),
            !sentence.isEmpty
        {
            found.append(sentence)
        }
    }
    return found
}

/// Halves a sentence over budget at the word nearest its middle.
private func fitted(
    _ sentence: String,
    budget: Int,
    tokenCount: (String) async -> Int
) async -> [String] {
    guard await tokenCount(sentence) > budget else { return [sentence] }
    let words = sentence.split(whereSeparator: \.isWhitespace)
    guard words.count > 1 else { return [sentence] }
    let middle = words.count / 2
    let left = await fitted(
        words[..<middle].joined(separator: " "), budget: budget, tokenCount: tokenCount)
    let right = await fitted(
        words[middle...].joined(separator: " "), budget: budget, tokenCount: tokenCount)
    return left + right
}
