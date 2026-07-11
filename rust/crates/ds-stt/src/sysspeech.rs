//! System speech-to-text: Apple's on-device recognizer, en-US. macOS only —
//! SpeechAnalyzer on macOS 26+, legacy `SFSpeechRecognizer` on 14–25 (the shim picks).
//! `dlopen`s `libsmkokoro.dylib` (the SAME shim as the apple-native Kokoro TTS and
//! Parakeet STT backends) via `SMKOKORO_DYLIB_PATH`, and transcribes 16 kHz mono f32
//! PCM → text through Apple's recognizer. Mirrors [`crate::coreml::CoremlTranscriber`]'s
//! lazy-load interface (`preload`/`unload`/`transcribe_pcm_16k`) so the helper can hold
//! it behind [`crate::local::LocalTranscriber`].
//!
//! Distinct from Parakeet: there is no model to download or remove — the recognizer is
//! the OS's. `requiresOnDeviceRecognition` keeps audio on the machine; when the locale
//! has no on-device model the engine reports UNAVAILABLE rather than falling back.

use std::ffi::c_void;

use libloading::{Library, Symbol};

use crate::streaming::{StreamingStt, timed};
use ds_model::shim::StrCb;

type SysAvailFn = unsafe extern "C" fn() -> i32;
type SysAuthorizeFn = unsafe extern "C" fn() -> i32;
// Transcription still BLOCKS and returns its status; the text comes back through a borrowed
// callback (copied out by `ds_model::shim::collect_str`), so there's no out-param and no smk_free_str.
type SysTranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;

// Streaming system-STT C ABI (the shim's `smk_sys_stream_*`, `apps/macos/SmKokoro/Sources/
// smkokoro/shim.swift`'s "System STT streaming" section). `start` begins a new utterance (no
// model-dir arg — unlike `smk_asr_stream_start`, there's no model to point at), `push` feeds a
// 16 kHz chunk and returns the hypothesis-so-far, `finish` flushes the final transcript. Same
// shapes as `crate::coreml`'s `StreamStartFn`/`StreamPushFn`/`StreamFinishFn`, distinctly named
// so both can be `dlopen`'d from the same dylib without symbol-name confusion in this crate.
type SysStreamStartFn = unsafe extern "C" fn() -> i32;
type SysStreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type SysStreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

/// Usability of the System STT engine, mapped from the shim's `smk_sys_available` code.
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

/// Turn a shim status code (see smkokoro.h) into a human reason for the unavailable
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

/// Probe the shim's `smk_sys_available` WITHOUT prompting/downloading (safe for the
/// frequent model-status poll). Shim absent (non-app build) ⇒ [`SystemState::Unavailable`].
pub fn state() -> SystemState {
    let Ok(lib) = ds_model::shim::open() else {
        return SystemState::Unavailable;
    };
    // SAFETY: app-signed dylib whose C ABI matches smkokoro.h.
    let rc = unsafe {
        lib.get::<SysAvailFn>(b"smk_sys_available\0")
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
    let lib = ds_model::shim::open()?;
    // SAFETY: app-signed dylib whose C ABI matches smkokoro.h.
    let rc = unsafe {
        let f: Symbol<SysAuthorizeFn> = lib
            .get(b"smk_sys_authorize\0")
            .map_err(|e| format!("smk_sys_authorize symbol: {e}"))?;
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
    /// Not loaded until the first [`preload`](Self::preload) / transcription.
    pub fn new() -> Self {
        SystemTranscriber { lib: None }
    }

    /// Ensure the shim dylib is open (resolves `SMKOKORO_DYLIB_PATH`).
    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(ds_model::shim::open()?);
        }
        Ok(())
    }

    /// Open the shim so the first utterance doesn't pay the dlopen cost. The recognizer
    /// itself is created lazily inside the shim on first use.
    pub fn preload(&mut self) -> Result<(), String> {
        self.ensure_lib()
    }

    /// Nothing persistent to free (the OS owns the recognizer); kept for interface parity.
    pub fn unload(&mut self) -> bool {
        false
    }

    /// Transcribe 16 kHz mono f32 PCM → text. Empty input → empty string.
    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        self.ensure_lib()?;
        let lib = self.lib.as_ref().expect("lib opened above");
        // SAFETY: `smk_sys_transcribe` in the app-signed shim has exactly
        // `SysTranscribeFn`'s signature (smkokoro.h); the returned Symbol borrows `lib`.
        let tr: Symbol<SysTranscribeFn> = unsafe { lib.get(b"smk_sys_transcribe\0") }
            .map_err(|e| format!("smk_sys_transcribe symbol: {e}"))?;
        // The shim borrows the transcript to our sink, which copies it out (no smk_free_str).
        // The call blocks; `pcm` lives across it.
        // SAFETY: `pcm.as_ptr()`/`len()` describe a live buffer that outlives the blocking
        // call, and `ctx`/`cb` are the borrowed-result pair `collect_str` supplies, fired
        // synchronously per smkokoro.h's callback contract.
        ds_model::shim::collect_str(|ctx, cb| unsafe {
            tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("smk_sys_transcribe failed (rc={rc})"))
    }
}

impl Default for SystemTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache-aware STREAMING System STT backend (Apple `SpeechAnalyzer`/`SFSpeechRecognizer`
/// behind the shim), implementing [`StreamingStt`] so the helper drives it through the SAME
/// [`crate::StreamSession`] + loop as the ONNX and Core ML/ANE paths. The shim's
/// `smk_sys_stream_*` ABI is the exact analogue of `CoremlStreamer`'s `smk_asr_stream_*`
/// (reset/accept/finalize). Opens its OWN shim handle, independent of [`SystemTranscriber`]'s
/// (safe — `ds_model::shim`'s doc comment confirms `dlopen` refcounts the same image). Loaded
/// eagerly in [`new`](Self::new) so a missing shim surfaces as an error → the caller falls back
/// to the offline path. No authorization/availability/batch-transcription logic here — that
/// stays exclusively in [`state`]/[`authorize`]/[`SystemTranscriber`] above.
pub struct SystemStreamer {
    lib: Library,
    /// Cumulative wall time (ms) spent inside the real `smk_sys_stream_push`/`_finish` FFI
    /// calls for the CURRENT utterance — zeroed by `reset`; mirrors `CoremlStreamer`'s field
    /// of the same name. Exposed via `StreamingStt::transcribe_ms` for the STTSTATS line.
    transcribe_ms: f64,
}

impl SystemStreamer {
    /// Open the shim (resolves `SMKOKORO_DYLIB_PATH`). `Err` (→ offline fallback) when the
    /// shim dylib is unavailable.
    pub fn new() -> Result<Self, String> {
        let lib = ds_model::shim::open()?;
        Ok(Self {
            lib,
            transcribe_ms: 0.0,
        })
    }

    fn push(&self, pcm: &[f32]) -> Result<String, String> {
        // SAFETY: `smk_sys_stream_push` in the app-signed shim has exactly
        // `SysStreamPushFn`'s signature (smkokoro.h); the returned Symbol borrows `self.lib`.
        let f: Symbol<SysStreamPushFn> = unsafe { self.lib.get(b"smk_sys_stream_push\0") }
            .map_err(|e| format!("smk_sys_stream_push symbol: {e}"))?;
        // Mirrors `SystemTranscriber::transcribe_pcm_16k`'s borrowed-callback pattern for the
        // batch symbol: the shim copies the transcript out during the call, so there's no
        // out-param and no `smk_free_str`.
        // SAFETY: `pcm.as_ptr()`/`len()` describe a live buffer that outlives the blocking
        // call, and `ctx`/`cb` are the borrowed-result pair `collect_str` supplies, fired
        // synchronously per smkokoro.h's callback contract.
        ds_model::shim::collect_str(|ctx, cb| unsafe {
            f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("smk_sys_stream_push failed (rc={rc})"))
    }
}

impl StreamingStt for SystemStreamer {
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: `smk_sys_stream_start` is looked up by NUL-terminated name from the
        // app-signed shim and has exactly `SysStreamStartFn`'s signature (smkokoro.h — no
        // arguments); the Symbol borrows `self.lib`.
        let rc = unsafe {
            let f: Symbol<SysStreamStartFn> = self
                .lib
                .get(b"smk_sys_stream_start\0")
                .map_err(|e| format!("smk_sys_stream_start symbol: {e}"))?;
            f()
        };
        if rc != 0 {
            return Err(format!("smk_sys_stream_start failed (rc={rc})"));
        }
        self.transcribe_ms = 0.0;
        Ok(())
    }

    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        // The shim accumulates internally; an empty chunk is a cheap no-op that just returns
        // the current hypothesis (the shared StreamSession may hand us an empty stable window).
        let (result, elapsed_ms) = timed(|| self.push(pcm_16k));
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn finalize(&mut self) -> Result<String, String> {
        // SAFETY: `smk_sys_stream_finish` in the app-signed shim has exactly
        // `SysStreamFinishFn`'s signature (smkokoro.h); the returned Symbol borrows
        // `self.lib`.
        let f: Symbol<SysStreamFinishFn> = unsafe { self.lib.get(b"smk_sys_stream_finish\0") }
            .map_err(|e| format!("smk_sys_stream_finish symbol: {e}"))?;
        let (result, elapsed_ms) = timed(|| {
            // SAFETY: `ctx`/`cb` are the borrowed-result pair `collect_str` supplies,
            // fired synchronously per smkokoro.h's callback contract; the call takes no
            // other pointers.
            ds_model::shim::collect_str(|ctx, cb| unsafe { f(ctx, cb) })
                .map_err(|rc| format!("smk_sys_stream_finish failed (rc={rc})"))
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
