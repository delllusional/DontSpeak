//! Shared macOS speech C-ABI shim loader. One dylib per runtime family (`sys`, `mlx`,
//! `fluid`), each with its own path variable and its own `Frameworks` member, so a host can
//! carry any subset. One place for the env vars + dlopen so `ds-stt` and `ds-tts` can't drift
//! (neither depends on the other). Each caller keeps its own `Library` (dlopen refcounts).

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use libloading::Library;

/// Which shim dylib a call targets. `Mlx` and `Fluid` are SEPARATE dylibs: one presence
/// answer cannot stand for both, or a Fluid-present/MLX-absent host silently falls through
/// to ONNX and the status row reads a plausible-looking "ORT CPU" (#241).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shim {
    Sys,
    Mlx,
    Fluid,
}

impl Shim {
    pub const ALL: &'static [Shim] = &[Shim::Sys, Shim::Mlx, Shim::Fluid];

    pub fn env_var(self) -> &'static str {
        match self {
            Shim::Sys => "DONTSPEAK_SYS_DYLIB_PATH",
            Shim::Mlx => "DONTSPEAK_MLX_DYLIB_PATH",
            Shim::Fluid => "DONTSPEAK_FLUID_DYLIB_PATH",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Shim::Sys => "libdontspeak_sys.dylib",
            Shim::Mlx => "libdontspeak_mlx.dylib",
            Shim::Fluid => "libdontspeak_fluid.dylib",
        }
    }

    /// The family's `ds_*_set_log_cb` export (dontspeak_shim.h).
    fn log_symbol(self) -> &'static [u8] {
        match self {
            Shim::Sys => b"ds_sys_set_log_cb\0",
            Shim::Mlx => b"ds_mlx_set_log_cb\0",
            Shim::Fluid => b"ds_fluid_set_log_cb\0",
        }
    }

    /// `ds-log` source token, which is what the Logs tab filters on.
    fn log_source(self) -> &'static str {
        match self {
            Shim::Sys => "sys",
            Shim::Mlx => "mlx",
            Shim::Fluid => "fluid",
        }
    }
}

/// Diagnostic sink registered with each dylib after `dlopen`. One thunk per family so the
/// source token is a Rust-side fact rather than something the shim reports.
type LogCb = unsafe extern "C" fn(i32, *const c_char);

unsafe extern "C" fn log_sys(level: i32, msg: *const c_char) {
    forward(Shim::Sys, level, msg)
}
unsafe extern "C" fn log_mlx(level: i32, msg: *const c_char) {
    forward(Shim::Mlx, level, msg)
}
unsafe extern "C" fn log_fluid(level: i32, msg: *const c_char) {
    forward(Shim::Fluid, level, msg)
}

/// Copy one shim line into the unified log. Cannot panic (null-checked pointer,
/// `to_string_lossy` is infallible, `ds-log`'s writer is fail-quiet), which matters because
/// an escaping panic across `extern "C"` aborts -- same contract as `pcm_sink` / `str_sink`.
fn forward(shim: Shim, level: i32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: NUL-terminated UTF-8 owned by the shim for this call only (dontspeak_shim.h);
    // copied before return, like `str_sink`.
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    // No installed logger (`log`'s default max level is Off) => `log::log!` would drop this
    // silently. The ds-stt/ds-tts examples dlopen a shim without calling `ds_log::init`, so
    // keep their diagnostics on stderr exactly as they are today.
    if log::max_level() == log::LevelFilter::Off {
        eprintln!("[{}] {text}", shim.log_source());
        return;
    }
    // Baseline filter is Info, so DEBUG lines drop unless DONTSPEAK_DEBUG raises it.
    let level = match level {
        0 => log::Level::Debug,
        2 => log::Level::Warn,
        3 => log::Level::Error,
        _ => log::Level::Info,
    };
    log::log!(target: shim.log_source(), level, "{text}");
}

// Borrowed-result callbacks: shim fires once, sync, on this thread before return.
// We copy out; no free. Stack `&mut Option<…>` ctx — no channel/Send.

/// dontspeak_shim.h borrowed-result callbacks; buffer valid only during the call.
pub type PcmCb = unsafe extern "C" fn(*mut c_void, *const f32, usize, i32);
pub type StrCb = unsafe extern "C" fn(*mut c_void, *const c_char);

unsafe extern "C" fn pcm_sink(ctx: *mut c_void, ptr: *const f32, len: usize, _rate: i32) {
    // SAFETY: `ctx` is collect_pcm's stack `Option<Vec<f32>>`; callback is sync same-thread.
    let slot = unsafe { &mut *(ctx as *mut Option<Vec<f32>>) };
    *slot = Some(if ptr.is_null() || len == 0 {
        Vec::new()
    } else {
        // SAFETY: non-null `ptr` is `len` f32s owned by the shim for this callback (dontspeak_shim.h).
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

fn validated_bundled_shim(shim: Shim, path: &Path) -> Result<PathBuf, String> {
    let dylib = path
        .canonicalize()
        .map_err(|e| format!("resolve {}: {e}", shim.env_var()))?;
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
    let expected = frameworks_dir.join(shim.file_name());
    if dylib != expected {
        return Err(format!(
            "{} is not the bundled {}",
            shim.env_var(),
            shim.file_name()
        ));
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

/// Cheap presence probe for the gates: the family's variable is set AND the path exists. No
/// dlopen, no `codesign` -- [`open`] is what validates the bundle member.
pub fn available(shim: Shim) -> bool {
    std::env::var_os(shim.env_var())
        .map(|p| Path::new(&p).exists())
        .unwrap_or(false)
}

/// `dlopen` the app-bundled, signature-verified dylib for one shim family, then register the
/// diagnostic sink. The caller fails quiet or falls back on rejection.
pub fn open(shim: Shim) -> Result<Library, String> {
    let path = std::env::var(shim.env_var()).map_err(|_| format!("{} not set", shim.env_var()))?;
    let path = validated_bundled_shim(shim, Path::new(&path))?;
    // SAFETY: `validated_bundled_shim` restricts this to the canonical Frameworks
    // member sealed by the verified DontSpeak app signature; its ABI is dontspeak_shim.h.
    let lib =
        unsafe { Library::new(&path) }.map_err(|e| format!("dlopen {}: {e}", path.display()))?;
    register_log_sink(shim, &lib);
    Ok(lib)
}

/// Best-effort: a missing setter leaves that dylib logging to stderr rather than failing the
/// load. Idempotent -- `open` runs per consumer and writes the same 'static pointer each time.
fn register_log_sink(shim: Shim, lib: &Library) {
    let thunk: LogCb = match shim {
        Shim::Sys => log_sys,
        Shim::Mlx => log_mlx,
        Shim::Fluid => log_fluid,
    };
    // SAFETY: the symbol's ABI is dontspeak_shim.h's `void (*)(ds_shim_log_cb)`. The thunk
    // lives in the Rust image (helper binary / libds_core.a), which outlives every `dlclose`.
    unsafe {
        if let Ok(set) = lib.get::<unsafe extern "C" fn(Option<LogCb>)>(shim.log_symbol()) {
            set(Some(thunk));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three families must stay addressable independently: a duplicated variable, file
    /// name, or setter symbol would silently make two of them share one answer -- the exact
    /// coupling the split removes (#241). Pure, no filesystem.
    #[test]
    fn every_shim_family_has_its_own_variable_file_and_log_symbol() {
        let vars: Vec<_> = Shim::ALL.iter().map(|s| s.env_var()).collect();
        let files: Vec<_> = Shim::ALL.iter().map(|s| s.file_name()).collect();
        let symbols: Vec<_> = Shim::ALL.iter().map(|s| s.log_symbol()).collect();
        let sources: Vec<_> = Shim::ALL.iter().map(|s| s.log_source()).collect();
        for set in [&vars, &files, &sources] {
            let mut sorted = set.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), 3, "{set:?} are not three distinct values");
        }
        let mut sorted = symbols.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            3,
            "{symbols:?} are not three distinct symbols"
        );
        assert!(
            symbols.iter().all(|s| s.ends_with(b"\0")),
            "dlsym names must be NUL-terminated"
        );
    }
}
