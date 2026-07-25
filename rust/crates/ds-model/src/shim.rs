//! Shared macOS speech C-ABI shim loader. One dylib per family (`sys` / `mlx` / `fluid`),
//! each with its own env var and Frameworks member. Single env+dlopen site so `ds-stt` and
//! `ds-tts` cannot drift. Callers retain each loaded `Library` for its use period.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};

use libloading::Library;

/// Shim dylib family. `Mlx` and `Fluid` are separate: one presence must not stand for both
/// or Fluid-present/MLX-absent falls to ONNX with a plausible "ORT CPU" status (#241).
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

    /// `ds_*_set_log_cb` export (dontspeak_shim.h).
    fn log_symbol(self) -> &'static [u8] {
        match self {
            Shim::Sys => b"ds_sys_set_log_cb\0",
            Shim::Mlx => b"ds_mlx_set_log_cb\0",
            Shim::Fluid => b"ds_fluid_set_log_cb\0",
        }
    }

    /// `ds-log` source token (Logs tab filter).
    fn log_source(self) -> &'static str {
        match self {
            Shim::Sys => "sys",
            Shim::Mlx => "mlx",
            Shim::Fluid => "fluid",
        }
    }
}

/// Post-`dlopen` diagnostic sink. One thunk per family so the source token is Rust-side.
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

/// Copy one shim line into the unified log. Must not panic across `extern "C"` (abort) —
/// same contract as `pcm_sink` / `str_sink`.
fn forward(shim: Shim, level: i32, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    // SAFETY: NUL C string owned by the shim for this call only (dontspeak_shim.h); copy out.
    let text = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    // No logger (default Off) would drop this; examples dlopen without `ds_log::init`.
    if log::max_level() == log::LevelFilter::Off {
        eprintln!("[{}] {text}", shim.log_source());
        return;
    }
    // Baseline Info; DEBUG needs DONTSPEAK_DEBUG.
    let level = match level {
        0 => log::Level::Debug,
        2 => log::Level::Warn,
        3 => log::Level::Error,
        _ => log::Level::Info,
    };
    log::log!(target: shim.log_source(), level, "{text}");
}

// Borrowed-result callbacks: sync, same-thread, once before return. Copy out; no free.

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

/// PCM-returning shim call; copy out on sync callback. `Ok` on status 0.
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

pub fn tts_model_dir_arg(model: ds_config::TtsModel) -> CString {
    path_arg(crate::mlx_repo::tts_mlx_dir(model))
}

/// Core ML root for Fluid ANE Kokoro — see [`crate::coreml_repo::kokoro_hub_root`].
pub fn fluid_kokoro_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::kokoro_hub_root(&roots));
    path_arg(dir)
}

pub fn parakeet_model_dir_arg() -> CString {
    path_arg(crate::mlx_repo::parakeet_mlx_dir())
}

/// Batch Parakeet dir for Fluid `AsrModels.load` — see [`crate::coreml_repo::parakeet_batch_dir`].
pub fn fluid_parakeet_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::parakeet_batch_dir(&roots));
    path_arg(dir)
}

/// Streaming EOU dir — see [`crate::coreml_repo::parakeet_eou_dir`].
pub fn fluid_parakeet_eou_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::parakeet_eou_dir(&roots));
    path_arg(dir)
}

/// Diarization set dir — see [`crate::coreml_repo::diarization_coreml_dir`].
pub fn fluid_diarization_dir_arg() -> CString {
    let dir = crate::hf_repo::ModelRoots::ambient()
        .map(|roots| crate::coreml_repo::diarization_coreml_dir(&roots));
    path_arg(dir)
}

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

/// Env var set and path exists. No dlopen/`codesign` — [`open`] validates the bundle member.
pub fn available(shim: Shim) -> bool {
    std::env::var_os(shim.env_var())
        .map(|p| Path::new(&p).exists())
        .unwrap_or(false)
}

/// `dlopen` the app-bundled, signature-verified dylib and register the log sink.
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

/// Best-effort log sink; missing setter leaves stderr. Idempotent across consumers.
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

    /// Families stay independent: duplicated env/file/symbol would reintroduce #241.
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
