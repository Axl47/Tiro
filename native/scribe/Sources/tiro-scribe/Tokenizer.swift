import Foundation
import MLXLMCommon
import Tokenizers

/// Loads the model's tokenizer through swift-transformers for MLX. Non-strict
/// because the config names `Qwen3_5Tokenizer`, a class swift-transformers does
/// not list yet; the tokenizer itself is ordinary byte-level BPE.
struct ScribeTokenizerLoader: MLXLMCommon.TokenizerLoader {
    func load(from directory: URL) async throws -> any MLXLMCommon.Tokenizer {
        ScribeTokenizer(base: try await AutoTokenizer.from(modelFolder: directory, strict: false))
    }
}

private struct ScribeTokenizer: MLXLMCommon.Tokenizer {
    let base: any Tokenizers.Tokenizer

    func encode(text: String, addSpecialTokens: Bool) -> [Int] {
        base.encode(text: text, addSpecialTokens: addSpecialTokens)
    }
    func decode(tokenIds: [Int], skipSpecialTokens: Bool) -> String {
        base.decode(tokens: tokenIds, skipSpecialTokens: skipSpecialTokens)
    }
    func convertTokenToId(_ token: String) -> Int? { base.convertTokenToId(token) }
    func convertIdToToken(_ id: Int) -> String? { base.convertIdToToken(id) }
    var bosToken: String? { base.bosToken }
    var eosToken: String? { base.eosToken }
    var unknownToken: String? { base.unknownToken }

    func applyChatTemplate(
        messages: [[String: any Sendable]],
        tools: [[String: any Sendable]]?,
        additionalContext: [String: any Sendable]?
    ) throws -> [Int] {
        try base.applyChatTemplate(
            messages: messages, tools: tools, additionalContext: additionalContext)
    }
}
