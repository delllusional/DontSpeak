// swift-tools-version: 6.0
//
// SmKokoro — the @_cdecl shim dylib over FluidAudio's Core ML / ANE Kokoro TTS,
// Parakeet STT, and speaker diarization. FluidAudio requires macOS 14, which is also
// the DontSpeak app floor — the Apple-native backends still degrade to the ONNX-CPU
// path when the dylib or its models are absent, but there is no older-OS fallback.
import PackageDescription

let package = Package(
    name: "SmKokoro",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "smoke", targets: ["smoke"]),
        // The C-callable shim the Rust helper dlopens (libsmkokoro.dylib).
        .library(name: "smkokoro", type: .dynamic, targets: ["smkokoro"]),
    ],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", from: "0.15.0"),
    ],
    targets: [
        .executableTarget(
            name: "smoke",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio"),
            ]
        ),
        .target(
            name: "smkokoro",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio"),
            ]
        ),
    ]
)
