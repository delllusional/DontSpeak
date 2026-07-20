//! ONNX inference over `kokoro-v1.0.onnx` via `ort` (load-dynamic) — `synthesizeBatch`.
//!
//! I/O parity with kokoro-onnx `_create_audio`:
//!   * "tokens": int64 [1, n+2], padded `[0, …ids, 0]` (pad = BOS/EOS)
//!   * "style": f32 [1, 256] — voice 510*256 row by UNPADDED token count (`style_row`)
//!   * "speed": f32 `[1]`, clamped 0.5..=2.0 (`rate` maps directly; not rate_to_wpm)
//!
//! Output: first tensor, f32 24 kHz mono `[-1, 1]`, then `trim_silence`.
//!
//! load-dynamic: compiles without onnxruntime; dylib from `ORT_DYLIB_PATH` at runtime.
//! Missing dylib → `load` Err → fail-quiet. Sessions not unit-tested (no model); pure
//! stages in vocab/voices/trim/batch + `style_for_voice` below.

use std::collections::HashMap;
use std::sync::Arc;

use ort::session::Session;
use ort::value::Tensor;

use crate::batch::split_phonemes;
use crate::vocab::{MAX_PHONEME_LENGTH, tokenize};

/// Loaded Kokoro ONNX session + per-voice style arrays.
pub struct KokoroSynth {
    session: Session,
    // Arc: ~522 KB style arrays; clone per batch is a pointer. Forward reads 256 floats.
    voices: HashMap<String, Arc<Vec<f32>>>,
    output_name: String,
    /// Realized EP for stats / child's `PROVIDER` (shared type with STT).
    provider: ds_config::RealizedProvider,
}

impl KokoroSynth {
    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.provider
    }
}

impl KokoroSynth {
    /// Session from model bytes + voices npz. Call [`ds_model::ensure_ort_dylib`]
    /// (or set path) first. Errors for caller fail-quiet.
    pub fn load(model_bytes: &[u8], voices_npz: &[u8]) -> Result<Self, String> {
        crate::ort_session::load_with_fallback("synth", |preference| {
            Self::load_with_provider(model_bytes, voices_npz, preference)
        })
    }

    /// Like [`KokoroSynth::load`] but with an EXPLICIT provider — also used by the
    /// CPU fallback above. `provider` records what we actually got (engine stats).
    pub fn load_with_provider(
        model_bytes: &[u8],
        voices_npz: &[u8],
        pref: &str,
    ) -> Result<Self, String> {
        let voices: HashMap<String, Arc<Vec<f32>>> = crate::voices::parse_voices_npz(voices_npz)?
            .into_iter()
            .map(|(name, style)| (name, Arc::new(style)))
            .collect();
        let mut sessions =
            crate::ort_session::OrtSessions::from_preference(ds_config::TtsModel::Kokoro, pref);
        let (mut builder, provider) = sessions.builder()?;

        // Full-duplex on CPU: keep Kokoro off the CoreAudio REAL-TIME render thread
        // (VPIO), or the speech chops/stutters. Two parts, per Apple's audio-glitch
        // guidance + the ONNX Runtime threading docs:
        //   • CAP intra-op threads, leaving ≥2 cores for the audio IO thread; and
        //   • DISABLE ORT thread SPINNING — by default ORT's idle inference threads
        //     busy-wait, pinning every core even between forwards and starving the
        //     render thread (the actual smoking gun: chops even when synth ≫ realtime
        //     and the ring is huge, because it's deadline jitter, not throughput).
        // (Half-duplex uses rodio, which buffers; the MLX path bypasses this session.)
        #[cfg(target_os = "macos")]
        if provider == ds_config::RealizedProvider::Cpu
            && std::env::var_os("DONTSPEAK_FULL_DUPLEX").is_some()
        {
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            let cap = cores.saturating_sub(2).max(1);
            builder = builder
                .with_intra_threads(cap)
                .map_err(|e| format!("ort intra threads: {e}"))?;
            builder = builder
                .with_config_entry("session.intra_op.allow_spinning", "0")
                .map_err(|e| format!("ort intra spinning: {e}"))?;
            builder = builder
                .with_config_entry("session.inter_op.allow_spinning", "0")
                .map_err(|e| format!("ort inter spinning: {e}"))?;
        }

        let session = builder
            .commit_from_memory(model_bytes)
            .map_err(|e| format!("ort load model: {e}"))?;
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| "model has no outputs".to_string())?;
        Ok(Self {
            session,
            voices,
            output_name,
            provider,
        })
    }

    /// The available voice names (sorted), for a picker.
    pub fn voice_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.voices.keys().cloned().collect();
        v.sort();
        v
    }

    /// Synthesize a whole utterance: phoneme string → trimmed 24 kHz mono PCM,
    /// batching at sentence marks (`split_phonemes`) and concatenating. `voice`
    /// must be a key from the voices file; `speed` is clamped to [0.5, 2.0].
    pub fn synthesize(
        &mut self,
        phonemes: &str,
        voice: &str,
        speed: f32,
    ) -> Result<Vec<f32>, String> {
        let style = style_for_voice(&self.voices, voice)?;
        let speed = speed.clamp(0.5, 2.0);
        let mut audio: Vec<f32> = Vec::new();
        for batch in split_phonemes(phonemes) {
            let part = self.synthesize_batch(&batch, &style, speed)?;
            audio.extend_from_slice(&part);
        }
        Ok(audio)
    }

    /// One phoneme batch → trimmed PCM (the Kokoro synthesize step).
    fn synthesize_batch(
        &mut self,
        batch: &str,
        style: &[f32],
        speed: f32,
    ) -> Result<Vec<f32>, String> {
        // Truncate to the model context, then tokenize (unknown chars dropped).
        let phonemes: String = batch.chars().take(MAX_PHONEME_LENGTH).collect();
        let tokens = tokenize(&phonemes);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        // Style row indexed by the UNPADDED token count (kokoro-onnx parity).
        let style_row = crate::voices::style_row(style, tokens.len())?;

        // padded = [0, ...tokens, 0] (pad/BOS/EOS at both ends).
        let mut padded: Vec<i64> = Vec::with_capacity(tokens.len() + 2);
        padded.push(0);
        padded.extend_from_slice(&tokens);
        padded.push(0);

        let tokens_t = Tensor::from_array((vec![1_i64, padded.len() as i64], padded))
            .map_err(|e| format!("tokens tensor: {e}"))?;
        let style_t = Tensor::from_array((vec![1_i64, 256], style_row))
            .map_err(|e| format!("style tensor: {e}"))?;
        let speed_t = Tensor::from_array((vec![1_i64], vec![speed]))
            .map_err(|e| format!("speed tensor: {e}"))?;

        let outputs = self
            .session
            .run(ort::inputs! {
                "tokens" => tokens_t,
                "style" => style_t,
                "speed" => speed_t,
            })
            .map_err(|e| format!("ort run: {e}"))?;

        // `try_extract_tensor::<f32>()` validates the dtype is f32 and returns a
        // flat, C-contiguous `&[f32]`, so `data` goes straight to `trim_silence`.
        // The length cross-check guards against a future model whose output is
        // multi-dimensional (kokoro-onnx emits 1-D mono PCM, shape `[n_samples]`).
        let (shape, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract output: {e}"))?;
        if shape.num_elements() != data.len() {
            return Err(format!(
                "unexpected output tensor: shape {shape:?} ({} elems) vs {} samples",
                shape.num_elements(),
                data.len()
            ));
        }
        Ok(crate::trim::trim_silence(data))
    }
}

/// Style lookup with a registry-default fallback: a stale pool id (config voice pool
/// survives a model switch; ds-config cannot see the on-disk voices) must not drop the
/// utterance. Safe at synth time — the Kokoro frontend ignores the voice id. Err only
/// when the fallback is absent from the loaded voices too.
fn style_for_voice(
    voices: &HashMap<String, Arc<Vec<f32>>>,
    voice: &str,
) -> Result<Arc<Vec<f32>>, String> {
    if let Some(style) = voices.get(voice) {
        return Ok(style.clone());
    }
    let fallback = ds_config::TtsModel::Kokoro.descriptor().voices[0];
    match voices.get(fallback) {
        Some(style) => {
            log::warn!(target: "tts", "unknown voice '{voice}'; falling back to '{fallback}'");
            Ok(style.clone())
        }
        None => Err(format!("unknown voice '{voice}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(names: &[&str]) -> HashMap<String, Arc<Vec<f32>>> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.to_string(), Arc::new(vec![i as f32])))
            .collect()
    }

    #[test]
    fn unknown_voice_falls_back_to_the_registry_default() {
        let voices = map(&["af_sarah", "bf_emma"]);
        assert_eq!(*style_for_voice(&voices, "bf_emma").unwrap(), vec![1.0]);
        // Stale pool id → the registry default's style, not an error.
        assert_eq!(*style_for_voice(&voices, "xx_gone").unwrap(), vec![0.0]);
        // Fallback missing too → Err (nothing sensible to synthesize with).
        let no_default = map(&["bf_emma"]);
        assert!(style_for_voice(&no_default, "xx_gone").is_err());
    }
}
