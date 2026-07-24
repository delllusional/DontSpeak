//! Shared macOS speech C-ABI shim loader (`libdontspeak_mlx`). The arm64 dylib contains MLX Audio
//! and Apple System STT; the x86_64 compatibility build contains only System STT. One place for
//! `DONTSPEAK_MLX_DYLIB_PATH` + dlopen so `ds-stt` and `ds-tts` can't drift (neither
//! depends on the other). Each caller keeps its own `Library` (dlopen refcounts).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use libloading::Library;

// Borrowed-result callbacks: shim fires once, sync, on this thread before return.
// We copy out; no free. Stack `&mut Option<…>` ctx — no channel/Send.

/// dontspeak_mlx.h borrowed-result callbacks; buffer valid only during the call.
pub type PcmCb = unsafe extern "C" fn(*mut c_void, *const f32, usize, i32);
pub type StrCb = unsafe extern "C" fn(*mut c_void, *const c_char);

unsafe extern "C" fn pcm_sink(ctx: *mut c_void, ptr: *const f32, len: usize, _rate: i32) {
    // SAFETY: `ctx` is collect_pcm's stack `Option<Vec<f32>>`; callback is sync same-thread.
    let slot = unsafe { &mut *(ctx as *mut Option<Vec<f32>>) };
    *slot = Some(if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: non-null `ptr` is `len` f32s owned by the shim for this callback (dontspeak_mlx.h).
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

fn path_arg(dir: Option<PathBuf>) -> CString {
    if let Some(dir) = dir
        && let Some(s) = dir.to_str()
        && let Ok(c) = CString::new(s)
    {
        return c;
    }
    CString::new("").unwrap()
}

/// Local directory for any built-in MLX TTS model.
pub fn tts_model_dir_arg(model: ds_config::TtsModel) -> CString {
    path_arg(crate::mlx_repo::tts_mlx_dir(model))
}

/// The Core ML root handed to FluidAudio's ANE Kokoro chain. FluidAudio appends the variant's
/// full `folderName` (`kokoro-82m-coreml/ANE`), so this is the root ABOVE the set dir -- see
/// [`crate::coreml_repo::kokoro_hub_root`], which shares one resolution with the download
/// target and with `ds-tts`'s voice-pack materializer.
pub fn fluid_kokoro_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::kokoro_hub_root(&roots));
    path_arg(dir)
}

/// Local MLX Parakeet directory, shared by batch and buffered streaming calls.
pub fn parakeet_model_dir_arg() -> CString {
    path_arg(crate::mlx_repo::parakeet_mlx_dir())
}

/// Directory handed to FluidAudio's `AsrModels.load(from:version:.v2)` for the batch Parakeet
/// set. The v0.15.5 loader strips the last path component and re-appends the v2 repo folder
/// (`parakeet-tdt-0.6b-v2`), so handing it the set directory itself round-trips to the same
/// files -- one resolution shared with the download target via `parakeet_batch_dir`.
pub fn fluid_parakeet_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::parakeet_batch_dir(&roots));
    path_arg(dir)
}

/// The `160ms/` streaming EOU directory `StreamingEouAsrManager.loadModels(from:)` reads the
/// `.mlmodelc` set + `vocab.json` from directly (no parent-stripping, unlike the batch loader).
pub fn fluid_parakeet_eou_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::parakeet_eou_dir(&roots));
    path_arg(dir)
}

/// The FluidAudio Core ML diarization set directory the shim loads
/// `pyannote_segmentation.mlmodelc` + `wespeaker_v2.mlmodelc` from directly (one resolution
/// shared with the download target via `diarization_coreml_dir`).
pub fn fluid_diarization_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::diarization_coreml_dir(&roots));
    path_arg(dir)
}

/// Parent directory containing the Sortformer and WeSpeaker model subdirectories.
pub fn mlx_model_root_arg() -> CString {
    path_arg(ds_config::mlx_dir())
}

fn validated_bundled_shim(path: &Path) -> Result<PathBuf, String> {
    let dylib = path
        .canonicalize()
        .map_err(|e| format!("resolve DONTSPEAK_MLX_DYLIB_PATH: {e}"))?;
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
    let expected = frameworks_dir.join("libdontspeak_mlx.dylib");
    if dylib != expected {
        return Err(
            "DONTSPEAK_MLX_DYLIB_PATH is not the bundled libdontspeak_mlx.dylib".to_string(),
        );
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
/// `DONTSPEAK_MLX_DYLIB_PATH`. The caller fails quiet or falls back on rejection.
pub fn open() -> Result<Library, String> {
    let path = std::env::var("DONTSPEAK_MLX_DYLIB_PATH")
        .map_err(|_| "DONTSPEAK_MLX_DYLIB_PATH not set".to_string())?;
    let path = validated_bundled_shim(Path::new(&path))?;
    // SAFETY: `validated_bundled_shim` restricts this to the canonical Frameworks
    // member sealed by the verified DontSpeak app signature; its ABI is dontspeak_mlx.h.
    unsafe { Library::new(&path) }.map_err(|e| format!("dlopen {}: {e}", path.display()))
}
