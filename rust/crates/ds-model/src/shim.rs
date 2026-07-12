//! Shared loader for the FluidAudio C-ABI shim dylib (`libsmkokoro.dylib`).
//!
//! The apple-native inference backends dlopen this dylib and share ONE loader so they
//! can't drift: the Parakeet transcriber and the System speech recognizer (both in
//! `ds-stt`), and the apple-native Kokoro TTS backend (`ds-tts`). It lives here in
//! `ds-model` — the crate that already owns the ORT runtime resolve + the Core ML
//! model downloads — so both `ds-stt` and `ds-tts` reach it without either depending
//! on the other. It centralizes the `SMKOKORO_DYLIB_PATH` resolution + `dlopen` + the
//! error string. Each caller keeps its OWN `Library` handle (dlopen refcounts the same
//! image).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

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
    // SAFETY: `ctx` is the `&mut Option<Vec<f32>>` stack slot `collect_pcm` threaded through
    // the shim call, which fires this callback once, synchronously, on the same thread
    // before returning — so the slot is live and nothing else aliases it during this call.
    let slot = unsafe { &mut *(ctx as *mut Option<Vec<f32>>) };
    *slot = Some(if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: per the borrowed-result contract (smkokoro.h), a non-null `ptr` points to
        // `len` valid f32s owned by the shim for the duration of this callback; `.to_vec()`
        // copies them out before we return.
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    });
}

unsafe extern "C" fn str_sink(ctx: *mut c_void, ptr: *const c_char) {
    // SAFETY: `ctx` is the `&mut Option<String>` stack slot `collect_str` threaded through
    // the shim call, which fires this callback once, synchronously, on the same thread
    // before returning — so the slot is live and nothing else aliases it during this call.
    let slot = unsafe { &mut *(ctx as *mut Option<String>) };
    *slot = Some(if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: per the borrowed-result contract (smkokoro.h), a non-null `ptr` is a
        // NUL-terminated C string owned by the shim for the duration of this callback;
        // `to_string_lossy().into_owned()` copies it out before we return.
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
    if let Some(dir) = crate::coreml_repo::parakeet_eou_dir()
        && let Some(s) = dir.to_str()
        && let Ok(c) = CString::new(s)
    {
        return c;
    }
    CString::new("").unwrap()
}

fn validated_bundled_shim(path: &Path) -> Result<PathBuf, String> {
    let dylib = path
        .canonicalize()
        .map_err(|e| format!("resolve SMKOKORO_DYLIB_PATH: {e}"))?;
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|e| format!("resolve current executable: {e}"))?;
    let macos_dir = executable
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "MacOS"))
        .ok_or("current executable is not inside an app bundle")?;
    let contents_dir = macos_dir
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "Contents"))
        .ok_or("current executable is not inside an app bundle")?;
    let frameworks_dir = contents_dir
        .join("Frameworks")
        .canonicalize()
        .map_err(|e| format!("resolve app Frameworks directory: {e}"))?;
    let expected = frameworks_dir.join("libsmkokoro.dylib");
    if dylib != expected {
        return Err("SMKOKORO_DYLIB_PATH is not the bundled libsmkokoro.dylib".to_string());
    }
    let app_bundle = contents_dir
        .parent()
        .ok_or("current executable has no app bundle root")?;
    let verified = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app_bundle)
        .status()
        .map_err(|e| format!("verify app signature: {e}"))?;
    if !verified.success() {
        return Err("app signature verification failed".to_string());
    }
    Ok(dylib)
}

/// `dlopen` the app-bundled, signature-verified shim dylib selected by
/// `SMKOKORO_DYLIB_PATH`. The caller fails quiet or falls back on rejection.
pub fn open() -> Result<Library, String> {
    let path = std::env::var("SMKOKORO_DYLIB_PATH")
        .map_err(|_| "SMKOKORO_DYLIB_PATH not set".to_string())?;
    let path = validated_bundled_shim(Path::new(&path))?;
    // SAFETY: `validated_bundled_shim` restricts this to the canonical Frameworks
    // member sealed by the verified DontSpeak app signature; its ABI is smkokoro.h.
    unsafe { Library::new(&path) }.map_err(|e| format!("dlopen {}: {e}", path.display()))
}
