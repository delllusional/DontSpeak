//! When the `cbindgen` feature is on, regenerate committed `dontspeak.h` from the
//! `extern "C"` surface into `apps/macos/Sources/CDontSpeak/include/dontspeak.h`.
//!
//! Header is committed: default builds (feature off / offline) do nothing. Only
//! `--features cbindgen` regenerates — Swift does not require cbindgen installed.

fn main() {
    println!("cargo:rerun-if-changed=src/ffi.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");

    #[cfg(feature = "cbindgen")]
    regenerate_header();
}

#[cfg(feature = "cbindgen")]
fn regenerate_header() {
    use std::path::Path;

    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let config = match cbindgen::Config::from_file(Path::new(&crate_dir).join("cbindgen.toml")) {
        Ok(c) => c,
        Err(e) => {
            // Keep committed header; do not fall back to Config::default() (wrong
            // usize/uintptr_t mapping) while still claiming "regenerated".
            println!(
                "cargo:warning=cbindgen.toml failed to parse ({e}); keeping committed dontspeak.h"
            );
            return;
        }
    };

    let generated = match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
    {
        Ok(b) => b,
        Err(e) => {
            println!("cargo:warning=cbindgen generate failed ({e}); keeping committed dontspeak.h");
            return;
        }
    };

    // CARGO_MANIFEST_DIR = …/rust/crates/ds-core → repo root (parent ×3).
    let manifest = Path::new(&crate_dir);
    let repo_root = manifest
        .parent() // crates
        .and_then(|p| p.parent()) // rust
        .and_then(|p| p.parent()); // repo root

    if let Some(root) = repo_root {
        let macos_header = root.join("apps/macos/Sources/CDontSpeak/include/dontspeak.h");
        if let Some(dir) = macos_header.parent() {
            if dir.exists() {
                generated.write_to_file(&macos_header);
                println!("cargo:warning=regenerated {}", macos_header.display());
            }
        }
    }
}
