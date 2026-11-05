// swift-tools-version: 6.0
import PackageDescription

// Exact pins: the Qwen3.5 model type and its tokenizer class only load on
// these versions or newer, and a loose range would change inference behavior
// under a benchmark without anyone asking.
let package = Package(
    name: "tiro-scribe",
    platforms: [.macOS(.v15)],
    dependencies: [
        .package(url: "https://github.com/ml-explore/mlx-swift.git", exact: "0.31.6"),
        .package(url: "https://github.com/ml-explore/mlx-swift-lm.git", exact: "3.31.4"),
        .package(url: "https://github.com/huggingface/swift-transformers.git", exact: "1.3.4"),
    ],
    targets: [
        .executableTarget(
            name: "tiro-scribe",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXLLM", package: "mlx-swift-lm"),
                .product(name: "MLXLMCommon", package: "mlx-swift-lm"),
                .product(name: "Tokenizers", package: "swift-transformers"),
            ],
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
        .testTarget(name: "tiro-scribe-tests", dependencies: ["tiro-scribe"]),
    ]
)
