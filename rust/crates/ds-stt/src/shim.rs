//! Shared loader for the FluidAudio C-ABI shim dylib (`libsmkokoro.dylib`).
//!
//! The apple-native STT backends dlopen this dylib: the Parakeet transcriber
//! ([`crate::coreml`]) and the System speech recognizer ([`crate::sysspeech`]); the
//! apple-native Kokoro TTS backend (`ds-tts`) loads it through this same helper. It
//! centralizes the `SMKOKORO_DYLIB_PATH` resolution + `dlopen` + the error string so
//! the callers can't drift. Each keeps its OWN `Library` handle (dlopen refcounts the
//! same image).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};

use libloading::Library;

// ── borrowed-result callbacks ──────────────────────────────────────────────────────────
//
// The buffer-returning shim calls (synthesize/transcribe/diarize/embed) still BLOCK and still
// return their status code; what changed is how the RESULT crosses the boundary. Instead of
// handing back an owned buffer the caller must free (the old `float**`/`char**` out-param +
// `smk_free`/`smk_free_str` dance, with its two allocator families and pointer/len guards),
// the shim BORROWS the buffer to a callback it fires once, synchronously, before returning.
// We copy it out inside the callback — so there is no ownership transfer and nothing to free.
//
// Because the callback fires synchronously on THIS thread during the call, the context is just
// a `&mut Option<…>` on our stack: no channel, no Box, no Send/Sync concerns.

/// C borrowed-result callbacks (mirror the typedefs in smkokoro.h). The buffer is valid only
/// for the duration of the call.
pub type PcmCb = unsafe extern "C" fn(*mut c_void, *const f32, usize, i32);
pub type StrCb = unsafe extern "C" fn(*mut c_void, *const c_char);

unsafe extern "C" fn pcm_sink(ctx: *mut c_void, ptr: *const f32, len: usize, _rate: i32) {
    let slot = unsafe { &mut *(ctx as *mut Option<Vec<f32>>) };
    *slot = Some(if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    });
}

unsafe extern "C" fn str_sink(ctx: *mut c_void, ptr: *const c_char) {
    let slot = unsafe { &mut *(ctx as *mut Option<String>) };
    *slot = Some(if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    });
}

/// Invoke a buffer-returning shim fn that delivers its PCM through a synchronous borrowed
/// callback. `call` runs it with our sink + ctx and returns the shim's status code; the result
/// is copied out during the call. `Ok(samples)` on status 0 (empty if the shim produced none),
/// `Err(rc)` otherwise.
pub fn collect_pcm(call: impl FnOnce(*mut c_void, PcmCb) -> i32) -> Result<Vec<f32>, i32> {
    let mut out: Option<Vec<f32>> = None;
    let rc = call(&mut out as *mut _ as *mut c_void, pcm_sink);
    if rc != 0 {
        return Err(rc);
    }
    Ok(out.unwrap_or_default())
}

/// Like [`collect_pcm`] but for a UTF-8 string result. `Ok(text)` on status 0, `Err(rc)` otherwise.
pub fn collect_str(call: impl FnOnce(*mut c_void, StrCb) -> i32) -> Result<String, i32> {
    let mut out: Option<String> = None;
    let rc = call(&mut out as *mut _ as *mut c_void, str_sink);
    if rc != 0 {
        return Err(rc);
    }
    Ok(out.unwrap_or_default())
}

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
