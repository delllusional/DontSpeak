//! FluidAudio Parakeet TDT v2 STT on Core ML / ANE (macOS aarch64).
//! Same `libdontspeak_fluid` dylib as Fluid TTS via [`ds_model::shim`]; ASR symbols only.
//! Lazy-load shape matches MLX so the helper drives it through [`crate::local::LocalTranscriber`].

use std::ffi::{c_char, c_void};

use libloading::{Library, Symbol};

use crate::streaming::{StreamingStt, timed};
use ds_model::shim::StrCb;

// Text via borrowed callback (`collect_str`); init/shutdown/start plain int32.
type AsrInitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
type TranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type AsrShutdownFn = unsafe extern "C" fn();

// Streaming: start / push (live hypothesis) / finish.
type StreamStartFn = unsafe extern "C" fn(*const c_char) -> i32;
type StreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type StreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

/// Parakeet ASR behind the C ABI. Models load offline on first `preload`/transcribe.
pub struct FluidTranscriber {
    lib: Option<Library>,
    loaded: bool,
}

impl FluidTranscriber {
    pub fn new() -> Self {
        FluidTranscriber {
            lib: None,
            loaded: false,
        }
    }

    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(ds_model::shim::open(ds_model::shim::Shim::Fluid)?);
        }
        Ok(())
    }

    pub fn preload(&mut self) -> Result<(), String> {
        if self.loaded {
            return Ok(());
        }
        self.ensure_lib()?;
        let lib = self.lib.as_ref().expect("lib opened above");
        // SAFETY: app-signed shim `AsrInitFn`; `dir` lives across the call. compute_units=0 reserved.
        let rc = unsafe {
            let init: Symbol<AsrInitFn> = lib
                .get(b"ds_fluid_asr_init\0")
                .map_err(|e| format!("ds_fluid_asr_init symbol: {e}"))?;
            let dir = ds_model::shim::fluid_parakeet_dir_arg();
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("ds_fluid_asr_init failed (rc={rc})"));
        }
        self.loaded = true;
        Ok(())
    }

    pub fn unload(&mut self) -> bool {
        if !self.loaded {
            return false;
        }
        if let Some(lib) = &self.lib {
            // SAFETY: idempotent shim shutdown.
            unsafe {
                if let Ok(sd) = lib.get::<AsrShutdownFn>(b"ds_fluid_asr_shutdown\0") {
                    sd();
                }
            }
        }
        self.loaded = false;
        true
    }

    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        self.preload()?;
        let lib = self.lib.as_ref().expect("lib loaded above");
        // SAFETY: `TranscribeFn` ABI; `pcm` + `collect_str` pair outlive the call.
        let tr: Symbol<TranscribeFn> = unsafe { lib.get(b"ds_fluid_transcribe\0") }
            .map_err(|e| format!("ds_fluid_transcribe symbol: {e}"))?;
        ds_model::shim::collect_str(|ctx, cb| {
            // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking call;
            // `collect_str` supplies a synchronous pair the shim does not retain.
            unsafe { tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
        })
        .map_err(|rc| format!("ds_fluid_transcribe failed (rc={rc})"))
    }
}

impl Default for FluidTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for FluidTranscriber {
    fn drop(&mut self) {
        self.unload();
    }
}

/// Streaming FluidAudio STT ([`StreamingStt`]); same session loop as ONNX/MLX.
/// Eager load so a missing shim/model fails before STT is advertised ready.
pub struct FluidStreamer {
    lib: Library,
    model_dir: std::ffi::CString,
    /// Wall ms in `push`/`finish` for the current utterance (STTSTATS).
    transcribe_ms: f64,
}

impl FluidStreamer {
    /// Open shim + load streaming EOU model. `Err` → offline fallback.
    pub fn new() -> Result<Self, String> {
        let lib = ds_model::shim::open(ds_model::shim::Shim::Fluid)?;
        let mut streamer = Self {
            lib,
            model_dir: ds_model::shim::fluid_parakeet_eou_dir_arg(),
            transcribe_ms: 0.0,
        };
        streamer.reset()?;
        Ok(streamer)
    }

    fn push(&self, sym: &[u8], pcm: &[f32]) -> Result<String, String> {
        // SAFETY: `StreamPushFn`; Symbol borrows `self.lib`; `pcm` + collect_str outlive call.
        let f: Symbol<StreamPushFn> = unsafe { self.lib.get(sym) }
            .map_err(|e| format!("{} symbol: {e}", String::from_utf8_lossy(sym)))?;
        ds_model::shim::collect_str(|ctx, cb| {
            // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking call;
            // `collect_str` supplies a synchronous pair the shim does not retain.
            unsafe { f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
        })
        .map_err(|rc| format!("{} failed (rc={rc})", String::from_utf8_lossy(sym)))
    }
}

impl Drop for FluidStreamer {
    fn drop(&mut self) {
        // Utterance state only; leave process-global warm model for FluidTranscriber.
        // SAFETY: idempotent stream shutdown; Symbol cannot outlive `self.lib`.
        unsafe {
            if let Ok(shutdown) = self
                .lib
                .get::<AsrShutdownFn>(b"ds_fluid_asr_stream_shutdown\0")
            {
                shutdown();
            }
        }
    }
}

impl StreamingStt for FluidStreamer {
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: `StreamStartFn`; `model_dir` lives across the call.
        let rc = unsafe {
            let f: Symbol<StreamStartFn> = self
                .lib
                .get(b"ds_fluid_asr_stream_start\0")
                .map_err(|e| format!("ds_fluid_asr_stream_start symbol: {e}"))?;
            f(self.model_dir.as_ptr())
        };
        if rc != 0 {
            return Err(format!("ds_fluid_asr_stream_start failed (rc={rc})"));
        }
        self.transcribe_ms = 0.0;
        Ok(())
    }

    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        let (result, elapsed_ms) = timed(|| self.push(b"ds_fluid_asr_stream_push\0", pcm_16k));
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn finalize(&mut self) -> Result<String, String> {
        // SAFETY: `StreamFinishFn` + collect_str pair.
        let f: Symbol<StreamFinishFn> = unsafe { self.lib.get(b"ds_fluid_asr_stream_finish\0") }
            .map_err(|e| format!("ds_fluid_asr_stream_finish symbol: {e}"))?;
        let (result, elapsed_ms) = timed(|| {
            ds_model::shim::collect_str(|ctx, cb| {
                // SAFETY: `collect_str` keeps its context/callback pair valid through this
                // blocking call; the shim invokes it synchronously and does not retain it.
                unsafe { f(ctx, cb) }
            })
            .map_err(|rc| format!("ds_fluid_asr_stream_finish failed (rc={rc})"))
        });
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn transcribe_ms(&self) -> f64 {
        self.transcribe_ms
    }

    fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Fluid
    }
}
