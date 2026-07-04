//! Build script for ds-core.
//!
//! When the `cbindgen` feature is enabled (and the crate is available in the
//! cargo cache), regenerate the committed C header `dontspeak.h` from the
//! `extern "C"` surface and copy it next to the Swift module map at
//! `apps/macos/Sources/CDontSpeak/include/dontspeak.h`.
//!
//! The header is COMMITTED, so the DEFAULT build (cbindgen feature off, e.g.
//! offline CI or a box with no cbindgen) does nothing here and the Swift build
//! still sees an up-to-date header. This decouples the Swift app build from
//! having a cbindgen install — only a `--features cbindgen` regen touches it.

fn main() {
    // Always rebuild if the FFI surface changes (so a `--features cbindgen`
    // build picks up edits).
    println!("cargo:rerun-if-changed=src/ffi.rs");
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
            // Non-fatal (mirrors the `generate()` failure path below): keep the committed
            // header rather than regenerating it with cbindgen's own (C++-flavored, e.g.
            // wrong usize/uintptr_t mapping) defaults. Previously this silently fell back to
            // `Config::default()` via `unwrap_or_default()` while still printing "regenerated"
            // on the next successful `generate()` call — a false success message for a header
            // built from the wrong config.
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
            // Non-fatal: keep the committed header. Print a warning only.
            println!("cargo:warning=cbindgen generate failed ({e}); keeping committed dontspeak.h");
            return;
        }
    };

    // Compute the repo paths from CARGO_MANIFEST_DIR (…/rust/crates/ds-core).
    //   parent() x3 → repo root → apps/macos/Sources/CDontSpeak/include/dontspeak.h
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
