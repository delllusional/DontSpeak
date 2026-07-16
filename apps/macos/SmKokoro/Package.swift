// swift-tools-version: 6.0
//
// SmKokoro — @_cdecl shim over FluidAudio Core ML / ANE (Kokoro TTS, Parakeet STT,
// diarization). Floor macOS 14 (same as DontSpeak). Absent dylib/models → ONNX-CPU path.

import PackageDescription

let package = Package(
    name: "SmKokoro",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "smoke", targets: ["smoke"]),
        // C-callable dylib the Rust helper dlopens (libsmkokoro.dylib).
        .library(name: "smkokoro", type: .dynamic, targets: ["smkokoro"]),
    ],
    dependencies: [
        .package(url: "https://github.com/FluidInference/FluidAudio.git", from: "0.15.5"),
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
        .testTarget(
            name: "smkokoroTests",
            dependencies: ["smkokoro"]
        ),
    ]
)
