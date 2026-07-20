//! MLX Parakeet TDT STT (macOS). Same `libdontspeak_mlx` shim as MLX TTS via
//! `DONTSPEAK_MLX_DYLIB_PATH`. Lazy-load shape matches [`crate::parakeet::ParakeetTranscriber`]
//! for [`crate::local::LocalTranscriber`].

use std::ffi::{c_char, c_void};

use libloading::{Library, Symbol};

use crate::streaming::{StreamingStt, timed};
use ds_model::mlx_shim::StrCb;

// Text-returning calls still BLOCK and return their status; the transcript comes back through a
// borrowed callback (copied out by `ds_model::mlx_shim::collect_str`), so there's no out-param and no
// `ds_mlx_free_str`. init/shutdown/start carry no buffer, so they keep the plain int32 ABI.
type AsrInitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
type TranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type AsrShutdownFn = unsafe extern "C" fn();

// Streaming ASR C ABI. `start` begins a new utterance, `push` buffers 16 kHz audio and
// periodically returns a refreshed live hypothesis, and `finish` returns the final transcript.
type StreamStartFn = unsafe extern "C" fn(*const c_char) -> i32;
type StreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type StreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

/// Parakeet ASR behind the C ABI. Models download on first `preload`/transcribe.
pub struct MlxTranscriber {
    lib: Option<Library>,
    loaded: bool,
}

impl MlxTranscriber {
    pub fn new() -> Self {
        MlxTranscriber {
            lib: None,
            loaded: false,
        }
    }

    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(ds_model::mlx_shim::open()?);
        }
        Ok(())
    }

    pub fn preload(&mut self) -> Result<(), String> {
        if self.loaded {
            return Ok(());
        }
        self.ensure_lib()?;
        let lib = self.lib.as_ref().expect("lib opened above");
        // SAFETY: symbol from app-signed shim matches `AsrInitFn` (dontspeak_mlx.h);
        // `dir` lives across the blocking call.
        let rc = unsafe {
            let init: Symbol<AsrInitFn> = lib
                .get(b"ds_mlx_asr_init\0")
                .map_err(|e| format!("ds_mlx_asr_init symbol: {e}"))?;
            let dir = ds_model::mlx_shim::parakeet_model_dir_arg();
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("ds_mlx_asr_init failed (rc={rc})"));
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
                if let Ok(sd) = lib.get::<AsrShutdownFn>(b"ds_mlx_asr_shutdown\0") {
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
        // SAFETY: the shim exports this name with the `TranscribeFn` C ABI.
        let tr: Symbol<TranscribeFn> = unsafe { lib.get(b"ds_mlx_transcribe\0") }
            .map_err(|e| format!("ds_mlx_transcribe symbol: {e}"))?;
        // SAFETY: `pcm` outlives the blocking call; `ctx`/`cb` are `collect_str`'s
        // borrowed-result pair (dontspeak_mlx.h).
        ds_model::mlx_shim::collect_str(|ctx, cb| unsafe {
            tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("ds_mlx_transcribe failed (rc={rc})"))
    }
}

impl Default for MlxTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MlxTranscriber {
    fn drop(&mut self) {
        self.unload();
    }
}

/// Streaming MLX STT ([`StreamingStt`]); same session loop as ONNX. Eager load so a missing
/// shim/model fails before STT ready.
pub struct MlxStreamer {
    lib: Library,
    model_dir: std::ffi::CString,
    /// Wall ms in `push`/`finish` for the current utterance (STTSTATS).
    transcribe_ms: f64,
}

impl MlxStreamer {
    /// Open the shim and load the streaming model. `Err` lets the caller use offline fallback.
    pub fn new() -> Result<Self, String> {
        let lib = ds_model::mlx_shim::open()?;
        let mut streamer = Self {
            lib,
            model_dir: ds_model::mlx_shim::parakeet_model_dir_arg(),
            transcribe_ms: 0.0,
        };
        streamer.reset()?;
        Ok(streamer)
    }

    fn push(&self, sym: &[u8], pcm: &[f32]) -> Result<String, String> {
        // SAFETY: NUL-terminated symbol matching `StreamPushFn`; Symbol borrows `self.lib`.
        let f: Symbol<StreamPushFn> = unsafe { self.lib.get(sym) }
            .map_err(|e| format!("{} symbol: {e}", String::from_utf8_lossy(sym)))?;
        // SAFETY: `pcm` outlives the blocking call; `ctx`/`cb` are `collect_str`'s pair.
        ds_model::mlx_shim::collect_str(|ctx, cb| unsafe {
            f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
        })
        .map_err(|rc| format!("{} failed (rc={rc})", String::from_utf8_lossy(sym)))
    }
}

impl Drop for MlxStreamer {
    fn drop(&mut self) {
        // Streamer owns utterance state only; keep process-global warm model for MlxTranscriber.
        // SAFETY: stream shutdown is idempotent; Symbol cannot outlive `self.lib`.
        unsafe {
            if let Ok(shutdown) = self
                .lib
                .get::<AsrShutdownFn>(b"ds_mlx_asr_stream_shutdown\0")
            {
                shutdown();
            }
        }
    }
}

impl StreamingStt for MlxStreamer {
    fn reset(&mut self) -> Result<(), String> {
        // SAFETY: symbol matches `StreamStartFn`; `model_dir` lives across the call.
        let rc = unsafe {
            let f: Symbol<StreamStartFn> = self
                .lib
                .get(b"ds_mlx_asr_stream_start\0")
                .map_err(|e| format!("ds_mlx_asr_stream_start symbol: {e}"))?;
            f(self.model_dir.as_ptr())
        };
        if rc != 0 {
            return Err(format!("ds_mlx_asr_stream_start failed (rc={rc})"));
        }
        self.transcribe_ms = 0.0;
        Ok(())
    }

    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        let (result, elapsed_ms) = timed(|| self.push(b"ds_mlx_asr_stream_push\0", pcm_16k));
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn finalize(&mut self) -> Result<String, String> {
        // SAFETY: the shim exports this name with the `StreamFinishFn` C ABI.
        let f: Symbol<StreamFinishFn> = unsafe { self.lib.get(b"ds_mlx_asr_stream_finish\0") }
            .map_err(|e| format!("ds_mlx_asr_stream_finish symbol: {e}"))?;
        let (result, elapsed_ms) = timed(|| {
            // SAFETY: `collect_str` pair; call takes no other pointers.
            ds_model::mlx_shim::collect_str(|ctx, cb| unsafe { f(ctx, cb) })
                .map_err(|rc| format!("ds_mlx_asr_stream_finish failed (rc={rc})"))
        });
        let text = result?;
        self.transcribe_ms += elapsed_ms;
        Ok(text)
    }

    fn transcribe_ms(&self) -> f64 {
        self.transcribe_ms
    }

    fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Mlx
    }
}
