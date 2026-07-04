// Link the IOKit framework on macOS for the thin IOHIDGetModifierLockState FFI.
// Generic input libraries can't read modifier *lock* (Caps-Lock LED) state — that
// requires IOKit's IOHIDSystem param connection, so we link the framework directly.
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" {
        println!("cargo:rustc-link-lib=framework=IOKit");
        // ApplicationServices pulls in AXIsProcessTrusted + the AXUIElement
        // focused-element probe (paste-target detection), declared in our FFI shim.
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        // CoreFoundation: CFString build/release for the AX attribute names.
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
}
