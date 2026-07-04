//! Speaker SEPARATION for the dictation speaker-lock — the "talk over a YouTube video"
//! case that frame-gating (diarization) can't solve.
//!
//! A pretrained SepFormer (wsj0-2mix, 8 kHz, 2 sources), exported to int8 ONNX, splits a
//! single-mic mixture into its constituent voices. The caller then embeds each stream with
//! the existing WeSpeaker diarizer and keeps the one matching the enrolled user, so the
//! co-channel background voice is removed (not merely gated) before Parakeet transcribes.
//!
//! The model is fixed at 8 kHz mono; capture is 16 kHz, so we resample 16 k → 8 k in and
//! 8 k → 16 k out (via [`crate::resample`]). Runs on the shared `ort` runtime — CoreML EP
//! on macOS, CPU elsewhere — exactly like the Kokoro synth session.

use ort::session::Session;
use ort::value::Tensor;

/// The separator's native sample rate (wsj0-2mix SepFormer is 8 kHz).
const SEP_RATE: u32 = 8_000;

/// A loaded speaker-separation model. One `ort` session; `separate_16k` is the whole API.
pub struct Separator {
    session: Session,
    /// The model's single output name (resolved at load), so `run` extraction indexes it
    /// directly instead of borrowing through a temporary output iterator.
    output_name: String,
    /// Active execution provider ("CoreML" / "CPU"), for logging.
    provider: &'static str,
}

impl Separator {
    /// Load the int8 SepFormer ONNX at `model_path` on the CPU execution provider.
    ///
    /// DELIBERATELY CPU, not CoreML: the separator has a DYNAMIC time axis (variable
    /// utterance length), and the CoreML EP recompiles the model for every new input
    /// length — measured at >120 s per call on-device (effectively a hang). The CPU EP
    /// handles dynamic shapes natively and benches at RTF ~0.4 (a 7 s utterance separates
    /// in ~3 s) — and dictation separation is OFFLINE (record-then-submit), so that's
    /// plenty. (The Kokoro/Parakeet models keep CoreML because their shapes are static.)
    pub fn load(model_path: &std::path::Path) -> Result<Self, String> {
        // Ensure the onnxruntime dylib is resolved + `ORT_DYLIB_PATH` set BEFORE the first
        // session build. On Apple Silicon, TTS/STT run on Core ML (FluidAudio), so NOTHING
        // else initializes onnxruntime — the separator is the only ort user, and without
        // this the load-dynamic `ort` has no dylib to dlopen. Prefers an already-set path
        // (the bundled dylib in a dist build), else resolves the downloaded copy.
        ds_model::ensure_ort_dylib()?;
        let provider = "CPU";
        let mut builder = Session::builder().map_err(|e| format!("ort session builder: {e}"))?;
        // DISABLE graph optimization: onnxruntime 1.24's optimizer hangs (does not return)
        // while loading this SepFormer graph — both fp32 and int8, isolated from the engine.
        // Level-0 (no optimization) loads in well under a second and runs fine; the model
        // was already constant-folded at export, so we lose little. (Kokoro/Parakeet keep
        // full optimization — only this transformer graph trips the 1.24 optimizer.)
        use ort::session::builder::GraphOptimizationLevel;
        builder = builder
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .map_err(|e| format!("ort opt level: {e}"))?;
        // Single-threaded, no spinning: onnxruntime 1.24's intra-op thread pool deadlocks
        // on a dispatch semaphore while LOADING this graph (sampled: blocked in
        // semaphore_wait at 0 % CPU). Forcing one intra-op thread + disabling spin sidesteps
        // the pool-init deadlock; separation is offline so single-thread throughput is fine.
        builder = builder
            .with_intra_threads(1)
            .map_err(|e| format!("ort intra threads: {e}"))?;
        builder = builder
            .with_config_entry("session.intra_op.allow_spinning", "0")
            .map_err(|e| format!("ort intra spinning: {e}"))?;
        // Read the model bytes and `commit_from_memory` (NOT `commit_from_file`): the latter
        // deadlocks under ort 2.0-rc + load-dynamic on macOS, mirroring the Kokoro synth
        // session which loads from memory for the same reason.
        let model_bytes = std::fs::read(model_path)
            .map_err(|e| format!("read separator {}: {e}", model_path.display()))?;
        let session = builder
            .commit_from_memory(&model_bytes)
            .map_err(|e| format!("ort load separator {}: {e}", model_path.display()))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| "separator model has no outputs".to_string())?;
        Ok(Self {
            session,
            output_name,
            provider,
        })
    }

    /// The active execution provider ("CoreML" / "CPU").
    pub fn provider(&self) -> &'static str {
        self.provider
    }

    /// Separate a 16 kHz mono mixture into its constituent voices, each 16 kHz mono.
    /// Resamples to the model's 8 kHz, runs the net, splits the `[1, T, n_src]` output
    /// into per-source channels, and resamples each back to 16 kHz. `Err` on any model
    /// error (the caller fails OPEN — transcribes the mixture unfiltered).
    pub fn separate_16k(&mut self, pcm_16k: &[f32]) -> Result<Vec<Vec<f32>>, String> {
        if pcm_16k.is_empty() {
            return Ok(Vec::new());
        }
        let mix8 = crate::resample(pcm_16k, 16_000, SEP_RATE);
        let n = mix8.len();
        let input = Tensor::from_array((vec![1_i64, n as i64], mix8))
            .map_err(|e| format!("separator input tensor: {e}"))?;
        let outputs = self
            .session
            .run(ort::inputs! { "mix" => input })
            .map_err(|e| format!("separator run: {e}"))?;
        // Single output: [1, T, n_src] interleaved by source along the last axis.
        let (shape, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("separator extract: {e}"))?;
        let dims: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
        let (t, src) = match dims.as_slice() {
            [1, t, src] => (*t, *src),
            other => return Err(format!("unexpected separator output shape {other:?}")),
        };
        if src == 0 || t == 0 || data.len() < t * src {
            return Err(format!(
                "separator output too small: shape {dims:?} vs {} samples",
                data.len()
            ));
        }
        // De-interleave [T, src] → src channels, then resample each 8 k → 16 k.
        let mut streams = Vec::with_capacity(src);
        for s in 0..src {
            let chan: Vec<f32> = (0..t).map(|i| data[i * src + s]).collect();
            streams.push(crate::resample(&chan, SEP_RATE, 16_000));
        }
        Ok(streams)
    }
}
