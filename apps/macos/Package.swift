// swift-tools-version: 6.2
//
// DontSpeak — macOS SwiftUI host (MenuBarExtra + sidebar Status/Tools/Logs/Libraries)
// over the Rust C ABI (`CDontSpeak/include/dontspeak.h`).
//
// Floor: macOS 14 (MenuBarExtra / Layout / SMAppService + SmKokoro Core ML stack).
// Newer APIs degrade behind availability (e.g. Liquid Glass → ultraThinMaterial).
// Link against libds_core.a from build.sh (`../../rust/target/release-ffi`); plain
// `swift build` without that staticlib fails at link.

import PackageDescription

let package = Package(
    name: "DontSpeak",
    platforms: [
        .macOS(.v14)
    ],
    targets: [
        // Header-only C target → `import CDontSpeak`; symbols come from the Rust staticlib.
        .target(
            name: "CDontSpeak",
            publicHeadersPath: "include"
        ),
        // Pure helpers only — executable force-loads the staticlib and can't host XCTest.
        .target(
            name: "DontSpeakLogic"
        ),
        .testTarget(
            name: "DontSpeakLogicTests",
            dependencies: ["DontSpeakLogic"]
        ),
        .executableTarget(
            name: "DontSpeak",
            dependencies: ["CDontSpeak", "DontSpeakLogic"],
            linkerSettings: [
                // `-force_load` retains every archive member so `-dead_strip` cannot drop
                // ds_* symbols that Swift only references across the C ABI boundary.
                .unsafeFlags([
                    "-L", "../../rust/target/release-ffi",
                    "-Xlinker", "-force_load",
                    "-Xlinker", "../../rust/target/release-ffi/libds_core.a",
                ]),
                // Transitive native deps of the staticlib (`cargo rustc -- --print
                // native-static-libs`), PLUS Carbon by hand: ds-platform links it for TIS
                // layout keycodes, but that `cargo:rustc-link-lib` never reaches this
                // separate SwiftPM list (we `-force_load` the prebuilt .a, not Cargo).
                // Missing Carbon fails only at `swift test` / `bundle.sh`, not `cargo test`.
                // Re-derive the whole list after any native-linkage change.
                // Snapshot: AudioToolbox CoreAudio IOKit ApplicationServices AppKit
                //   Foundation CoreGraphics CoreFoundation Carbon + libiconv/libobjc.
                .linkedFramework("AppKit"),
                .linkedFramework("Foundation"),
                .linkedFramework("AVFoundation"),
                .linkedFramework("AudioToolbox"),
                .linkedFramework("CoreAudio"),
                .linkedFramework("IOKit"),
                .linkedFramework("ApplicationServices"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("Carbon"),
                .linkedLibrary("iconv"),
                .linkedLibrary("objc"),
            ]
        ),
    ]
)
