//! Shared loader for the FluidAudio C-ABI shim dylib (`libsmkokoro.dylib`).
//!
//! The apple-native STT backends dlopen this dylib: the Parakeet transcriber
//! ([`crate::coreml`]) and the System speech recognizer ([`crate::sysspeech`]). It
//! centralizes the `SMKOKORO_DYLIB_PATH` resolution + `dlopen` + the error string so
//! the callers can't drift. Each keeps its OWN `Library` handle (dlopen refcounts the
//! same image). (The shim still ships TTS symbols, but apple-native Kokoro TTS was
//! removed, so nothing here loads them anymore.)

use std::ffi::CString;

use libloading::Library;

/// The Core ML model directory to hand the shim's `smk_*_init` (its `modelDir` argument),
/// as a `CString`. Returns the DontSpeak-controlled [`ds_config::coreml_dir`] (created
/// if absent) so FluidAudio downloads under OUR cache folder — not its own scattered
/// per-model defaults (`~/.cache/fluidaudio`, `~/Library/Application Support/FluidAudio`) —
/// keeping every model under the one folder the uninstaller wipes. Falls back to `""`
/// (FluidAudio's default) only if the path can't resolve.
pub fn model_dir_arg() -> CString {
    if let Some(dir) = ds_config::coreml_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Some(s) = dir.to_str()
            && let Ok(c) = CString::new(s)
        {
            return c;
        }
    }
    CString::new("").unwrap()
}

/// The directory to hand the shim's `smk_asr_stream_start` (the STREAMING Parakeet EOU set),
/// as a `CString`. Unlike the offline [`model_dir_arg`], this is the EOU model's OWN subdir —
/// `ds_model::coreml_repo::parakeet_eou_dir` (the ONE source of truth shared with the download
/// target), since FluidAudio's `StreamingEouAsrManager.loadModels(from:)` loads the `.mlmodelc`
/// files FLAT from the dir it's given. NOT created here: the dir exists only once the model is
/// downloaded, and an absent dir makes `smk_asr_stream_start` fail → the caller cleanly falls
/// back to the offline path. Falls back to `""` only if the path can't resolve.
pub fn eou_model_dir_arg() -> CString {
    if let Some(dir) = ds_model::coreml_repo::parakeet_eou_dir()
        && let Some(s) = dir.to_str()
        && let Ok(c) = CString::new(s)
    {
        return c;
    }
    CString::new("").unwrap()
}

/// `dlopen` the shim dylib pointed to by `SMKOKORO_DYLIB_PATH` (set by the macOS
/// app, mirroring `ORT_DYLIB_PATH`). Errors if the env var is unset or the load
/// fails — the caller fails-quiet / falls back from there.
pub fn open() -> Result<Library, String> {
    let path = std::env::var("SMKOKORO_DYLIB_PATH")
        .map_err(|_| "SMKOKORO_DYLIB_PATH not set".to_string())?;
    // SAFETY: a trusted, app-signed dylib whose C ABI matches smkokoro.h.
    unsafe { Library::new(&path) }.map_err(|e| format!("dlopen {path}: {e}"))
}
