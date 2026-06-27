//! Bakes a BUILD_ID into the engine so the SwiftUI app can detect when the running
//! engine has drifted from the app it was built alongside (see apps/macos/bundle.sh,
//! which sets DONTSPEAK_BUILD_ID once from git and builds BOTH with it).
//!
//! `rerun-if-env-changed` makes cargo recompile when the id changes even if no
//! source did, so the embedded value is never stale. Absent the env (a plain
//! `cargo build`), the id is "dev" — a harmless, always-mismatching sentinel.

fn main() {
    println!("cargo:rerun-if-env-changed=DONTSPEAK_BUILD_ID");
    let id = std::env::var("DONTSPEAK_BUILD_ID").unwrap_or_else(|_| "dev".to_string());
    println!("cargo:rustc-env=DONTSPEAK_BUILD_ID={id}");
}
