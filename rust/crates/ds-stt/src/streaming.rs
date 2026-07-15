//! Cache-aware STREAMING Parakeet/FastConformer transducer over `ort`.
//!
//! The offline [`crate::parakeet`] path re-encodes the whole buffer on every preview tick
//! (`transcribe-rs` `ParakeetModel`, `supports_streaming: false`). This module instead feeds
//! audio to a *cache-aware* NeMo FastConformer encoder in fixed chunks, threading the encoder
//! cache so each frame is encoded EXACTLY ONCE — prototyped/validated in `scripts/streaming-stt/`.
//!
//! Model: `sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-*` (encoder + decoder(LSTM) +
//! joiner ONNX + `tokens.txt`). Tensor contract, metadata keys and the greedy-decode logic are
//! mirrored from the validated Python reference; see `scripts/streaming-stt/README.md`.
//!
//! Feature extraction is kaldi log-mel fbank (80 bins, 25/10 ms, dither 0, snip_edges false,
//! `use_energy` off) over the waveform in [-1, 1] — NO 32768 scaling, NO CMVN. This exactly
//! reproduces the reference; the wrong scaling/normalization yields all-blank output.

use std::path::Path;
use std::time::Instant;

use kaldi_native_fbank::fbank::{FbankComputer, FbankOptions};
use kaldi_native_fbank::online::{FeatureComputer, OnlineFeature};
use ort::session::Session;
use ort::value::Tensor;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler};

/// One decoder step's result: (decoder_out column, next LSTM `h`, next LSTM `c`).
type DecoderStep = (Vec<f32>, Vec<f32>, Vec<f32>);

/// Number of mel bins the encoder expects (`audio_signal` channel dim).
const MEL_BINS: usize = 80;
/// Greedy decode cap: max non-blank symbols emitted per encoder output frame.
const MAX_SYMBOLS_PER_FRAME: usize = 10;

/// Encoder metadata (read at load — never hardcode; the 80/480/1040 ms variants differ).
struct Meta {
    window_size: usize, // feature frames fed per encoder step
    chunk_shift: usize, // feature frames advanced per step (overlap = window - shift)
    blank_id: i32,      // = vocab_size; tokens.txt has vocab_size + 1 entries
    pred_hidden: usize, // decoder LSTM hidden size (state dim)
    pred_layers: usize, // decoder LSTM layers (state dim 0)
    c1: [i64; 4],       // cache_last_channel shape [1, d1, d2, d3]
    c2: [i64; 4],       // cache_last_time shape    [1, d1, d2, d3]
}

/// A loaded streaming model: the three `ort` sessions + parsed metadata + token table.
pub struct StreamingModel {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    /// Decoder output names (index 2/3 are the next LSTM states; index-3's name is unstable).
    dec_out_names: Vec<String>,
    /// Decoder input names in semantic export order: targets, length, h, c.
    dec_in_names: Vec<String>,
    meta: Meta,
    tokens: Vec<String>,
    /// The REALIZED ort execution provider — what the sessions ACTUALLY loaded on, CPU fallback
    /// included. Reported up (via `STT_PROVIDER`) so the STT status row shows the same realized
    /// token TTS does, from the same [`ds_model::cuda_session_builder`] path. Shared type.
    provider: ds_config::RealizedProvider,
}

/// Per-utterance streaming state: feature buffer, encoder cache, decoder LSTM state, and the
/// hypothesis so far. One per dictation; `StreamingModel::new_state` seeds it.
pub struct StreamingState {
    fbank: OnlineFeature,
    feat_off: usize, // feature frames already consumed by an encoder step
    cache1: Vec<f32>,
    cache2: Vec<f32>,
    cache_len: i64,
    dec_out: Vec<f32>, // [pred_hidden] current decoder output column
    h: Vec<f32>,       // [pred_layers, 1, pred_hidden]
    c: Vec<f32>,
    hyp: Vec<i32>,
    transcribe_ms: f64, // cumulative encoder+decode model time (for STTSTATS)
}

fn meta_str(s: &Session, key: &str) -> Option<String> {
    s.metadata().ok().and_then(|m| m.custom(key))
}
fn meta_usize(s: &Session, key: &str, default: usize) -> usize {
    meta_str(s, key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// Build one streaming session for `path` via the ONE GPU-aware builder shared with Kokoro TTS
/// ([`ds_model::cuda_session_builder`]): registers the CUDA EP when `want_gpu` (the shared
/// `provider` ladder resolved STT to `cuda`, carried in `DONTSPEAK_STT_PROVIDER`), best-effort with
/// a silent CPU fallback. Returns the session AND the realized [`RealizedProvider`] — the same
/// shared type Kokoro reports, so STT and TTS report through ONE path and can't drift. (int8 ops
/// ORT can't place on CUDA fall back to CPU per-op; the graph parts it can offload run on GPU.)
fn build(path: &Path, want_gpu: bool) -> Result<(Session, ds_config::RealizedProvider), String> {
    let bytes = ds_model::read_model_file(path)?;
    let (mut builder, realized) = ds_model::cuda_session_builder(want_gpu)?;
    let session = builder
        .commit_from_memory(&bytes)
        .map_err(|e| format!("ort load {}: {e}", path.display()))?;
    Ok((session, realized))
}

impl StreamingModel {
    /// Load the encoder/decoder/joiner ONNX (int8 by default) + `tokens.txt` from `dir`, honoring
    /// the resolved STT provider (`DONTSPEAK_STT_PROVIDER`). Mirrors Kokoro TTS's CPU RETRY: if the
    /// GPU load fails (e.g. a device-init failure that surfaces at session-commit, Win32 1114), it
    /// retries on CPU so dictation never dies where CPU would have worked.
    pub fn load(dir: &Path, int8: bool) -> Result<Self, String> {
        let want_gpu = ds_config::provider_pref_wants_gpu(
            &std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default(),
        );
        Self::load_with_gpu_preference(dir, int8, want_gpu)
    }

    /// Load using an explicit resolved provider token. In-process users cannot rely on
    /// the helper child's environment contract.
    pub fn load_for_provider(dir: &Path, int8: bool, provider: &str) -> Result<Self, String> {
        Self::load_with_gpu_preference(dir, int8, ds_config::provider_pref_wants_gpu(provider))
    }

    fn load_with_gpu_preference(dir: &Path, int8: bool, want_gpu: bool) -> Result<Self, String> {
        match Self::load_on(dir, int8, want_gpu) {
            Ok(m) => Ok(m),
            Err(e) if want_gpu => {
                eprintln!("dontspeak/helper: STT GPU load failed ({e}); retrying on CPU");
                Self::load_on(dir, int8, false)
            }
            Err(e) => Err(e),
        }
    }

    /// The actual load, on the EXPLICIT `want_gpu`. Routes the ort bootstrap through the GPU-aware
    /// entry so STT and TTS pick the SAME onnxruntime (first engine to warm wins the shared runtime).
    fn load_on(dir: &Path, int8: bool, want_gpu: bool) -> Result<Self, String> {
        ds_model::ensure_ort_dylib_gpu(want_gpu)?;
        let sfx = if int8 { ".int8" } else { "" };
        // All three sessions build over the SAME shared ort runtime, so they realize the same EP;
        // the encoder's is representative.
        let (encoder, provider) = build(&dir.join(format!("encoder{sfx}.onnx")), want_gpu)?;
        let (decoder, _) = build(&dir.join(format!("decoder{sfx}.onnx")), want_gpu)?;
        let (joiner, _) = build(&dir.join(format!("joiner{sfx}.onnx")), want_gpu)?;

        if encoder.outputs().len() < 5 {
            return Err(format!(
                "encoder has {} outputs, need >= 5",
                encoder.outputs().len()
            ));
        }

        let vocab = meta_usize(&encoder, "vocab_size", 1024);
        let window_size = meta_usize(&encoder, "window_size", 65);
        let chunk_shift = meta_usize(&encoder, "chunk_shift", 56);
        if chunk_shift == 0 || window_size == 0 {
            return Err(format!(
                "invalid streaming meta: window={window_size}, shift={chunk_shift} (must be > 0)"
            ));
        }
        let meta = Meta {
            window_size,
            chunk_shift,
            blank_id: vocab as i32,
            pred_hidden: meta_usize(&encoder, "pred_hidden", 640),
            pred_layers: meta_usize(&encoder, "pred_rnn_layers", 1),
            c1: [
                1,
                meta_usize(&encoder, "cache_last_channel_dim1", 17) as i64,
                meta_usize(&encoder, "cache_last_channel_dim2", 70) as i64,
                meta_usize(&encoder, "cache_last_channel_dim3", 512) as i64,
            ],
            c2: [
                1,
                meta_usize(&encoder, "cache_last_time_dim1", 17) as i64,
                meta_usize(&encoder, "cache_last_time_dim2", 512) as i64,
                meta_usize(&encoder, "cache_last_time_dim3", 8) as i64,
            ],
        };
        let dec_out_names: Vec<String> = decoder
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        if dec_out_names.len() < 4 {
            return Err(format!(
                "decoder has {} outputs, need >= 4",
                dec_out_names.len()
            ));
        }
        let dec_in_names: Vec<String> = decoder
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        if dec_in_names.len() < 4 {
            return Err(format!(
                "decoder has {} inputs, need >= 4",
                dec_in_names.len()
            ));
        }

        let tokens_path = dir.join(ds_model::PARAKEET_TOKENS_FILE);
        let tokens = parse_tokens(&ds_model::read_model_file_to_string(&tokens_path)?)?;
        if tokens.len() <= vocab {
            return Err(format!(
                "tokens.txt has {} entries, need > vocab_size {vocab}",
                tokens.len()
            ));
        }
        Ok(Self {
            encoder,
            decoder,
            joiner,
            dec_out_names,
            dec_in_names,
            meta,
            tokens,
            provider,
        })
    }

    /// The REALIZED ort execution provider these sessions loaded on.
    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.provider
    }

    /// Build the kaldi log-mel fbank matching the reference (Slaney mel default; only `use_energy`
    /// is overridden off, plus dither 0 / snip_edges off / 80 bins).
    fn new_fbank() -> Result<OnlineFeature, String> {
        let mut opts = FbankOptions::default();
        opts.frame_opts.samp_freq = 16_000.0;
        opts.frame_opts.dither = 0.0;
        opts.frame_opts.snip_edges = false;
        opts.mel_opts.num_bins = MEL_BINS;
        opts.use_energy = false;
        let comp = FbankComputer::new(opts).map_err(|e| format!("fbank init: {e}"))?;
        Ok(OnlineFeature::new(FeatureComputer::Fbank(comp)))
    }

    /// Start a new dictation: zeroed encoder cache + decoder LSTM state, seeded by one decoder
    /// step on the blank/SOS token (mirrors the reference). Audio fed to [`accept_16k`](Self::accept_16k)
    /// must already be 16 kHz mono (the device-rate → 16 kHz resample lives in [`StreamSession`]).
    pub fn new_state(&mut self) -> Result<StreamingState, String> {
        // Copy the (Copy) metadata out so the &self.meta borrow doesn't span the &mut run_decoder.
        let (blank_id, state_len, c1, c2) = {
            let m = &self.meta;
            (m.blank_id, m.pred_layers * m.pred_hidden, m.c1, m.c2)
        };
        let fbank = Self::new_fbank()?;
        let (dec_out, h, c) =
            self.run_decoder(blank_id, vec![0.0f32; state_len], vec![0.0f32; state_len])?;
        Ok(StreamingState {
            fbank,
            feat_off: 0,
            cache1: vec![0.0f32; (c1[1] * c1[2] * c1[3]) as usize],
            cache2: vec![0.0f32; (c2[1] * c2[2] * c2[3]) as usize],
            cache_len: 0,
            dec_out,
            h,
            c,
            hyp: Vec::new(),
            transcribe_ms: 0.0,
        })
    }

    /// Feed a chunk of 16 kHz mono PCM into the fbank, run any newly-available encoder windows,
    /// and return the hypothesis text so far. Empty input just returns the current hypothesis.
    pub fn accept_16k(
        &mut self,
        state: &mut StreamingState,
        pcm_16k: &[f32],
    ) -> Result<String, String> {
        if !pcm_16k.is_empty() {
            state.fbank.accept_waveform(16_000.0, pcm_16k);
            self.drain_windows(state, false)?;
        }
        Ok(self.text(state))
    }

    /// Flush: run the remaining (zero-padded) windows and return the final text.
    pub fn finalize(&mut self, state: &mut StreamingState) -> Result<String, String> {
        state.fbank.input_finished();
        self.drain_windows(state, true)?;
        Ok(self.text(state))
    }

    /// Run encoder steps while a full `window_size` of features is available (or, on `flush`, pad
    /// the final partial window). Each step advances `feat_off` by `chunk_shift`.
    fn drain_windows(&mut self, state: &mut StreamingState, flush: bool) -> Result<(), String> {
        let (window, shift) = (self.meta.window_size, self.meta.chunk_shift);
        loop {
            let ready = state.fbank.num_frames_ready();
            let have = ready.saturating_sub(state.feat_off);
            if have == 0 || (!flush && have < window) {
                break;
            }
            // Gather `window` feature frames (channel-major [80, window]); zero-pad on flush.
            let mut audio = vec![0.0f32; MEL_BINS * window];
            for i in 0..window {
                let fi = state.feat_off + i;
                if fi >= ready {
                    break;
                }
                let frame = state
                    .fbank
                    .get_frame(fi)
                    .ok_or_else(|| format!("fbank frame {fi} missing"))?;
                for (ch, &v) in frame.iter().enumerate().take(MEL_BINS) {
                    audio[ch * window + i] = v;
                }
            }
            self.run_encoder_step(state, &audio)?;
            state.feat_off += shift;
            if flush && have <= window {
                break;
            }
        }
        Ok(())
    }

    /// One encoder forward over a `[1, 80, window]` feature block: thread the 3 cache tensors and
    /// greedily decode every output column.
    fn run_encoder_step(
        &mut self,
        state: &mut StreamingState,
        audio: &[f32],
    ) -> Result<(), String> {
        let t0 = Instant::now();
        let m = &self.meta;
        let window = m.window_size as i64;
        let audio_t = Tensor::from_array((vec![1i64, MEL_BINS as i64, window], audio.to_vec()))
            .map_err(|e| format!("audio tensor: {e}"))?;
        let len_t = Tensor::from_array((vec![1i64], vec![window]))
            .map_err(|e| format!("length tensor: {e}"))?;
        let c1_t = Tensor::from_array((m.c1.to_vec(), state.cache1.clone()))
            .map_err(|e| format!("cache1 tensor: {e}"))?;
        let c2_t = Tensor::from_array((m.c2.to_vec(), state.cache2.clone()))
            .map_err(|e| format!("cache2 tensor: {e}"))?;
        let clen_t = Tensor::from_array((vec![1i64], vec![state.cache_len]))
            .map_err(|e| format!("cache_len tensor: {e}"))?;
        let outputs = self
            .encoder
            .run(ort::inputs! {
                "audio_signal" => audio_t,
                "length" => len_t,
                "cache_last_channel" => c1_t,
                "cache_last_time" => c2_t,
                "cache_last_channel_len" => clen_t,
            })
            .map_err(|e| format!("encoder run: {e}"))?;
        // outputs[0]=encoded [1,512,T'], [2]=cache1_next, [3]=cache2_next, [4]=cache_len_next.
        let (enc_shape, enc_data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("encoder out extract: {e}"))?;
        let d = enc_shape[1] as usize; // encoder dim (512)
        let t_out = enc_shape[2] as usize;
        let enc = enc_data.to_vec();
        state.cache1 = outputs[2]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("cache1 next: {e}"))?
            .1
            .to_vec();
        state.cache2 = outputs[3]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("cache2 next: {e}"))?
            .1
            .to_vec();
        state.cache_len = outputs[4]
            .try_extract_tensor::<i64>()
            .map_err(|e| format!("cache_len next: {e}"))?
            .1
            .first()
            .copied()
            .unwrap_or(0);
        drop(outputs);

        // Greedy transducer decode over each encoder output column (channel-major: enc[ch*T'+t]).
        let mut col = vec![0.0f32; d];
        for t in 0..t_out {
            for (ch, slot) in col.iter_mut().enumerate() {
                *slot = enc[ch * t_out + t];
            }
            let mut emitted = 0;
            while emitted < MAX_SYMBOLS_PER_FRAME {
                let k = self.run_joiner(&col, &state.dec_out)?;
                if k == self.meta.blank_id {
                    break;
                }
                state.hyp.push(k);
                let (dec_out, h, c) = self.run_decoder(
                    k,
                    std::mem::take(&mut state.h),
                    std::mem::take(&mut state.c),
                )?;
                state.dec_out = dec_out;
                state.h = h;
                state.c = c;
                emitted += 1;
            }
        }
        state.transcribe_ms += t0.elapsed().as_secs_f64() * 1000.0;
        Ok(())
    }

    /// Run the decoder (prediction LSTM) for one token, returning (decoder_out, h_next, c_next).
    fn run_decoder(&mut self, token: i32, h: Vec<f32>, c: Vec<f32>) -> Result<DecoderStep, String> {
        let m = &self.meta;
        let sh = vec![m.pred_layers as i64, 1, m.pred_hidden as i64];
        let targets = Tensor::from_array((vec![1i64, 1], vec![token]))
            .map_err(|e| format!("targets tensor: {e}"))?;
        let tlen = Tensor::from_array((vec![1i64], vec![1i32]))
            .map_err(|e| format!("target_length tensor: {e}"))?;
        let h_t = Tensor::from_array((sh.clone(), h)).map_err(|e| format!("h tensor: {e}"))?;
        let c_t = Tensor::from_array((sh, c)).map_err(|e| format!("c tensor: {e}"))?;
        let outputs = self
            .decoder
            .run(ort::inputs! {
                self.dec_in_names[0].as_str() => targets,
                self.dec_in_names[1].as_str() => tlen,
                self.dec_in_names[2].as_str() => h_t,
                self.dec_in_names[3].as_str() => c_t,
            })
            .map_err(|e| format!("decoder run: {e}"))?;
        // outputs[0]=decoder_out [1,640,1], [2]=h_next, [3]=c_next (index-3 name is unstable).
        let dec_out = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("decoder out: {e}"))?
            .1
            .to_vec();
        let h_next = outputs[self.dec_out_names[2].as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("h next: {e}"))?
            .1
            .to_vec();
        let c_next = outputs[self.dec_out_names[3].as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("c next: {e}"))?
            .1
            .to_vec();
        Ok((dec_out, h_next, c_next))
    }

    /// Run the joiner on one encoder column + the current decoder column; return argmax token id.
    fn run_joiner(&mut self, enc_col: &[f32], dec_out: &[f32]) -> Result<i32, String> {
        let enc_t = Tensor::from_array((vec![1i64, enc_col.len() as i64, 1], enc_col.to_vec()))
            .map_err(|e| format!("joiner enc tensor: {e}"))?;
        let dec_t = Tensor::from_array((vec![1i64, dec_out.len() as i64, 1], dec_out.to_vec()))
            .map_err(|e| format!("joiner dec tensor: {e}"))?;
        let outputs = self
            .joiner
            .run(ort::inputs! { "encoder_outputs" => enc_t, "decoder_outputs" => dec_t })
            .map_err(|e| format!("joiner run: {e}"))?;
        let (_, logits) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("joiner out: {e}"))?;
        let mut best = 0i32;
        let mut best_v = f32::NEG_INFINITY;
        for (i, &v) in logits.iter().enumerate() {
            if v > best_v {
                best_v = v;
                best = i as i32;
            }
        }
        Ok(best)
    }

    /// The hypothesis so far, detokenized (BPE `▁` → space).
    fn text(&self, state: &StreamingState) -> String {
        let mut s = String::new();
        for &t in &state.hyp {
            if let Some(tok) = self.tokens.get(t as usize) {
                s.push_str(tok);
            } else {
                eprintln!("dontspeak/helper: STT emitted out-of-range token id {t}");
            }
        }
        s.replace('\u{2581}', " ").trim().to_string()
    }
}

/// Parse `tokens.txt`: each line `token<space>id`; index = id (line order is id order here).
fn parse_tokens(text: &str) -> Result<Vec<String>, String> {
    let mut parsed = Vec::new();
    let mut max_id = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (token, id) = line
            .rsplit_once(' ')
            .ok_or_else(|| format!("tokens.txt line has no id: {line:?}"))?;
        let id: usize = id
            .parse()
            .map_err(|_| format!("tokens.txt has invalid id in line: {line:?}"))?;
        max_id = max_id.max(id);
        parsed.push((id, token.to_string()));
    }
    let mut tokens = vec![None; max_id.saturating_add(1)];
    for (id, token) in parsed {
        if tokens[id].is_some() {
            return Err(format!("tokens.txt has duplicate id {id}"));
        }
        tokens[id] = Some(token);
    }
    Ok(tokens
        .into_iter()
        .map(|token| token.unwrap_or_default())
        .collect())
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared streaming layer — reused by EVERY streaming STT backend (ONNX here, the
// macOS Core ML / FluidAudio backend in `crate::coreml`). Only the per-backend
// inference differs (the `StreamingStt` trait); the resampling, tail-withholding,
// audio accounting (`StreamSession`) and the helper's drain→partial→finalize loop
// + STTSTATS schema are common.
// ─────────────────────────────────────────────────────────────────────────────

/// A streaming speech-to-text backend: fed 16 kHz mono PCM incrementally, yields a growing
/// hypothesis, and flushes a final transcript. The ONE backend-specific surface — everything
/// around it (resampling, cadence, partial/STTSTATS emission) is shared.
pub trait StreamingStt: Send {
    /// Begin a NEW utterance: clear per-utterance state (caches, hypothesis, timers) while keeping
    /// the loaded model resident, so a cached backend is reused across dictations without reloading.
    fn reset(&mut self) -> Result<(), String>;
    /// Feed 16 kHz mono PCM (may be empty); return the hypothesis text so far.
    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String>;
    /// Flush remaining audio and return the final transcript.
    fn finalize(&mut self) -> Result<String, String>;
    /// Cumulative model-inference time (ms), for the STTSTATS `transcribe_ms` field.
    fn transcribe_ms(&self) -> f64 {
        0.0
    }
    /// The execution provider this concrete streaming backend actually loaded on.
    fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Cpu
    }
}

/// Run `call`, returning its result alongside the wall-clock time it took (ms). Shared by every
/// FFI-backed [`StreamingStt`] impl that times its push/finalize calls for the STTSTATS
/// `transcribe_ms` field (originally `CoremlStreamer`'s private helper — relocated here so
/// `crate::sysspeech::SystemStreamer` reuses the SAME implementation instead of a third copy).
/// Pure (no FFI/self dependency), so it's unit-testable without a real backend; callers add the
/// elapsed time to their own accumulator only when `call` succeeds (mirroring
/// `OnnxStreamer::run_encoder_step`, where an early `?` bail-out skips the accumulate line too).
pub fn timed<T>(call: impl FnOnce() -> Result<T, String>) -> (Result<T, String>, f64) {
    let t0 = Instant::now();
    let out = call();
    (out, t0.elapsed().as_secs_f64() * 1000.0)
}

/// The ONNX cache-aware streaming backend bound into one owner (model + per-utterance state) so it
/// fits the [`StreamingStt`] trait object the helper drives.
pub struct OnnxStreamer {
    model: StreamingModel,
    state: StreamingState,
}

impl OnnxStreamer {
    /// Load the streaming model from `dir` (int8 by default) and seed a fresh utterance.
    pub fn load(dir: &Path, int8: bool) -> Result<Self, String> {
        let mut model = StreamingModel::load(dir, int8)?;
        let state = model.new_state()?;
        Ok(Self { model, state })
    }
}

impl StreamingStt for OnnxStreamer {
    fn reset(&mut self) -> Result<(), String> {
        self.state = self.model.new_state()?;
        Ok(())
    }
    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
        self.model.accept_16k(&mut self.state, pcm_16k)
    }
    fn finalize(&mut self) -> Result<String, String> {
        self.model.finalize(&mut self.state)
    }
    fn transcribe_ms(&self) -> f64 {
        self.state.transcribe_ms
    }
    fn provider(&self) -> ds_config::RealizedProvider {
        self.model.provider()
    }
}

/// Persistent device-rate -> 16 kHz converter for one utterance. Unlike the old
/// `StreamSession` implementation, this never re-runs the resampler over samples it already
/// consumed: each input frame enters Rubato exactly once, full chunks are emitted immediately,
/// and `finish` pads only the final short chunk and drains the resampler delay.
struct IncrementalResampler {
    input_rate: u32,
    resampler: Option<Async<f32>>,
    pending: Vec<f32>,
    delay_left: usize,
    total_input: usize,
    total_output: usize,
    #[cfg(test)]
    processed_input: usize,
}

impl IncrementalResampler {
    fn new(input_rate: u32) -> Result<Self, String> {
        let input_rate = input_rate.max(1);
        let resampler = if input_rate == 16_000 {
            None
        } else {
            Some(
                Async::<f32>::new_poly(
                    16_000.0 / input_rate as f64,
                    1.1,
                    PolynomialDegree::Septic,
                    1024,
                    1,
                    FixedAsync::Input,
                )
                .map_err(|e| format!("stream resampler init: {e}"))?,
            )
        };
        let delay_left = resampler.as_ref().map_or(0, Resampler::output_delay);
        Ok(Self {
            input_rate,
            resampler,
            pending: Vec::with_capacity(2048),
            delay_left,
            total_input: 0,
            total_output: 0,
            #[cfg(test)]
            processed_input: 0,
        })
    }

    fn expected_output(&self) -> usize {
        ((self.total_input as u128 * 16_000).div_ceil(self.input_rate as u128)) as usize
    }

    fn accept(&mut self, input: &[f32]) -> Result<Vec<f32>, String> {
        self.total_input = self.total_input.saturating_add(input.len());
        if self.resampler.is_none() {
            self.total_output = self.total_output.saturating_add(input.len());
            #[cfg(test)]
            {
                self.processed_input = self.processed_input.saturating_add(input.len());
            }
            return Ok(input.to_vec());
        }
        self.pending.extend_from_slice(input);
        let mut out = Vec::new();
        loop {
            let need = self
                .resampler
                .as_ref()
                .expect("non-passthrough resampler")
                .input_frames_next();
            if self.pending.len() < need {
                break;
            }
            self.process_one(need, None, &mut out)?;
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<f32>, String> {
        if self.resampler.is_none() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        while !self.pending.is_empty() {
            let need = self
                .resampler
                .as_ref()
                .expect("non-passthrough resampler")
                .input_frames_next();
            if self.pending.len() >= need {
                self.process_one(need, None, &mut out)?;
            } else {
                let available = self.pending.len();
                self.process_one(need, Some(available), &mut out)?;
            }
        }

        // Rubato's delay is real state, not input: pump zero-length partials until the exact
        // duration implied by the captured input has emerged. The small fixed cap is only a
        // corruption guard; normal polynomial resamplers need at most a couple of calls.
        let expected = self.expected_output();
        for _ in 0..8 {
            if self.total_output >= expected {
                break;
            }
            let need = self
                .resampler
                .as_ref()
                .expect("non-passthrough resampler")
                .input_frames_next();
            self.process_one(need, Some(0), &mut out)?;
        }
        if self.total_output < expected {
            return Err(format!(
                "stream resampler drained {} of {expected} expected samples",
                self.total_output
            ));
        }
        Ok(out)
    }

    fn process_one(
        &mut self,
        required_input: usize,
        partial_len: Option<usize>,
        dst: &mut Vec<f32>,
    ) -> Result<(), String> {
        let available = partial_len.unwrap_or(required_input);
        let mut input = vec![0.0f32; required_input.max(1)];
        if available > 0 {
            input[..available].copy_from_slice(&self.pending[..available]);
        }
        let input_adapter = InterleavedSlice::new(&input, 1, input.len())
            .map_err(|e| format!("stream resampler input: {e}"))?;
        let output_cap = self
            .resampler
            .as_ref()
            .expect("non-passthrough resampler")
            .output_frames_max()
            .max(1);
        let mut output = vec![0.0f32; output_cap];
        let mut output_adapter = InterleavedSlice::new_mut(&mut output, 1, output_cap)
            .map_err(|e| format!("stream resampler output: {e}"))?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            active_channels_mask: None,
            partial_len,
        };
        let (consumed, produced) = self
            .resampler
            .as_mut()
            .expect("non-passthrough resampler")
            .process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing))
            .map_err(|e| format!("stream resampler process: {e}"))?;

        let consumed_real = consumed.min(available);
        if consumed_real > 0 {
            self.pending.drain(..consumed_real);
        }
        #[cfg(test)]
        {
            self.processed_input = self.processed_input.saturating_add(consumed_real);
        }
        let skip = self.delay_left.min(produced);
        self.delay_left -= skip;
        let remaining = self.expected_output().saturating_sub(self.total_output);
        let keep = (produced - skip).min(remaining);
        dst.extend_from_slice(&output[skip..skip + keep]);
        self.total_output = self.total_output.saturating_add(keep);
        Ok(())
    }
}

/// SHARED capture-to-backend plumbing: owns a [`StreamingStt`] backend plus a persistent
/// device-rate -> 16 kHz resampler and `audio_ms` accounting. Both the ONNX and macOS
/// Core ML/System backends run behind this, so only inference differs.
pub struct StreamSession {
    backend: Box<dyn StreamingStt>,
    resampler: IncrementalResampler,
    audio_ms: f64,
}

impl StreamSession {
    /// Wrap `backend`, feeding it audio captured at `in_rate` (resampled to 16 kHz internally;
    /// passthrough when already 16 kHz).
    pub fn new(backend: Box<dyn StreamingStt>, in_rate: u32) -> Result<Self, String> {
        Ok(Self {
            backend,
            resampler: IncrementalResampler::new(in_rate)?,
            audio_ms: 0.0,
        })
    }

    /// Accept a chunk of device-rate mono audio; resample, hand the new stable 16 kHz frames to
    /// the backend, and return the hypothesis so far.
    pub fn accept(&mut self, pcm_device: &[f32]) -> Result<String, String> {
        let new = self.resampler.accept(pcm_device)?;
        if !new.is_empty() {
            self.audio_ms += new.len() as f64 / 16.0;
        }
        self.backend.accept_16k(&new)
    }

    /// Flush the withheld tail + the backend, returning the final transcript.
    ///
    /// The tail-flush `accept_16k` is BEST-EFFORT: on the Core ML/system backends,
    /// `backend.finalize()` is what tears down the Swift-side session
    /// (`smk_asr_stream_finish`/`smk_sys_stream_finish`) — skipping it after a failed tail
    /// flush used to leak that session, since nothing else ever finishes it and the next
    /// utterance's `reset()` would then clobber the reference with no cleanup. So a tail-flush
    /// error is logged, not propagated, and `finalize()` always runs. `OnnxStreamer::finalize`
    /// has no such side effect (purely local model state), so calling it unconditionally here
    /// doesn't change its behavior either.
    pub fn finalize(&mut self) -> Result<String, String> {
        match self.resampler.finish() {
            Ok(new) if !new.is_empty() => {
                self.audio_ms += new.len() as f64 / 16.0;
                if let Err(e) = self.backend.accept_16k(&new) {
                    eprintln!("StreamSession::finalize: tail flush failed, finalizing anyway: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "StreamSession::finalize: resampler flush failed, finalizing anyway: {e}"
                );
            }
        }
        self.backend.finalize()
    }

    /// 16 kHz audio duration fed so far, in ms (STTSTATS).
    pub fn audio_ms(&self) -> f64 {
        self.audio_ms
    }
    /// Backend model-inference time, in ms (STTSTATS).
    pub fn transcribe_ms(&self) -> f64 {
        self.backend.transcribe_ms()
    }
    /// Reclaim the backend (to cache the loaded model for the next dictation).
    pub fn into_backend(self) -> Box<dyn StreamingStt> {
        self.backend
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeBackendState {
        accepted: Vec<Vec<f32>>,
        finalized: bool,
        fail_accept: bool,
    }

    struct FakeBackend {
        state: Arc<Mutex<FakeBackendState>>,
    }

    impl StreamingStt for FakeBackend {
        fn reset(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String> {
            let mut state = self.state.lock().unwrap();
            state.accepted.push(pcm_16k.to_vec());
            if state.fail_accept && !pcm_16k.is_empty() {
                Err("accept failed".to_string())
            } else {
                Ok("partial".to_string())
            }
        }

        fn finalize(&mut self) -> Result<String, String> {
            self.state.lock().unwrap().finalized = true;
            Ok("final".to_string())
        }

        fn transcribe_ms(&self) -> f64 {
            12.5
        }

        fn provider(&self) -> ds_config::RealizedProvider {
            ds_config::RealizedProvider::Cpu
        }
    }

    fn fake_backend(fail_accept: bool) -> (Box<dyn StreamingStt>, Arc<Mutex<FakeBackendState>>) {
        let state = Arc::new(Mutex::new(FakeBackendState {
            fail_accept,
            ..FakeBackendState::default()
        }));
        (
            Box::new(FakeBackend {
                state: state.clone(),
            }),
            state,
        )
    }

    /// Minimal 16-bit PCM mono WAV reader → f32 [-1,1] (test-only; assumes 16 kHz mono LE).
    fn read_wav_16k_mono_pcm(path: &std::path::Path) -> Vec<f32> {
        let bytes = std::fs::read(path).expect("read wav");
        // Find the "data" chunk, then read i16 samples after its 8-byte header.
        let pos = bytes
            .windows(4)
            .position(|w| w == b"data")
            .expect("no data chunk");
        let start = pos + 8;
        bytes[start..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect()
    }

    #[test]
    fn parse_tokens_splits_on_last_space() {
        let v = parse_tokens("\u{2581}the 5\n<blk> 1024\n").unwrap();
        assert_eq!(v[5], "\u{2581}the");
        assert_eq!(v[1024], "<blk>");
    }

    /// End-to-end oracle: gated on a real model dir via DONTSPEAK_STREAMING_MODEL_DIR (containing
    /// encoder/decoder/joiner.int8.onnx + tokens.txt + test_wavs/0.wav). Reproduces the reference
    /// transcript. Skipped (passes) when the env/model isn't present so CI stays self-contained.
    #[test]
    fn oracle_transcribes_test_wav() {
        let Ok(dir) = std::env::var("DONTSPEAK_STREAMING_MODEL_DIR") else {
            eprintln!("skip: set DONTSPEAK_STREAMING_MODEL_DIR to run the oracle test");
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let wav = dir.join("test_wavs/0.wav");
        let pcm = read_wav_16k_mono_pcm(&wav);
        let mut model = StreamingModel::load(&dir, true).expect("load");
        let mut st = model.new_state().expect("state");
        model.accept_16k(&mut st, &pcm).expect("accept");
        let text = model.finalize(&mut st).expect("finalize");
        eprintln!("streaming oracle => {text:?}");
        assert!(
            text.contains("after early nightfall the yellow lamps"),
            "unexpected transcript: {text:?}"
        );
    }

    // `timed` relocated from `coreml.rs` (see its doc comment) — coverage moves with it.

    #[test]
    fn timed_reports_elapsed_ms_and_preserves_ok() {
        let (result, ms) = timed(|| -> Result<i32, String> {
            std::thread::sleep(std::time::Duration::from_millis(5));
            Ok(42)
        });
        assert_eq!(result, Ok(42));
        assert!(ms >= 5.0, "expected >= 5ms elapsed, got {ms}");
    }

    #[test]
    fn timed_still_reports_elapsed_on_err() {
        let (result, ms) = timed(|| -> Result<i32, String> { Err("boom".to_string()) });
        assert_eq!(result, Err("boom".to_string()));
        assert!(ms >= 0.0);
    }

    #[test]
    fn incremental_resampler_processes_each_input_sample_once() {
        let mut rs = IncrementalResampler::new(48_000).unwrap();
        let block = vec![0.1f32; 2_400]; // 50 ms at 48 kHz
        let mut output = Vec::new();
        for _ in 0..1_200 {
            output.extend(rs.accept(&block).unwrap()); // one minute, delivered live
            assert!(
                rs.pending.len() < 1_024,
                "only one incomplete Rubato chunk may be retained"
            );
        }
        output.extend(rs.finish().unwrap());
        assert_eq!(rs.processed_input, block.len() * 1_200);
        assert_eq!(output.len(), 16_000 * 60);
    }

    #[test]
    fn incremental_resampler_passthrough_is_exact_and_has_no_history() {
        let mut rs = IncrementalResampler::new(16_000).unwrap();
        let a = vec![0.25f32; 800];
        let b = vec![-0.5f32; 320];
        assert_eq!(rs.accept(&a).unwrap(), a);
        assert_eq!(rs.accept(&b).unwrap(), b);
        assert!(rs.pending.is_empty());
        assert!(rs.finish().unwrap().is_empty());
        assert_eq!(rs.processed_input, 1_120);
    }

    #[test]
    fn stream_session_passthrough_preserves_audio_and_backend_metadata() {
        let (backend, state) = fake_backend(false);
        let mut session = StreamSession::new(backend, 16_000).unwrap();
        let pcm = vec![0.25f32, -0.5, 0.75, 0.0];

        assert_eq!(session.accept(&pcm).unwrap(), "partial");
        assert_eq!(session.audio_ms(), 0.25);
        assert_eq!(session.transcribe_ms(), 12.5);
        assert_eq!(session.finalize().unwrap(), "final");

        let state = state.lock().unwrap();
        assert_eq!(state.accepted, vec![pcm]);
        assert!(state.finalized);
        drop(state);
        assert_eq!(
            session.into_backend().provider(),
            ds_config::RealizedProvider::Cpu
        );
    }

    /// The tail accept can fail on a native backend, but finalization owns native-session
    /// cleanup and therefore must still run.
    #[test]
    fn stream_session_finalizes_after_resampled_tail_accept_fails() {
        let (backend, state) = fake_backend(true);
        let mut session = StreamSession::new(backend, 48_000).unwrap();

        assert!(session.accept(&vec![0.1; 480]).is_ok());
        assert_eq!(session.finalize().unwrap(), "final");
        assert!(session.audio_ms() > 0.0);

        let state = state.lock().unwrap();
        assert!(state.accepted.iter().any(|chunk| !chunk.is_empty()));
        assert!(state.finalized);
    }

    #[test]
    fn stream_session_resampling_accounts_for_exact_audio_duration() {
        let (backend, state) = fake_backend(false);
        let mut session = StreamSession::new(backend, 48_000).unwrap();
        let input = vec![0.1f32; 4_800];

        session.accept(&input[..1_700]).unwrap();
        session.accept(&input[1_700..]).unwrap();
        session.finalize().unwrap();

        let received: usize = state.lock().unwrap().accepted.iter().map(Vec::len).sum();
        assert_eq!(received, 1_600);
        assert_eq!(session.audio_ms(), 100.0);
    }
}
