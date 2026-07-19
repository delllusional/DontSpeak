// swift-tools-version: 6.0
//
// DsKokoro — @_cdecl shim over FluidAudio Core ML / ANE (Kokoro TTS, Parakeet STT,
// diarization). Floor macOS 14 (same as DontSpeak). Absent dylib/models → ONNX-CPU path.

import PackageDescription

let package = Package(
    name: "DsKokoro",
    platforms: [
        .macOS(.v14)
    ],
    products: [
        .executable(name: "smoke", targets: ["smoke"]),
        // C-callable dylib the Rust helper dlopens (libdskokoro.dylib).
        .library(name: "dskokoro", type: .dynamic, targets: ["dskokoro"]),
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
            name: "dskokoro",
            dependencies: [
                .product(name: "FluidAudio", package: "FluidAudio"),
            ]
        ),
        .testTarget(
            name: "dskokoroTests",
            dependencies: ["dskokoro"]
        ),
    ]
)
