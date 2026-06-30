//! Apple-native speech-to-text: FluidAudio's Parakeet TDT on Core ML / the Neural
//! Engine. macOS only. `dlopen`s `libsmkokoro.dylib` (the SAME shim as the apple-native
//! TTS backend) via `SMKOKORO_DYLIB_PATH`, and transcribes 16 kHz mono f32 PCM → text.
//! Mirrors [`crate::parakeet::ParakeetTranscriber`]'s lazy-load interface
//! (`preload`/`unload`/`transcribe_pcm_16k`) so the helper can hold either behind
//! [`crate::local::LocalTranscriber`].

use std::ffi::{c_char, c_void};

use libloading::{Library, Symbol};

use crate::shim::StrCb;
use crate::streaming::StreamingStt;

// Text-returning calls still BLOCK and return their status; the transcript comes back through a
// borrowed callback (copied out by `crate::shim::collect_str`), so there's no out-param and no
// `smk_free_str`. init/shutdown/start carry no buffer, so they keep the plain int32 ABI.
type AsrInitFn = unsafe extern "C" fn(*const c_char, i32) -> i32;
type TranscribeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type AsrShutdownFn = unsafe extern "C" fn();

// Streaming ASR C ABI (FluidAudio `StreamingEouAsrManager` behind the shim). `start` begins a new
// utterance, `push` feeds a 16 kHz chunk and returns the hypothesis-so-far, `finish` flushes the
// final transcript.
type StreamStartFn = unsafe extern "C" fn(*const c_char) -> i32;
type StreamPushFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
type StreamFinishFn = unsafe extern "C" fn(*mut c_void, StrCb) -> i32;

/// Parakeet ASR behind the C ABI. Models download on first `preload`/transcribe.
pub struct CoremlTranscriber {
    lib: Option<Library>,
    loaded: bool,
}

impl CoremlTranscriber {
    /// Not loaded until the first [`preload`](Self::preload) / transcription.
    pub fn new() -> Self {
        CoremlTranscriber {
            lib: None,
            loaded: false,
        }
    }

    /// Ensure the shim dylib is open (resolves `SMKOKORO_DYLIB_PATH`).
    fn ensure_lib(&mut self) -> Result<(), String> {
        if self.lib.is_none() {
            self.lib = Some(crate::shim::open()?);
        }
        Ok(())
    }

    /// Download (first use) + load the Parakeet models so STT is resident before the
    /// first utterance — the eager counterpart to [`unload`](Self::unload).
    pub fn preload(&mut self) -> Result<(), String> {
        if self.loaded {
            return Ok(());
        }
        self.ensure_lib()?;
        let lib = self.lib.as_ref().expect("lib opened above");
        let rc = unsafe {
            let init: Symbol<AsrInitFn> = lib
                .get(b"smk_asr_init\0")
                .map_err(|e| format!("smk_asr_init symbol: {e}"))?;
            // Our DontSpeak-controlled Core ML cache dir (not "" → FluidAudio's default).
            let dir = crate::shim::model_dir_arg();
            init(dir.as_ptr(), 0)
        };
        if rc != 0 {
            return Err(format!("smk_asr_init failed (rc={rc})"));
        }
        self.loaded = true;
        Ok(())
    }

    /// Free the loaded Parakeet models; the next transcription lazily reloads them.
    pub fn unload(&mut self) -> bool {
        if !self.loaded {
            return false;
        }
        if let Some(lib) = &self.lib {
            // SAFETY: idempotent shim shutdown.
            unsafe {
                if let Ok(sd) = lib.get::<AsrShutdownFn>(b"smk_asr_shutdown\0") {
                    sd();
                }
            }
        }
        self.loaded = false;
        true
    }

    /// Transcribe 16 kHz mono f32 PCM → text. Empty input → empty string.
    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        self.preload()?;
        let lib = self.lib.as_ref().expect("lib loaded above");
        let tr: Symbol<TranscribeFn> = unsafe { lib.get(b"smk_transcribe\0") }
            .map_err(|e| format!("smk_transcribe symbol: {e}"))?;
        // The shim borrows the transcript to our sink, which copies it out (no smk_free_str).
        // The call blocks; `pcm` lives across it.
        crate::shim::collect_str(|ctx, cb| unsafe { tr(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) })
            .map_err(|rc| format!("smk_transcribe failed (rc={rc})"))
    }
}

impl Default for CoremlTranscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CoremlTranscriber {
    fn drop(&mut self) {
        self.unload();
    }
}

/// Cache-aware STREAMING Core ML / ANE backend — FluidAudio's `StreamingAsrManager` behind the
/// shim, implementing the shared [`StreamingStt`] trait so the helper drives it through the SAME
/// [`crate::StreamSession`] + loop as the ONNX path. The shim's `smk_asr_stream_*` ABI is the exact
/// analogue of `OnnxStreamer` (reset/accept/finalize). Loaded eagerly in [`new`](Self::new) so a
/// missing shim/model surfaces as an error → the caller falls back to the offline path.
pub struct CoremlStreamer {
    lib: Library,
    /// The streaming EOU model dir, passed to `smk_asr_stream_start` (consulted on first start).
    model_dir: std::ffi::CString,
}

impl CoremlStreamer {
    /// Open the shim (resolves `SMKOKORO_DYLIB_PATH`). The streaming model loads lazily on the
    /// first [`reset`](StreamingStt::reset) (→ `smk_asr_stream_start`). `Err` (→ offline fallback)
    /// when the shim dylib is unavailable.
    pub fn new() -> Result<Self, String> {
        let lib = crate::shim::open()?;
        Ok(Self {
            lib,
            // The streaming EOU set lives in its OWN subdir (downloaded by the helper via
            // `ds_model::coreml_repo::PARAKEET_EOU_COREML`), NOT flat in `coreml_dir` like the
            // offline model — so point `smk_asr_stream_start` straight at it.
            model_dir: crate::shim::eou_model_dir_arg(),
        })
    }

    fn push(&self, sym: &[u8], pcm: &[f32]) -> Result<String, String> {
        let f: Symbol<StreamPushFn> = unsafe { self.lib.get(sym) }
            .map_err(|e| format!("{} symbol: {e}", String::from_utf8_lossy(sym)))?;
        crate::shim::collect_str(|ctx, cb| unsafe { f(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) })
            .map_err(|rc| format!("{} failed (rc={rc})", String::from_utf8_lossy(sym)))
    }
}

impl StreamingStt for CoremlStreamer {
    fn reset(&mut self) -> Result<(), String> {
        let rc = unsafe {
            let f: Symbol<StreamStartFn> = self
                .lib
                .get(b"smk_asr_stream_start\0")
                .map_err(|e| format!("smk_asr_stream_start symbol: {e}"))?;
            f(self.model_dir.as_ptr())
        };
        if rc != 0 {
            return Err(format!("smk_asr_stream_start failed (rc={rc})"));
        }
        Ok(())
    }

    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        // FluidAudio accumulates internally; an empty chunk is a cheap no-op that just returns the
        // current hypothesis (the shared StreamSession may hand us an empty stable window).
        self.push(b"smk_asr_stream_push\0", pcm_16k)
    }

    fn finalize(&mut self) -> Result<String, String> {
        let f: Symbol<StreamFinishFn> = unsafe { self.lib.get(b"smk_asr_stream_finish\0") }
            .map_err(|e| format!("smk_asr_stream_finish symbol: {e}"))?;
        crate::shim::collect_str(|ctx, cb| unsafe { f(ctx, cb) })
            .map_err(|rc| format!("smk_asr_stream_finish failed (rc={rc})"))
    }
}
