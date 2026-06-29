// swift-tools-version: 6.2
//
// DontSpeak — native macOS SwiftUI app (MenuBarExtra tray + a sidebar window with the
// Status / Tools / Logs / Libraries screens) bound to the Rust core over the C ABI in
// CDontSpeak/include/dontspeak.h.
//
// Deployment target is macOS 14 (Sonoma) — the floor set by the app's core shell
// (MenuBarExtra, the Layout protocol, SMAppService) plus the SmKokoro apple-native audio
// stack (FluidAudio's Core ML / ANE Kokoro + Parakeet + diarization, all macOS 14+).
// Newer adoptions degrade behind availability checks: Liquid Glass `.glassEffect` and the
// top window-resize anchor (macOS 26) fall back to `.ultraThinMaterial` / AppKit's default
// bottom anchor — so the app still builds against the latest SDK and runs back to Sonoma
// without serious degradation. There is no longer any macOS 13 legacy path.
//
// The DontSpeak target links the Rust staticlib libds_core.a built by
// build.sh into ../../rust/target/release-ffi. Running `swift build` directly
// (without build.sh first) fails at link with "library 'ds_core' not
// found" — use build.sh, which builds the staticlib first.

import PackageDescription

let package = Package(
    name: "DontSpeak",
    platforms: [
        .macOS(.v14)
    ],
    targets: [
        // The C target wrapping the committed header. A SwiftPM auto-generated
        // module map (from include/) exposes `import CDontSpeak`. The shim.c keeps
        // it a buildable C target; the real symbols come from the linked Rust
        // staticlib (linkerSettings on the DontSpeak target below).
        .target(
            name: "CDontSpeak",
            publicHeadersPath: "include"
        ),
        // Pure, dependency-free app logic (no FFI, no frameworks) split out so it is
        // unit-testable on its own — the executable target can't host XCTest because it
        // force-loads the Rust staticlib. Keep only genuinely pure helpers here.
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
                // The Rust staticlib (built by build.sh, symbols un-stripped via
                // the release-ffi profile). `-force_load` pulls EVERY member
                // object in so the linker's `-dead_strip` (default for SwiftPM
                // executables) cannot drop the FFI symbols that Swift references
                // only across the C-ABI boundary — without it the archive
                // members are not retained and the link leaves the ds_*
                // symbols undefined.
                .unsafeFlags([
                    "-L", "../../rust/target/release-ffi",
                    "-Xlinker", "-force_load",
                    "-Xlinker", "../../rust/target/release-ffi/libds_core.a",
                ]),
                // System frameworks the staticlib transitively needs — derived
                // from `cargo rustc -- --print native-static-libs`:
                //   AudioToolbox CoreAudio IOKit ApplicationServices AppKit
                //   Foundation CoreGraphics CoreFoundation + libiconv/libobjc.
                .linkedFramework("AppKit"),
                .linkedFramework("Foundation"),
                .linkedFramework("AVFoundation"),
                .linkedFramework("AudioToolbox"),
                .linkedFramework("CoreAudio"),
                .linkedFramework("IOKit"),
                .linkedFramework("ApplicationServices"),
                .linkedFramework("CoreGraphics"),
                .linkedFramework("CoreFoundation"),
                .linkedLibrary("iconv"),
                .linkedLibrary("objc"),
            ]
        ),
    ]
)
