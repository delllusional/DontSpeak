//! Shared FluidAudio C-ABI shim loader (`libsmkokoro`). One place for
//! `SMKOKORO_DYLIB_PATH` + dlopen so `ds-stt` and `ds-tts` can't drift (neither
//! depends on the other). Each caller keeps its own `Library` (dlopen refcounts).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use libloading::Library;

// Borrowed-result callbacks: shim fires once, sync, on this thread before return.
// We copy out; no free. Stack `&mut Option<…>` ctx — no channel/Send.

/// smkokoro.h borrowed-result callbacks; buffer valid only during the call.
pub type PcmCb = unsafe extern "C" fn(*mut c_void, *const f32, usize, i32);
pub type StrCb = unsafe extern "C" fn(*mut c_void, *const c_char);

unsafe extern "C" fn pcm_sink(ctx: *mut c_void, ptr: *const f32, len: usize, _rate: i32) {
    // SAFETY: `ctx` is collect_pcm's stack `Option<Vec<f32>>`; callback is sync same-thread.
    let slot = unsafe { &mut *(ctx as *mut Option<Vec<f32>>) };
    *slot = Some(if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: non-null `ptr` is `len` f32s owned by the shim for this callback (smkokoro.h).
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    });
}

unsafe extern "C" fn str_sink(ctx: *mut c_void, ptr: *const c_char) {
    // SAFETY: `ctx` is collect_str's stack `Option<String>`; callback is sync same-thread.
    let slot = unsafe { &mut *(ctx as *mut Option<String>) };
    *slot = Some(if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: non-null `ptr` is a NUL C string owned by the shim for this callback.
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    });
}

/// Run a PCM-returning shim call; copy out during the sync callback. `Ok` on status 0.
pub fn collect_pcm(call: impl FnOnce(*mut c_void, PcmCb) -> i32) -> Result<Vec<f32>, i32> {
    let mut out: Option<Vec<f32>> = None;
    let rc = call(&mut out as *mut _ as *mut c_void, pcm_sink);
    if rc != 0 {
        return Err(rc);
    }
    Ok(out.unwrap_or_default())
}

/// Like [`collect_pcm`] for a UTF-8 string.
pub fn collect_str(call: impl FnOnce(*mut c_void, StrCb) -> i32) -> Result<String, i32> {
    let mut out: Option<String> = None;
    let rc = call(&mut out as *mut _ as *mut c_void, str_sink);
    if rc != 0 {
        return Err(rc);
    }
    Ok(out.unwrap_or_default())
}

/// `smk_*_init` modelDir: our [`ds_config::coreml_dir`] (uninstaller wipe root), else `""`.
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
