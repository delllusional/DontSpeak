//! System STT: Apple on-device en-US (macOS). SpeechAnalyzer 26+, legacy
//! `SFSpeechRecognizer` 14–25 (shim picks). Its own dependency-free `libdontspeak_sys` dylib,
//! bundled on every macOS arch. No model download — OS recognizer only; missing on-device
//! locale → UNAVAILABLE.

use std::ffi::c_void;

use libloading::{Library, Symbol};

use crate::streaming::{StreamingStt, timed};
use ds_model::shim::StrCb;

type SysAvailFn = unsafe extern "C" fn() -> i32;
type SysAuthorizeFn = unsafe extern "C" fn() -> i32;
// Text via borrowed callback (`collect_str`); no out-param / free.
type SysTranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;

// Streaming: `ds_sys_stream_*` (start/push/finish; no model-dir).
type SysStreamStartFn = unsafe extern "C" fn() -> i32;
type SysStreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type SysStreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

/// Usability of the System STT engine, mapped from the shim's `ds_sys_available` code.
/// Mirrors Parakeet's present/warming/ready split so the status dot reads the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemState {
    /// Ready to transcribe now (green dot) — on-device model installed (macOS 26+) /
    /// permission granted (macOS 14–25).
    Ready,
    /// Locale supported but a one-time step is pending — the on-device model download
    /// (macOS 26+) or the Speech-Recognition permission prompt (14–25). Either runs on
    /// the authorize gate, or on demand on the first dictation. Orange dot, same as
    /// Parakeet warming.
    Preparing,
    /// Locale unsupported, permission denied, or the shim is absent — cannot run.
    Unavailable,
}

/// Turn a shim status code (see dontspeak_sys.h) into a human reason for the unavailable
/// cases; `0` (ready) and `1` (preparing) have no error reason.
fn reason_for(rc: i32) -> Option<String> {
    match rc {
        0 | 1 => None,
        2 => Some(
            "on-device speech recognition isn't available for your locale — enabling \
             Dictation in System Settings can install it"
                .into(),
        ),
        3 => Some("system speech recognition isn't supported on this version of macOS".into()),
        4 => Some(
            "speech recognition permission was denied — allow DontSpeak under System \
             Settings → Privacy & Security → Speech Recognition"
                .into(),
        ),
        _ => Some("the system speech recognizer is unavailable".into()),
    }
}

/// Probe the shim's `ds_sys_available` WITHOUT prompting/downloading (safe for the
/// frequent model-status poll). Shim absent (non-app build) ⇒ [`SystemState::Unavailable`].
pub fn state() -> SystemState {
    let Ok(lib) = ds_model::shim::open(ds_model::shim::Shim::Sys) else {
        return SystemState::Unavailable;
    };
    // SAFETY: app-signed dylib whose C ABI matches dontspeak_sys.h.
    let rc = unsafe {
        lib.get::<SysAvailFn>(b"ds_sys_available\0")
            .map(|f| f())
            .unwrap_or(-1)
    };
    match rc {
        0 => SystemState::Ready,
        1 => SystemState::Preparing,
        _ => SystemState::Unavailable,
    }
}

/// Is on-device system STT usable at all right now (ready OR preparing — the model
/// downloads on demand)? The `build_stt` gate: true ⇒ route Caps dictation through the
/// helper; false ⇒ the inert engine (no silent fallback). `Preparing` counts as usable so
/// the engine goes live (orange) and the first dictation triggers the on-demand download.
pub fn available() -> bool {
    state() != SystemState::Unavailable
}

/// Request Speech Recognition authorization (prompts on first use), BLOCKING, then
/// re-check. `Ok(())` when usable afterwards; `Err(reason)` otherwise. Called both when
/// the user explicitly opts into `stt_engine=system` (so the prompt is attributed to
/// DontSpeak.app and enabling never silently degrades) and automatically at boot/reload
/// when the config resolves to System via the default ladder — see
/// `dontspeakd::boot::authorize_system_stt_if_needed`.
pub fn authorize() -> Result<(), String> {
    let lib = ds_model::shim::open(ds_model::shim::Shim::Sys)?;
    // SAFETY: app-signed dylib whose C ABI matches dontspeak_sys.h.
    let rc = unsafe {
        let f: Symbol<SysAuthorizeFn> = lib
            .get(b"ds_sys_authorize\0")
            .map_err(|e| format!("ds_sys_authorize symbol: {e}"))?;
        f()
    };
    match reason_for(rc) {
        None => Ok(()),
        Some(reason) => Err(reason),
    }
}

/// `SFSpeechRecognizer` ASR behind the C ABI. No model files — the recognizer is the
/// OS's, so `preload` only opens the shim and `unload` is a no-op.
pub struct SystemTranscriber {
    lib: Option<Library>,
}

impl SystemTranscriber {
    pub fn new() -> Self {
        SystemTranscriber { lib: None }
    }

    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(ds_model::shim::open(ds_model::shim::Shim::Sys)?);
        }
        Ok(())
    }

    pub fn preload(&mut self) -> Result<(), String> {
        self.ensure_lib()
    }

    pub fn unload(&mut self) -> bool {
        false
    }

    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        self.ensure_lib()?;
        let lib = self.lib.as_ref().expect("lib opened above");
        // SAFETY: the shim exports this name with the `SysTranscribeFn` C ABI.
        let tr: Symbol<SysTranscribeFn> = unsafe { lib.get(b"ds_sys_transcribe\0") }
            .map_err(|e| format!("ds_sys_transcribe symbol: {e}"))?;
        // SAFETY: `pcm` outlives the blocking call; `ctx`/`cb` are `collect_str`'s pair.
        ds_model::shim::collect_str(|ctx, cb| unsafe {
            tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("ds_sys_transcribe failed (rc={rc})"))
    }
}

impl Default for SystemTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming system STT ([`StreamingStt`]); same session loop as ONNX/MLX.
/// Own shim handle (`dlopen` refcounts). Auth/availability stay in [`state`]/[`authorize`].
pub struct SystemStreamer {
    lib: Library,
    /// Wall ms in `push`/`finish` for the current utterance (STTSTATS).
    transcribe_ms: f64,
}

impl SystemStreamer {
    /// Open shim; `Err` → offline fallback.
    pub fn new() -> Result<Self, String> {
        let lib = ds_model::shim::open(ds_model::shim::Shim::Sys)?;
        Ok(Self {
            lib,
            transcribe_ms: 0.0,
        })
    }

    fn push(&self, pcm: &[f32]) -> Result<String, String> {
        // SAFETY: symbol matches `SysStreamPushFn`; Symbol borrows `self.lib`.
        let f: Symbol<SysStreamPushFn> = unsafe { self.lib.get(b"ds_sys_stream_push\0") }
            .map_err(|e| format!("ds_sys_stream_push symbol: {e}"))?;
        // SAFETY: `pcm` outlives the blocking call; `ctx`/`cb` are `collect_str`'s pair.
        ds_model::shim::collect_str(|ctx, cb| unsafe {
            f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("ds_sys_stream_push failed (rc={rc})"))
    }
}

impl StreamingStt for SystemStreamer {
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: symbol matches `SysStreamStartFn` (no args).
        let rc = unsafe {
            let f: Symbol<SysStreamStartFn> = self
                .lib
                .get(b"ds_sys_stream_start\0")
                .map_err(|e| format!("ds_sys_stream_start symbol: {e}"))?;
            f()
        };
        if rc != 0 {
            return Err(format!("ds_sys_stream_start failed (rc={rc})"));
        }
        self.transcribe_ms = 0.0;
        Ok(())
    }

    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        let (result, elapsed_ms) = timed(|| self.push(pcm_16k));
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn finalize(&mut self) -> Result<String, String> {
        // SAFETY: the shim exports this name with the `SysStreamFinishFn` C ABI.
        let f: Symbol<SysStreamFinishFn> = unsafe { self.lib.get(b"ds_sys_stream_finish\0") }
            .map_err(|e| format!("ds_sys_stream_finish symbol: {e}"))?;
        let (result, elapsed_ms) = timed(|| {
            // SAFETY: `collect_str` pair; call takes no other pointers.
            ds_model::shim::collect_str(|ctx, cb| unsafe { f(ctx, cb) })
                .map_err(|rc| format!("ds_sys_stream_finish failed (rc={rc})"))
        });
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn transcribe_ms(&self) -> f64 {
        self.transcribe_ms
    }

    fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::System
    }
}
