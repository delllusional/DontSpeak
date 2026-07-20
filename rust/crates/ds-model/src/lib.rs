//! ds-model — locate & download model assets for dontspeak (ARCHITECTURE §C.1 / §D).
//!
//! Assets: the Parakeet STT model (streaming FastConformer: encoder + decoder + joiner + tokens) AND the native
//! Kokoro TTS set — `kokoro-v1.0-fp32.onnx`, voices, the tiny English G2P encoder/decoder,
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
//! Minimal deps by design: `attohttpc` via `ds-http` (tiny blocking HTTP over rustls,
//! no tokio; socket-level `read_timeout` is a per-read INACTIVITY timeout — right for
//! large model downloads, which intentionally omit a wall-clock total timeout),
//! `sha2`, `tempfile` (atomic rename), plus `flate2`+`tar` ONLY for the one-member
//! ORT `.tgz` extraction (model paths come from ds-config). No async runtime in the
//! engine.
//!
//! A stalled download retains its partial temp file and resumes with a validated HTTP Range
//! request. Download progress is exposed by the engine. The pure fns below are network-free
//! and unit-tested; `ensure` is exercised by localhost fixtures (no real CDN).
//!
//! Also: `read_retry` (AV/EDR transient-`NotFound`); [`update_check`] (shared HTTP GET).

use std::path::{Path, PathBuf};

mod archive;
pub mod download;
pub mod hash;
mod kokoro_frontend;
pub mod libraries;
pub mod mlx_repo;
/// MLX Audio shim loader for `ds-stt` + `ds-tts` (no cross-crate dependency).
#[cfg(target_os = "macos")]
pub mod mlx_shim;
pub mod ort;
mod read_retry;
pub mod setup;
pub mod spec;
pub mod target;
pub mod tts_assets;
pub mod update_check;
pub mod urls;

// Flat facade — stable `ds_model::<item>` paths.
pub use download::{
    ensure, ensure_in_dir, ensure_with_progress, prefetch_key, set_prefetch_source, url_basename,
};
pub use hash::{sha256_file, sha256_hex, verify_sha256};
pub use kokoro_frontend::{
    ensure_espeak_loader, ensure_espeak_loader_with_progress, ensure_japanese_dictionary,
    ensure_japanese_dictionary_with_progress, espeak_data_dir, espeak_library_path,
    espeak_root_dir, is_espeak_loader_present, is_japanese_dictionary_present,
    japanese_dictionary_dir,
};
pub use ort::{
    ONNXRUNTIME_VERSION, cuda_session_builder, ensure_onnxruntime,
    ensure_onnxruntime_with_progress, ensure_ort_dylib, ensure_ort_dylib_gpu,
    is_onnxruntime_dylib_version_ok, onnxruntime_dylib_file, onnxruntime_dylib_path,
    set_ort_dylib_path,
};
pub use read_retry::{read_model_file, read_model_file_to_string};
pub use setup::{
    run_setup_kokoro, run_setup_kokoro_frontend_with_progress, run_setup_kokoro_with_progress,
    run_setup_parakeet, run_setup_parakeet_with_progress, run_setup_sepformer_with_progress,
};
pub use spec::{
    DownloadFile, KOKORO_G2P_DECODER_FILE, KOKORO_G2P_ENCODER_FILE, KOKORO_ONNX_FILE,
    KOKORO_VOICES_FILE, ModelSpec, PARAKEET_DECODER_FILE, PARAKEET_ENCODER_FILE,
    PARAKEET_JOINER_FILE, PARAKEET_TOKENS_FILE, PrefetchItem, SEPFORMER_FILE,
    is_kokoro_frontend_present, is_kokoro_g2p_present, is_kokoro_present, is_parakeet_present,
    is_sepformer_present, kokoro_files, kokoro_frontend_files, kokoro_g2p_decoder_spec,
    kokoro_g2p_encoder_spec, kokoro_onnx_spec, kokoro_voices_spec, parakeet_decoder_spec,
    parakeet_dir, parakeet_encoder_spec, parakeet_files, parakeet_joiner_spec,
    parakeet_tokens_spec, prefetch_items, sepformer_spec,
};
pub use target::DownloadTarget;
pub use tts_assets::{
    TTS_ORT_ASSETS, TtsOrtAssetSet, is_tts_model_present, run_setup_tts_model_with_progress,
    tts_model_dir, tts_model_file_path, tts_model_files_present, tts_ort_asset_set,
};
pub use update_check::{UpdateInfo, check_for_update};

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
pub use ort::{
    cuda_onnxruntime_path, cuda_runtime_dir, ensure_cuda_runtime_with_progress,
    is_cuda_driver_present, is_cuda_runtime_present,
};

/// Serializes every in-crate test that mutates the process-wide `DONTSPEAK_MODEL_DIR` /
/// `ORT_DYLIB_PATH` env vars. ONE crate-level lock — a second module-local lock would NOT
/// serialize against this one, reintroducing the interleaving race the parallel test
/// runner makes possible (spec.rs and tts_assets.rs tests share it).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve a file name to its full path under [`ds_config::model_dir`].
/// `None` only if the per-OS data dir cannot be resolved.
pub fn model_path(file_name: &str) -> Option<PathBuf> {
    let path = Path::new(file_name);
    let mut components = path.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return None;
    }
    Some(ds_config::model_dir()?.join(path))
}
