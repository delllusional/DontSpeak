//! ds-model — locate & download model assets for dontspeak (ARCHITECTURE §C.1 / §D).
//!
//! Assets: the Parakeet STT model (streaming FastConformer: encoder + decoder + joiner + tokens) AND the native
//! Kokoro TTS triple — `kokoro-v1.0.onnx` (~310 MB), `voices-v1.0.bin` (~28 MB),
//! and the matching `libonnxruntime` dylib for `ort` (load-dynamic, resolved at
//! runtime). One base dir [`ds_config::model_dir`] holds every asset; each is a
//! [`ModelSpec`] with a pinned SHA-256. [`ensure`] returns a cached file if its
//! checksum matches, else downloads to a `.part` temp file (blocking `attohttpc`,
//! N retries), verifies the SHA-256, and atomically renames it onto the final
//! path — never leaving a half-written model behind.
//!
//! ONNXRUNTIME, two routes (documented in README):
//!   (A) DEFAULT — [`ensure_onnxruntime`] downloads the version-matched prebuilt
//!       ORT for the platform (the pyke CDN `.tgz` `ort` itself trusts; pinned
//!       SHA-256), extracts the single `libonnxruntime*.dylib` member, and lands
//!       it in `model_dir()`. The caller sets `ORT_DYLIB_PATH` to it. Keeps the
//!       host build onnxruntime-free and the binary lean.
//!   (B) FALLBACK — the `ort` crate's own `download-binaries` cargo feature
//!       fetches a vetted ORT at BUILD time. Not the default (it bakes the lib).
//!
//! Minimal deps by design: `attohttpc` (tiny blocking HTTP over rustls, no tokio;
//! its socket-level `read_timeout` gives a per-read INACTIVITY timeout — right for
//! large model downloads), `sha2`, `tempfile` (atomic rename), plus `flate2`+`tar`
//! ONLY for the one-member ORT `.tgz` extraction (model paths come from ds-config).
//! No async runtime in the engine.
//!
//! TODO: HTTP Range-resume on a stalled download (§D, here we full re-download on
//! failure); a `DownloadStatus` progress channel for the GUI. The pure fns below
//! are network-free and unit-tested; `ensure` is exercised by a localhost-
//! TcpListener fixture (no real CDN).
//!
//! ## Module map
//! - [`hash`] — SHA-256 + checksum verify (pure).
//! - [`spec`] — the asset catalog (URLs + pinned digests + sizes), presence
//!   probes, and the installer prefetch list.
//! - [`download`] — the network engine: atomic temp+rename, retry, sha-verify,
//!   and the installer prefetch fast-path.
//! - `archive` — extract the onnxruntime lib / CUDA DLLs from the archives.
//! - [`ort`] — onnxruntime dylib resolve + version gate + fetch + the CUDA runtime.
//! - [`setup`] — eager pre-download orchestrators with aggregate progress.

use std::path::PathBuf;

mod archive;
/// Self-managed FluidAudio (Core ML / ANE) downloads at pinned revisions (see module docs).
pub mod coreml_repo;
pub mod download;
pub mod hash;
/// Third-party libraries catalog (the downloaded models + runtime + their licenses),
/// collected from the [`urls`] registry for the UI's Libraries tab (see the module docs).
pub mod libraries;
pub mod ort;
pub mod setup;
pub mod spec;
/// The canonical [`DownloadTarget`] enum — the single definition of every download/prefetch
/// target token, parsed-to and matched-on by all three dispatchers (see the module docs).
pub mod target;
/// THE single registry of every download URL + SHA-256 + size (see the module docs).
pub mod urls;

// Flat facade: every public item keeps its historical `ds_model::<item>` path.
pub use download::{ensure, ensure_with_progress, set_prefetch_source, url_basename};
pub use hash::{sha256_file, sha256_hex, verify_sha256};
pub use ort::{
    ONNXRUNTIME_VERSION, ensure_onnxruntime, ensure_onnxruntime_with_progress, ensure_ort_dylib,
    ensure_ort_dylib_gpu, onnxruntime_dylib_file, onnxruntime_dylib_path,
    onnxruntime_dylib_version_ok, set_ort_dylib_path,
};
pub use setup::{
    run_setup_kokoro, run_setup_kokoro_voices_with_progress, run_setup_kokoro_with_progress,
    run_setup_parakeet, run_setup_parakeet_with_progress,
};
pub use target::DownloadTarget;
pub use spec::{
    DownloadFile, KOKORO_ONNX_FILE, KOKORO_VOICES_FILE, ModelSpec, PARAKEET_DECODER_FILE,
    PARAKEET_ENCODER_FILE, PARAKEET_JOINER_FILE, PARAKEET_TOKENS_FILE, PrefetchItem, kokoro_files,
    kokoro_onnx_spec, kokoro_present, kokoro_voices_spec, parakeet_decoder_spec, parakeet_dir,
    parakeet_encoder_spec, parakeet_files, parakeet_joiner_spec, parakeet_present,
    parakeet_tokens_spec, prefetch_items,
};

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub use ort::{
    cuda_driver_present, cuda_onnxruntime_path, cuda_runtime_dir, cuda_runtime_present,
    ensure_cuda_runtime_with_progress,
};

/// Resolve a file name to its full path under [`ds_config::model_dir`].
/// `None` only if the per-OS data dir cannot be resolved.
pub fn model_path(file_name: &str) -> Option<PathBuf> {
    Some(ds_config::model_dir()?.join(file_name))
}
