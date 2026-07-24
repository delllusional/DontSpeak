// swift-tools-version: 6.2
//
// Three peer @_cdecl shim families, one dynamic dylib each, so a host can carry any subset:
//   dontspeak_sys   -- Apple System STT; no package dependencies (builds on every macOS arch)
//   dontspeak_mlx   -- MLX Audio TTS, Parakeet STT, Sortformer diarization (Apple Silicon)
//   dontspeak_fluid -- FluidAudio Core ML / ANE TTS, ASR, diarization (Apple Silicon)
// Floor macOS 14 (same as DontSpeak). Absent dylib/models -> ONNX-CPU path.

import PackageDescription

let package = Package(
    name: "DontSpeakShims",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "smoke", targets: ["smoke"]),
        // C-callable dylibs the Rust helper dlopens (libdontspeak_<family>.dylib).
        .library(name: "dontspeak_sys", type: .dynamic, targets: ["DontSpeakSys"]),
        .library(name: "dontspeak_mlx", type: .dynamic, targets: ["DontSpeakMLX"]),
        .library(name: "dontspeak_fluid", type: .dynamic, targets: ["DontSpeakFluid"]),
    ],
    dependencies: [
        .package(url: "https://github.com/Blaizzy/mlx-audio-swift.git", exact: "0.1.3"),
        .package(url: "https://github.com/ml-explore/mlx-swift.git", exact: "0.31.3"),
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
        // Dependency-free on purpose: that is what lets bundle-lib.sh ship it from a bare
        // `xcrun swiftc` call (no SwiftPM resolution, no network) on Intel as well as arm64.
        .target(name: "DontSpeakSys", dependencies: []),
        // Must NOT list FluidAudio -- keeping the two Core ML / MLX runtimes in separate
        // dylibs is the point of the split (verify_shim_isolation asserts it on the artifact).
        .target(
            name: "DontSpeakMLX",
            dependencies: [
                .product(name: "MLX", package: "mlx-swift"),
                .product(name: "MLXNN", package: "mlx-swift"),
                .product(name: "MLXAudioCore", package: "mlx-audio-swift"),
                .product(name: "MLXAudioTTS", package: "mlx-audio-swift"),
                .product(name: "MLXAudioSTT", package: "mlx-audio-swift"),
                .product(name: "MLXAudioVAD", package: "mlx-audio-swift"),
            ]
        ),
        .target(
            name: "DontSpeakFluid",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio")
            ]
        ),
        .testTarget(
            name: "DontSpeakShimsTests",
            dependencies: [
                "DontSpeakSys",
                "DontSpeakMLX",
                "DontSpeakFluid",
                .product(name: "MLX", package: "mlx-swift"),
            ]
        ),
    ]
)
