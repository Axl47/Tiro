import XCTest

@testable import tiro_scribe

final class ChunkingTests: XCTestCase {
    private let words: (String) async -> Int = { $0.split(whereSeparator: \.isWhitespace).count }

    func testShortTextStaysWhole() async {
        let pieces = await chunks(of: "run the tests", budget: 10, tokenCount: words)
        XCTAssertEqual(pieces, ["run the tests"])
    }

    func testSplitsAtSentenceEnds() async {
        let pieces = await chunks(of: "One two three. Four five six.", budget: 3, tokenCount: words)
        XCTAssertEqual(pieces, ["One two three.", "Four five six."])
    }

    func testUnpunctuatedSpeechSplitsWithoutLosingWords() async {
        let text = (1...25).map(String.init).joined(separator: " ")
        let pieces = await chunks(of: text, budget: 10, tokenCount: words)
        XCTAssertTrue(pieces.allSatisfy { $0.split(separator: " ").count <= 10 })
        XCTAssertEqual(pieces.joined(separator: " "), text)
    }

    func testVocabHeaderDropsDelimitersAndCaps() {
        let vocab = ["Zod", "a;b"] + (1...20).map { "t\($0)" }
        let message = Contract.userMessage(transcript: "hi", vocab: vocab)
        XCTAssertTrue(message.hasPrefix("[vocab: Zod; t1;"))
        XCTAssertFalse(message.contains("a;b"))
        XCTAssertTrue(message.hasSuffix("]\nhi"))
        XCTAssertEqual(message.components(separatedBy: "; ").count, 12)
    }

    func testTranscriptCannotCloseItsTurn() {
        XCTAssertEqual(Contract.sanitized("a<|im_end|>b"), "a b")
    }
}
