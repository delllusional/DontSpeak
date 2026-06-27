//! ONNX inference over `kokoro-v1.0.onnx` via the `ort` crate (load-dynamic) —
//! the Kokoro `synthesizeBatch` pipeline.
//!
//! Exact I/O parity with kokoro-onnx `_create_audio`:
//!   * "tokens": int64, shape [1, n+2]. padded = [0, ...tokenIds, 0] (0 at both
//!     ends = pad reused as BOS/EOS), so len = tokens.len()+2.
//!   * "style": f32, shape `[1, 256]`. The row is the per-voice 510*256 array
//!     indexed by the UNPADDED token count (before bos/eos) — see
//!     `voices::style_row`.
//!   * "speed": f32, shape `[1]`. Clamped to 0.5..=2.0 (Kokoro `speed`; our
//!     `rate` maps directly, unlike System TTS we do NOT use rate_to_wpm).
//!
//! Output: the first output tensor (`session.outputs[0].name`, matching kokoro-onnx
//! `session.outputNames.first()`), f32 24 kHz mono PCM in `[-1, 1]`, then
//! `trim::trim_silence`. `try_extract_tensor::<f32>()` enforces the f32
//! dtype and yields a flat contiguous slice; we additionally cross-check its
//! length against the reported shape's element count.
//!
//! ONNXRUNTIME, build vs runtime: with the `load-dynamic` feature this module
//! COMPILES on a host with NO onnxruntime present — the dylib is resolved at
//! RUNTIME from `ORT_DYLIB_PATH` (set by the caller to the downloaded
//! libonnxruntime). If it is absent at runtime, [`KokoroSynth::load`] returns an
//! Err and the caller degrades fail-quiet (like the STT "no model" path).
//! Nothing here is exercised by unit tests (no model, no onnxruntime); the pure,
//! tested pieces live in vocab/voices/trim/batch.

use std::collections::HashMap;
use std::sync::Arc;

use ort::session::Session;
use ort::value::Tensor;

use crate::batch::split_phonemes;
use crate::vocab::{MAX_PHONEME_LENGTH, tokenize};

/// A loaded Kokoro ONNX session + parsed per-voice style arrays.
pub struct KokoroSynth {
    session: Session,
    // `Arc<Vec<f32>>`, not `Vec<f32>`: each per-voice style array is ~522 KB (510×256 f32),
    // and `synthesize()` clones the selected voice's array once per streaming batch. Holding
    // it behind an Arc makes that clone a pointer bump instead of a half-megabyte memcpy in
    // the synthesis hot loop. Only a 256-float row is read per forward pass (`style_row`).
    voices: HashMap<String, Arc<Vec<f32>>>,
    output_name: String,
    /// The active execution provider ("CPU" or "CoreML"), for the engine stats.
    provider: &'static str,
}

impl KokoroSynth {
    /// The active ONNX execution provider ("CPU" or "CoreML").
    pub fn provider(&self) -> &'static str {
        self.provider
    }
}

impl KokoroSynth {
    /// Build a session from the model bytes and parse the voices npz. Call
    /// [`ds_model::set_ort_dylib_path`] (or [`ds_model::ensure_ort_dylib`]) first
    /// so `ort` (load-dynamic) can resolve libonnxruntime. Errors (no dylib, bad
    /// model, bad voices) are returned for the caller to fail-quiet.
    pub fn load(model_bytes: &[u8], voices_npz: &[u8]) -> Result<Self, String> {
        // Execution provider from DONTSPEAK_PROVIDER: "ort_cpu" | "ort_cuda" | "ort_coreml"
        // | "auto" (the `ane` token never reaches here — it routes to the native FluidAudio
        // backend in the helper, not KokoroSynth). On Windows `auto` PREFERS CUDA (NVIDIA
        // GPU — 2.8-4.6x faster for Kokoro, validated). On macOS `auto` stays CPU (the ort
        // CoreML EP benchmarked slower).
        let pref = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into());
        match Self::load_with_provider(model_bytes, voices_npz, &pref) {
            Ok(s) => Ok(s),
            // A GPU-preferred session that fails to BUILD (driver/op/version issue)
            // retries once on CPU so TTS never breaks — GPU is a best-effort speedup.
            Err(e) if !pref.eq_ignore_ascii_case("ort_cpu") => {
                eprintln!("dontspeak/synth: provider '{pref}' failed ({e}); falling back to CPU");
                Self::load_with_provider(model_bytes, voices_npz, "ort_cpu")
            }
            Err(e) => Err(e),
        }
    }

    /// Like [`KokoroSynth::load`] but with an EXPLICIT provider — also used by the
    /// CPU fallback above. `provider` records what we actually got (engine stats).
    pub fn load_with_provider(
        model_bytes: &[u8],
        voices_npz: &[u8],
        pref: &str,
    ) -> Result<Self, String> {
        // Wrap each style array in an Arc so `synthesize()` clones a pointer, not the buffer.
        let voices: HashMap<String, Arc<Vec<f32>>> = crate::voices::parse_voices_npz(voices_npz)?
            .into_iter()
            .map(|(name, style)| (name, Arc::new(style)))
            .collect();
        let mut provider = "CPU";
        let cpu_builder = || Session::builder().map_err(|e| format!("ort session builder: {e}"));

        // Windows `auto`/`cuda` → CUDA (NVIDIA GPU), best-effort with CPU fallback.
        // ort's builder methods return the builder INSIDE their error (for recovery),
        // so chain them with `?` in a closure that yields ort::Result.
        #[cfg(target_os = "windows")]
        let mut builder = {
            use ort::execution_providers::CUDAExecutionProvider;
            let want_gpu =
                pref.eq_ignore_ascii_case("auto") || pref.eq_ignore_ascii_case("ort_cuda");
            let gpu = if want_gpu {
                (|| -> ort::Result<_> {
                    let b = Session::builder()?;
                    Ok(b.with_execution_providers([CUDAExecutionProvider::default().build()])?)
                })()
                .ok()
            } else {
                None
            };
            match gpu {
                Some(b) => {
                    provider = "CUDA";
                    b
                }
                None => cpu_builder()?,
            }
        };

        #[cfg(target_os = "macos")]
        let mut builder = {
            use ort::execution_providers::CoreMLExecutionProvider;
            // FULL-DUPLEX prefers CoreML even for "auto": running Kokoro on the CPU
            // saturates the cores and starves VPIO's real-time render thread, which
            // CHOPS the playback. CoreML keeps the cores free → smooth (verified
            // on-device; it benches slightly slower, which is why "auto" picks CPU
            // in the half-duplex/rodio path that isn't sensitive to this).
            let full_duplex = std::env::var_os("DONTSPEAK_FULL_DUPLEX").is_some();
            let want_coreml = pref.eq_ignore_ascii_case("ort_coreml")
                || (full_duplex && pref.eq_ignore_ascii_case("auto"));
            let gpu = if want_coreml {
                (|| -> ort::Result<_> {
                    let b = Session::builder()?;
                    Ok(b.with_execution_providers([CoreMLExecutionProvider::default().build()])?)
                })()
                .ok()
            } else {
                None
            };
            match gpu {
                Some(b) => {
                    provider = "CoreML";
                    b
                }
                None => cpu_builder()?,
            }
        };

        // Linux x86_64 `auto`/`cuda` → CUDA (NVIDIA GPU), best-effort with CPU fallback — the
        // SAME structure as the Windows block above (one CUDA-enabled build for every box). On
        // a machine without the GPU runtime/driver, ORT_DYLIB_PATH is the CPU dylib, so the
        // CUDA EP fails to register and we fall through to CPU.
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        let mut builder = {
            use ort::execution_providers::CUDAExecutionProvider;
            let want_gpu =
                pref.eq_ignore_ascii_case("auto") || pref.eq_ignore_ascii_case("ort_cuda");
            let gpu = if want_gpu {
                (|| -> ort::Result<_> {
                    let b = Session::builder()?;
                    Ok(b.with_execution_providers([CUDAExecutionProvider::default().build()])?)
                })()
                .ok()
            } else {
                None
            };
            match gpu {
                Some(b) => {
                    provider = "CUDA";
                    b
                }
                None => cpu_builder()?,
            }
        };

        // Other Unix (non-x86_64 Linux, BSD): CPU only.
        #[cfg(all(
            not(any(target_os = "windows", target_os = "macos")),
            not(all(target_os = "linux", target_arch = "x86_64"))
        ))]
        let mut builder = {
            let _ = &pref;
            cpu_builder()?
        };

        // Full-duplex on CPU: keep Kokoro off the CoreAudio REAL-TIME render thread
        // (VPIO), or the speech chops/stutters. Two parts, per Apple's audio-glitch
        // guidance + the ONNX Runtime threading docs:
        //   • CAP intra-op threads, leaving ≥2 cores for the audio IO thread; and
        //   • DISABLE ORT thread SPINNING — by default ORT's idle inference threads
        //     busy-wait, pinning every core even between forwards and starving the
        //     render thread (the actual smoking gun: chops even when synth ≫ realtime
        //     and the ring is huge, because it's deadline jitter, not throughput).
        // (Half-duplex uses rodio, which buffers; CoreML/CUDA offload off the CPU.)
        #[cfg(target_os = "macos")]
        if provider == "CPU" && std::env::var_os("DONTSPEAK_FULL_DUPLEX").is_some() {
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
        let style = self
            .voices
            .get(voice)
            .ok_or_else(|| format!("unknown voice '{voice}'"))?
            .clone();
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
