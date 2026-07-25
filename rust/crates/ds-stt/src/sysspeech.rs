//! System STT: Apple on-device en-US (macOS). SpeechAnalyzer 26+, SFSpeechRecognizer 14–25
//! (shim picks). Own `libdontspeak_sys` dylib, all macOS arches. OS recognizer only.

use std::ffi::c_void;
use std::sync::OnceLock;

use libloading::{Library, Symbol};

use crate::streaming::{StreamingStt, timed};
use ds_model::shim::StrCb;

type SysAvailFn = unsafe extern "C" fn() -> i32;
type SysAuthorizeFn = unsafe extern "C" fn() -> i32;
// Text via collect_str.
type SysTranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;

// Streaming: start/push/finish (no model-dir).
type SysStreamStartFn = unsafe extern "C" fn() -> i32;
type SysStreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type SysStreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

// Cache rejection too: signed bundle path is fixed for the process; retry would re-codesign
// every status poll.
static SYSTEM_SHIM: OnceLock<Result<Library, String>> = OnceLock::new();

fn cached<T>(
    slot: &OnceLock<Result<T, String>>,
    init: impl FnOnce() -> Result<T, String>,
) -> Result<&T, String> {
    slot.get_or_init(init).as_ref().map_err(Clone::clone)
}

/// Process-lifetime verified dlopen; codesign gate runs at most once.
fn system_shim() -> Result<&'static Library, String> {
    cached(&SYSTEM_SHIM, || {
        ds_model::shim::open(ds_model::shim::Shim::Sys)
    })
}

/// Usability from `ds_sys_available` — same present/warming/ready status-dot shape as Parakeet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemState {
    /// Transcribe now (green) — model installed (26+) / permission granted (14–25).
    Ready,
    /// Locale ok; pending on-device download (26+) or Speech permission (14–25). Orange.
    Preparing,
    /// Unsupported locale, permission denied, or shim absent.
    Unavailable,
}

/// Shim status code → human reason for unavailable; ready/preparing have none.
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

/// Probe `ds_sys_available` without prompt/download (status-poll safe).
/// Shim absent ⇒ [`SystemState::Unavailable`].
pub fn state() -> SystemState {
    let Ok(lib) = system_shim() else {
        return SystemState::Unavailable;
    };
    // SAFETY: app-signed dylib; ABI matches dontspeak_sys.h.
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

/// Ready or Preparing (model downloads on demand). `build_stt` gate: true → Caps
/// dictation via helper; false → inert engine (no silent fallback).
pub fn available() -> bool {
    state() != SystemState::Unavailable
}

/// Blocking Speech Recognition authorize + re-check. Used on explicit `stt_engine=system`
/// and at boot when the ladder resolves to System
/// (`dontspeakd::boot::authorize_system_stt_if_needed`).
pub fn authorize() -> Result<(), String> {
    let lib = system_shim()?;
    // SAFETY: app-signed dylib; ABI matches dontspeak_sys.h.
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

/// Apple System Speech ASR. OS owns models: `preload` opens the shim; `unload` is a no-op.
pub struct SystemTranscriber {
    lib: Option<&'static Library>,
}

impl SystemTranscriber {
    pub fn new() -> Self {
        SystemTranscriber { lib: None }
    }

    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(system_shim()?);
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
        // SAFETY: `SysTranscribeFn`; `pcm` + collect_str outlive the call.
        let tr: Symbol<SysTranscribeFn> = unsafe { lib.get(b"ds_sys_transcribe\0") }
            .map_err(|e| format!("ds_sys_transcribe symbol: {e}"))?;
        ds_model::shim::collect_str(|ctx, cb| {
            // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking call;
            // `collect_str` supplies a synchronous pair the shim does not retain.
            unsafe { tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
        })
        .map_err(|rc| format!("ds_sys_transcribe failed (rc={rc})"))
    }
}

impl Default for SystemTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming system STT ([`StreamingStt`]); process-lifetime shim.
/// Auth/availability: [`state`] / [`authorize`].
pub struct SystemStreamer {
    lib: &'static Library,
    /// Wall ms in `push`/`finish` for the current utterance (STTSTATS).
    transcribe_ms: f64,
}

impl SystemStreamer {
    /// Acquire the shim; `Err` → offline fallback.
    pub fn new() -> Result<Self, String> {
        let lib = system_shim()?;
        Ok(Self {
            lib,
            transcribe_ms: 0.0,
        })
    }

    fn push(&self, pcm: &[f32]) -> Result<String, String> {
        // SAFETY: `SysStreamPushFn`; Symbol borrows `self.lib`.
        let f: Symbol<SysStreamPushFn> = unsafe { self.lib.get(b"ds_sys_stream_push\0") }
            .map_err(|e| format!("ds_sys_stream_push symbol: {e}"))?;
        ds_model::shim::collect_str(|ctx, cb| {
            // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking call;
            // `collect_str` supplies a synchronous pair the shim does not retain.
            unsafe { f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
        })
        .map_err(|rc| format!("ds_sys_stream_push failed (rc={rc})"))
    }
}

impl StreamingStt for SystemStreamer {
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: `SysStreamStartFn` (no args).
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
        // SAFETY: `SysStreamFinishFn` + collect_str.
        let f: Symbol<SysStreamFinishFn> = unsafe { self.lib.get(b"ds_sys_stream_finish\0") }
            .map_err(|e| format!("ds_sys_stream_finish symbol: {e}"))?;
        let (result, elapsed_ms) = timed(|| {
            ds_model::shim::collect_str(|ctx, cb| {
                // SAFETY: `collect_str` keeps its context/callback pair valid through this
                // blocking call; the shim invokes it synchronously and does not retain it.
                unsafe { f(ctx, cb) }
            })
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::cached;

    #[test]
    fn cached_handle_initializes_once_and_stays_live() {
        struct Handle<'a>(&'a AtomicUsize);
        impl Drop for Handle<'_> {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let opens = AtomicUsize::new(0);
        let drops = AtomicUsize::new(0);
        let slot = std::sync::OnceLock::new();
        let first = cached(&slot, || {
            opens.fetch_add(1, Ordering::Relaxed);
            Ok(Handle(&drops))
        })
        .unwrap();
        let second = cached(&slot, || {
            opens.fetch_add(1, Ordering::Relaxed);
            Err("must not reopen".into())
        })
        .unwrap();

        assert!(std::ptr::eq(first, second));
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cached_failure_is_not_retried_by_the_poll() {
        let opens = AtomicUsize::new(0);
        let slot = std::sync::OnceLock::<Result<(), String>>::new();
        for _ in 0..3 {
            assert_eq!(
                cached(&slot, || {
                    opens.fetch_add(1, Ordering::Relaxed);
                    Err("signature rejected".into())
                }),
                Err("signature rejected".into())
            );
        }
        assert_eq!(opens.load(Ordering::Relaxed), 1);
    }
}
