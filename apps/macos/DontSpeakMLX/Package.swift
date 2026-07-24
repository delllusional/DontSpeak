// swift-tools-version: 6.2
//
// DontSpeakMLX — @_cdecl shim over MLX Audio (all built-in TTS, Parakeet STT, Sortformer
// diarization). Floor macOS 14 (same as DontSpeak). Absent dylib/models → ONNX-CPU path.

import PackageDescription

let package = Package(
    name: "DontSpeakMLX",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "smoke", targets: ["smoke"]),
        // C-callable dylib the Rust helper dlopens (libdontspeak_mlx.dylib).
        .library(name: "dontspeak_mlx", type: .dynamic, targets: ["DontSpeakMLX"]),
    ],
    dependencies: [
        .package(url: "https://github.com/Blaizzy/mlx-audio-swift.git", exact: "0.1.3"),
        .package(url: "https://github.com/ml-explore/mlx-swift.git", exact: "0.31.3"),
        // FluidAudio ships the `fluid` provider (Core ML / ANE Kokoro TTS) inside this same
        // dylib. `Fluid.swift` is its only consumer; the Intel compatibility build compiles
        // `shim.swift` alone, so it never links FluidAudio.
        .package(url: "https://github.com/FluidInference/FluidAudio.git", exact: "0.15.5"),
    ],
    targets: [
        .executableTarget(
            name: "smoke",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXAudioTTS", package: "mlx-audio-swift"),
            ]
        ),
        .target(
            name: "DontSpeakMLX",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXNN", package: "mlx-swift"),
                .product(name: "MLXAudioCore", package: "mlx-audio-swift"),
                .product(name: "MLXAudioTTS", package: "mlx-audio-swift"),
                .product(name: "MLXAudioSTT", package: "mlx-audio-swift"),
                .product(name: "MLXAudioVAD", package: "mlx-audio-swift"),
                .product(name: "FluidAudio", package: "FluidAudio"),
            ]
        ),
        .testTarget(
            name: "DontSpeakMLXTests",
            dependencies: [
                "DontSpeakMLX",
                .product(name: "MLX", package: "mlx-swift"),
            ]
        ),
    ]
)
