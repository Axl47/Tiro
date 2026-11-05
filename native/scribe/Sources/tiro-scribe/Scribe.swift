// tiro-scribe: Vedu Scribe behind newline-delimited JSON on stdin and stdout.
//
// One process holds one model. It announces `{"event":"ready",...}` once
// loaded and warm, then answers each request in order:
//
//   → {"id":1,"text":"um so run the tests","vocab":["Zustand"]}
//   ← {"id":1,"text":"Run the tests.","elapsedMs":84,...}
//
// It is a separate process so that exiting returns every byte of the model to
// the system, a Metal fault cannot take the app down, and Hex's Cargo build
// never links MLX. Nothing here decides whether a result is good enough; the
// caller keeps the raw transcript and owns that policy.

import Foundation
import MLX
import MLXLLM
import MLXLMCommon

struct Request: Decodable {
    let id: UInt64
    let text: String
    var vocab: [String]? = nil
}

struct Response: Encodable {
    let id: UInt64
    var text: String?
    var error: String?
    var elapsedMs: Int?
    var firstTokenMs: Int?
    var inputTokens: Int?
    var outputTokens: Int?
    var chunks: Int?
    var footprintBytes: UInt64?
}

struct Ready: Encodable {
    let event = "ready"
    let loadMs: Int
    let footprintBytes: UInt64
}

/// The model's trained contract, from its card: this exact system message,
/// thinking off, greedy decoding.
enum Contract {
    static let systemPrompt = "Clean up this dictation."
    /// Inputs past this were never trained on; longer text is split.
    static let chunkTokenBudget = 400
    static let maxVocabTerms = 12

    static func outputCap(forInputTokens tokens: Int) -> Int {
        Int((Double(tokens) * 1.4).rounded(.up)) + 48
    }

    /// The `[vocab: …]` control line ahead of the transcript.
    static func userMessage(transcript: String, vocab: [String]) -> String {
        let terms = vocab
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty && !$0.contains(where: { "[];\n".contains($0) }) }
            .prefix(maxVocabTerms)
        return terms.isEmpty ? transcript : "[vocab: \(terms.joined(separator: "; "))]\n\(transcript)"
    }

    /// Transcript text must not be able to close its own chat turn.
    static func sanitized(_ text: String) -> String {
        text.replacingOccurrences(of: #"<\|[^|>]*\|>"#, with: " ", options: .regularExpression)
    }

    /// The template opens with an empty think block; drop any echo of it.
    static func stripped(_ output: String) -> String {
        output
            .replacingOccurrences(of: "<think>", with: "")
            .replacingOccurrences(of: "</think>", with: "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

let encoder = JSONEncoder()

/// The protocol's own handle on the real stdout. Fd 1 then points at stderr,
/// because dependencies `print` warnings and a stray line would corrupt the
/// protocol.
let protocolOutput: FileHandle = {
    let handle = FileHandle(fileDescriptor: dup(STDOUT_FILENO), closeOnDealloc: false)
    dup2(STDERR_FILENO, STDOUT_FILENO)
    return handle
}()

func emit(_ value: some Encodable) {
    guard var line = try? encoder.encode(value) else { return }
    line.append(0x0A)
    protocolOutput.write(line)
}

func log(_ message: String) {
    FileHandle.standardError.write(Data("tiro-scribe: \(message)\n".utf8))
}

/// What Activity Monitor calls Memory: the number that matters on a 16 GB Mac.
func footprintBytes() -> UInt64 {
    var info = task_vm_info_data_t()
    var count = mach_msg_type_number_t(
        MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
    let result = withUnsafeMutablePointer(to: &info) {
        $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
            task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
        }
    }
    return result == KERN_SUCCESS ? info.phys_footprint : 0
}

func milliseconds(since start: ContinuousClock.Instant) -> Int {
    let elapsed = ContinuousClock.now - start
    return Int(elapsed.components.seconds * 1000)
        + Int(elapsed.components.attoseconds / 1_000_000_000_000_000)
}

func clean(_ request: Request, with container: ModelContainer) async throws -> Response {
    let started = ContinuousClock.now
    let transcript = Contract.sanitized(request.text)
    let pieces = await chunks(of: transcript, budget: Contract.chunkTokenBudget) { text in
        await container.encode(text).count
    }

    var outputs: [String] = []
    var firstTokenMs: Int?
    var inputTokens = 0
    var outputTokens = 0
    for piece in pieces {
        let tokens = await container.encode(piece).count
        inputTokens += tokens
        let input = UserInput(
            chat: [
                .system(Contract.systemPrompt),
                .user(Contract.userMessage(transcript: piece, vocab: request.vocab ?? [])),
            ],
            additionalContext: ["enable_thinking": false]
        )
        let parameters = GenerateParameters(
            maxTokens: Contract.outputCap(forInputTokens: tokens),
            temperature: 0
        )
        let prepared = try await container.prepare(input: input)
        var output = ""
        for await event in try await container.generate(input: prepared, parameters: parameters) {
            switch event {
            case .chunk(let text):
                firstTokenMs = firstTokenMs ?? milliseconds(since: started)
                output += text
            case .info(let info):
                outputTokens += info.generationTokenCount
            default:
                break
            }
        }
        let cleaned = Contract.stripped(output)
        // Empty is a real answer: "um" has no written form.
        if !cleaned.isEmpty { outputs.append(cleaned) }
    }

    return Response(
        id: request.id,
        text: outputs.joined(separator: " "),
        elapsedMs: milliseconds(since: started),
        firstTokenMs: firstTokenMs,
        inputTokens: inputTokens,
        outputTokens: outputTokens,
        chunks: pieces.count,
        footprintBytes: footprintBytes()
    )
}

@main
enum Scribe {
    static func main() async {
        _ = protocolOutput

        guard let index = CommandLine.arguments.firstIndex(of: "--model"),
            index + 1 < CommandLine.arguments.count
        else {
            log("usage: tiro-scribe --model <directory>")
            exit(64)
        }
        let directory = URL(fileURLWithPath: CommandLine.arguments[index + 1], isDirectory: true)

        // A small buffer cache trades a little speed for a much lower idle
        // footprint; this process idles between dictations almost all the time.
        Memory.cacheLimit = 64 * 1024 * 1024

        let started = ContinuousClock.now
        let container: ModelContainer
        do {
            container = try await LLMModelFactory.shared.loadContainer(
                from: directory, using: ScribeTokenizerLoader())
        } catch {
            log("load failed: \(error)")
            exit(1)
        }
        // The first inference compiles Metal pipelines (seconds on a cold cache,
        // about 150 ms on a warm one). Pay for it here, not in someone's dictation.
        _ = try? await clean(Request(id: 0, text: "um run the tests"), with: container)
        emit(Ready(loadMs: milliseconds(since: started), footprintBytes: footprintBytes()))

        let decoder = JSONDecoder()
        // EOF means the app went away: exit and give the memory back.
        for await line in stdinLines() {
            guard let data = line.data(using: .utf8),
                let request = try? decoder.decode(Request.self, from: data)
            else {
                log("ignored a malformed request")
                continue
            }
            do {
                emit(try await clean(request, with: container))
            } catch {
                emit(Response(id: request.id, error: String(describing: error)))
            }
        }
    }
}

/// Lines from stdin, read off the cooperative pool so a blocking read never
/// starves inference.
func stdinLines() -> AsyncStream<String> {
    AsyncStream { continuation in
        Thread.detachNewThread {
            while let line = readLine(strippingNewline: true) {
                continuation.yield(line)
            }
            continuation.finish()
        }
    }
}
