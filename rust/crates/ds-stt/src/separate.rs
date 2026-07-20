//! Speaker SEPARATION for speaker-lock — "talk over YouTube" that frame-gating
//! (diarization) can't solve.
//!
//! SepFormer (wsj0-2mix, 8 kHz, 2 sources, int8 ONNX) splits a single-mic mixture.
//! Caller embeds each stream with WeSpeaker and keeps the enrolled match so background
//! is removed (not merely gated) before Parakeet. Capture is 16 kHz — resample via
//! [`crate::resample`].

use ort::session::Session;
use ort::value::Tensor;

/// Separator native rate (wsj0-2mix SepFormer).
const SEP_RATE: u32 = 8_000;

/// Loaded separation model. One ort session; `separate_16k` is the whole API.
pub struct Separator {
    session: Session,
    input_name: String,
    /// Resolved at load so `run` indexes by name without temp iterators.
    output_name: String,
    provider: &'static str,
}

impl Separator {
    /// Load int8 SepFormer on CPU.
    ///
    /// The dynamic time axis is CPU-only. CPU handles dynamic shapes at roughly 0.4 RTF;
    /// separation is offline.
    pub fn load(model_path: &std::path::Path) -> Result<Self, String> {
        // Resolve the ORT dylib before the first session. Apple Silicon TTS/STT use MLX,
        // so separator may be the only ort user — without this load-dynamic has nothing
        // to dlopen.
        ds_model::ensure_ort_dylib()?;
        let provider = "CPU";
        let mut builder = Session::builder().map_err(|e| format!("ort session builder: {e}"))?;
        // DISABLE graph opt: ort 1.24 optimizer hangs on this SepFormer graph.
        // Level-0 loads fast; model already constant-folded at export.
        use ort::session::builder::GraphOptimizationLevel;
        builder = builder
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .map_err(|e| format!("ort opt level: {e}"))?;
        // Single-thread + no spin: ort 1.24 intra-op pool deadlocks on dispatch
        // semaphore while LOADING this graph. Offline so throughput is fine.
        builder = builder
            .with_intra_threads(1)
            .map_err(|e| format!("ort intra threads: {e}"))?;
        builder = builder
            .with_config_entry("session.intra_op.allow_spinning", "0")
            .map_err(|e| format!("ort intra spinning: {e}"))?;
        // commit_from_memory (NOT commit_from_file): latter deadlocks under ort 2.0-rc
        // + load-dynamic on macOS (same as Kokoro synth).
        let model_bytes =
            ds_model::read_model_file(model_path).map_err(|e| format!("read separator: {e}"))?;
        let session = builder
            .commit_from_memory(&model_bytes)
            .map_err(|e| format!("ort load separator {}: {e}", model_path.display()))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| "separator model has no outputs".to_string())?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| "separator model has no inputs".to_string())?;
        Ok(Self {
            session,
            input_name,
            output_name,
            provider,
        })
    }

    pub fn provider(&self) -> &'static str {
        self.provider
    }

    /// 16 kHz mono mixture → per-source 16 kHz mono. Resample 16→8, run, split
    /// `[1, T, n_src]`, resample 8→16. `Err` → caller fails open (unfiltered mixture).
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
            .run(ort::inputs! { self.input_name.as_str() => input })
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
        if src == 0 || t == 0 {
            return Err(format!(
                "separator output has zero dimension: shape {dims:?}"
            ));
        }
        let expected = t
            .checked_mul(src)
            .ok_or_else(|| format!("separator output dims overflow: t={t}, src={src}"))?;
        if data.len() < expected {
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
