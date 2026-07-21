//! Offline Parakeet transducer over `ort`, segmented by voice activity.
//!
//! Model: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8` (encoder + decoder LSTM + joiner ONNX
//! + `tokens.txt`). Token-and-duration transducer: the joiner emits vocabulary logits AND a
//! duration, so decoding jumps forward by the predicted frame count instead of stepping one
//! frame at a time. The encoder is full-context with no cache — it was never trained with
//! limited context, so it cannot be re-exported cache-aware — and each speech segment is
//! therefore encoded ONCE, whole, at the pause that closes it. [`VadBoundaryDetector`]
//! bounds segment length, which keeps cost flat instead of growing with dictation length
//! the way a re-decoded open tail does.
//!
//! Features: kaldi log-mel fbank (`feat_dim` bins from encoder metadata, 25/10 ms, dither 0,
//! snip_edges false, `use_energy` off) over [-1, 1] — NO 32768 scaling. The encoder declares
//! its own `normalize_type`; `per_feature` means NeMo normalized each mel bin over the
//! utterance during training and the export did NOT bake that in, so it has to happen here.
//! Either mistake — wrong scaling, missing normalization — decodes to all-blank rather than
//! to degraded text.

use std::path::Path;
use std::time::Instant;

use kaldi_native_fbank::fbank::{FbankComputer, FbankOptions};
use kaldi_native_fbank::online::{FeatureComputer, OnlineFeature};
use ort::session::Session;
use ort::value::Tensor;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler};

use crate::boundary::VadBoundaryDetector;

/// One decoder step's result: (decoder_out column, next LSTM `h`, next LSTM `c`).
type DecoderStep = (Vec<f32>, Vec<f32>, Vec<f32>);

/// Consecutive zero-duration joiner steps tolerated on one frame before the decode advances
/// anyway. A TDT model legitimately emits several symbols at one frame; only a model that
/// never advances is a hang, and this is what bounds it.
const MAX_SYMBOLS_PER_FRAME: usize = 10;
/// Shortest segment worth encoding (100 ms at 16 kHz). Below this a "segment" is a click or
/// the residue of a boundary that landed on silence: features would be a handful of frames
/// and the decode is guaranteed blank.
const MIN_SEGMENT_SAMPLES: usize = 1_600;

/// NeMo's `normalize_batch` epsilon, added to the per-bin standard deviation.
const NORMALIZE_EPS: f32 = 1e-5;

/// Encoder metadata (read at load — never hardcode; the export carries its own contract).
struct Meta {
    feat_dim: usize,
    /// `per_feature` (NeMo's default) → normalize each mel bin over the segment.
    per_feature_norm: bool,
    blank_id: i32, // = vocab_size; tokens.txt has vocab_size + 1 entries
    pred_hidden: usize,
    pred_layers: usize,
}

/// Loaded model: three ort sessions + meta + tokens.
pub struct TransducerModel {
    encoder: Session,
    decoder: Session,
    joiner: Session,
    /// Index 2/3 = next LSTM states; index-3 name is unstable across exports.
    dec_out_names: Vec<String>,
    /// Semantic export order: targets, length, h, c.
    dec_in_names: Vec<String>,
    meta: Meta,
    tokens: Vec<String>,
    /// REALIZED EP (incl. CPU fallback). Same [`ds_model::cuda_session_builder`] path as ONNX TTS
    /// so STT/TTS status tokens can't drift.
    provider: ds_config::RealizedProvider,
}

/// Per-utterance state: the audio not yet decoded, plus the segments already decoded.
pub struct TranscribeState {
    /// Endpointer over the 16 kHz stream this state is fed.
    boundary: VadBoundaryDetector,
    /// Captured audio from the last boundary onward.
    pending: Vec<f32>,
    /// Samples the boundary detector has already accounted for and `pending` has dropped —
    /// boundaries come back in whole-stream coordinates, `pending` starts at this offset.
    dropped: usize,
    /// Finished segment texts, in spoken order.
    committed: Vec<String>,
    transcribe_ms: f64, // cumulative model time for STTSTATS
}

impl TranscribeState {
    /// Append `pcm_16k` and hand back every segment the endpointer just closed, in order.
    /// Audio past the last boundary stays pending for the next call or for finalize.
    fn take_closed_segments(&mut self, pcm_16k: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(pcm_16k);
        let mut segments = Vec::new();
        for boundary in self.boundary.feed(pcm_16k) {
            // Boundaries are absolute in the fed stream; `pending` starts at `dropped`.
            let end = boundary
                .saturating_sub(self.dropped)
                .min(self.pending.len());
            if end == 0 {
                continue;
            }
            segments.push(self.pending.drain(..end).collect());
            self.dropped += end;
        }
        segments
    }
}

fn meta_str(s: &Session, key: &str) -> Option<String> {
    s.metadata().ok().and_then(|m| m.custom(key))
}
fn meta_usize(s: &Session, key: &str, default: usize) -> usize {
    meta_str(s, key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

/// One session via the GPU-aware builder shared with Kokoro
/// ([`ds_model::cuda_session_builder`]). Returns session + realized EP so STT/TTS
/// report through ONE path. int8 ops ORT can't place on CUDA fall back per-op.
fn build(path: &Path, want_gpu: bool) -> Result<(Session, ds_config::RealizedProvider), String> {
    let bytes = ds_model::read_model_file(path)?;
    let (mut builder, realized) = ds_model::cuda_session_builder(want_gpu)?;
    let session = builder
        .commit_from_memory(&bytes)
        .map_err(|e| format!("ort load {}: {e}", path.display()))?;
    Ok((session, realized))
}

impl TransducerModel {
    /// Load encoder/decoder/joiner + `tokens.txt`. Honors `DONTSPEAK_STT_PROVIDER`.
    /// Mirrors Kokoro: GPU load failure retries on CPU (e.g. Win32 1114).
    pub fn load(dir: &Path, int8: bool) -> Result<Self, String> {
        let want_gpu = ds_config::provider_pref_wants_gpu(
            &std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default(),
        );
        Self::load_with_gpu_preference(dir, int8, want_gpu)
    }

    /// Explicit provider token (in-process users can't rely on helper env).
    pub fn load_for_provider(dir: &Path, int8: bool, provider: &str) -> Result<Self, String> {
        Self::load_with_gpu_preference(dir, int8, ds_config::provider_pref_wants_gpu(provider))
    }

    fn load_with_gpu_preference(dir: &Path, int8: bool, want_gpu: bool) -> Result<Self, String> {
        match Self::load_on(dir, int8, want_gpu) {
            Ok(m) => Ok(m),
            Err(e) if want_gpu => {
                log::warn!(target: "stt", "STT GPU load failed ({e}); retrying on CPU");
                Self::load_on(dir, int8, false)
            }
            Err(e) => Err(e),
        }
    }

    /// Load with explicit `want_gpu`. Shared ort bootstrap with TTS (first warm wins).
    fn load_on(dir: &Path, int8: bool, want_gpu: bool) -> Result<Self, String> {
        ds_model::ensure_ort_dylib_gpu(want_gpu)?;
        let sfx = if int8 { ".int8" } else { "" };
        // Same shared runtime → same realized EP; encoder's is representative.
        let (encoder, provider) = build(&dir.join(format!("encoder{sfx}.onnx")), want_gpu)?;
        let (decoder, _) = build(&dir.join(format!("decoder{sfx}.onnx")), want_gpu)?;
        let (joiner, _) = build(&dir.join(format!("joiner{sfx}.onnx")), want_gpu)?;

        // encoded + encoded_lengths. A cache-aware export has five and threads state the
        // decode loop below does not keep — reject it here rather than mis-decode it.
        if encoder.outputs().len() != 2 {
            return Err(format!(
                "encoder has {} outputs, expected 2 (offline transducer)",
                encoder.outputs().len()
            ));
        }

        let vocab = meta_usize(&encoder, "vocab_size", 1024);
        let feat_dim = meta_usize(&encoder, "feat_dim", 128);
        if feat_dim == 0 {
            return Err("encoder meta feat_dim is 0".to_string());
        }
        let meta = Meta {
            feat_dim,
            per_feature_norm: meta_str(&encoder, "normalize_type")
                .is_none_or(|t| t.trim() == "per_feature"),
            blank_id: vocab as i32,
            pred_hidden: meta_usize(&encoder, "pred_hidden", 640),
            pred_layers: meta_usize(&encoder, "pred_rnn_layers", 1),
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

    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.provider
    }

    /// Kaldi log-mel fbank matching reference (use_energy off, dither 0, snip_edges off).
    fn new_fbank(&self) -> Result<OnlineFeature, String> {
        let mut opts = FbankOptions::default();
        opts.frame_opts.samp_freq = 16_000.0;
        opts.frame_opts.dither = 0.0;
        opts.frame_opts.snip_edges = false;
        opts.mel_opts.num_bins = self.meta.feat_dim;
        opts.use_energy = false;
        let comp = FbankComputer::new(opts).map_err(|e| format!("fbank init: {e}"))?;
        Ok(OnlineFeature::new(FeatureComputer::Fbank(comp)))
    }

    /// New utterance: empty capture, fresh endpointer, nothing committed.
    /// [`Self::accept_16k`] expects 16 kHz mono (resample is in the stream session).
    pub fn new_state(&mut self) -> Result<TranscribeState, String> {
        Ok(TranscribeState {
            boundary: VadBoundaryDetector::new(16_000),
            pending: Vec::new(),
            dropped: 0,
            committed: Vec::new(),
            transcribe_ms: 0.0,
        })
    }

    /// Feed 16 kHz mono; decode every segment the endpointer closed; return the text so far.
    /// Audio after the last boundary stays pending — it is transcribed by [`Self::finalize`].
    pub fn accept_16k(
        &mut self,
        state: &mut TranscribeState,
        pcm_16k: &[f32],
    ) -> Result<String, String> {
        if pcm_16k.is_empty() {
            return Ok(self.text(state));
        }
        for segment in state.take_closed_segments(pcm_16k) {
            self.decode_segment(state, &segment)?;
        }
        Ok(self.text(state))
    }

    /// Decode the still-open tail → final text.
    pub fn finalize(&mut self, state: &mut TranscribeState) -> Result<String, String> {
        let tail = std::mem::take(&mut state.pending);
        state.dropped += tail.len();
        self.decode_segment(state, &tail)?;
        Ok(self.text(state))
    }

    /// One whole segment: features → one encoder forward → greedy decode → commit its text.
    /// Decoder state is per segment: a closed segment is a finished utterance, and carrying
    /// LSTM state across a pause would condition the next one on it.
    fn decode_segment(&mut self, state: &mut TranscribeState, pcm: &[f32]) -> Result<(), String> {
        if pcm.len() < MIN_SEGMENT_SAMPLES {
            return Ok(());
        }
        let t0 = Instant::now();
        let audio = self.features(pcm)?;
        let frames = audio.len() / self.meta.feat_dim;
        let hyp = self.encode_and_decode(&audio, frames)?;
        state.transcribe_ms += t0.elapsed().as_secs_f64() * 1000.0;
        let text = self.detokenize(&hyp);
        if !text.is_empty() {
            state.committed.push(text);
        }
        Ok(())
    }

    /// Channel-major log-mel features `[feat_dim, frames]` for a whole segment.
    fn features(&self, pcm: &[f32]) -> Result<Vec<f32>, String> {
        let mut fbank = self.new_fbank()?;
        fbank.accept_waveform(16_000.0, pcm);
        fbank.input_finished();
        let frames = fbank.num_frames_ready();
        let bins = self.meta.feat_dim;
        let mut audio = vec![0.0f32; bins * frames];
        for f in 0..frames {
            let frame = fbank
                .get_frame(f)
                .ok_or_else(|| format!("fbank frame {f} missing"))?;
            for (ch, &v) in frame.iter().enumerate().take(bins) {
                audio[ch * frames + f] = v;
            }
        }
        if self.meta.per_feature_norm && frames > 1 {
            normalize_per_feature(&mut audio, bins, frames);
        }
        Ok(audio)
    }

    /// Encode the whole segment once, then greedy-decode every encoder column.
    fn encode_and_decode(&mut self, audio: &[f32], frames: usize) -> Result<Vec<i32>, String> {
        if frames == 0 {
            return Ok(Vec::new());
        }
        let audio_t = Tensor::from_array((
            vec![1i64, self.meta.feat_dim as i64, frames as i64],
            audio.to_vec(),
        ))
        .map_err(|e| format!("audio tensor: {e}"))?;
        let len_t = Tensor::from_array((vec![1i64], vec![frames as i64]))
            .map_err(|e| format!("length tensor: {e}"))?;
        let outputs = self
            .encoder
            .run(ort::inputs! { "audio_signal" => audio_t, "length" => len_t })
            .map_err(|e| format!("encoder run: {e}"))?;
        let (enc_shape, enc_data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("encoder out extract: {e}"))?;
        let d = enc_shape[1] as usize; // encoder dim
        let padded = enc_shape[2] as usize;
        // Trust the encoder's own valid-length output over the padded time axis.
        let valid = outputs[1]
            .try_extract_tensor::<i64>()
            .map_err(|e| format!("encoded length extract: {e}"))?
            .1
            .first()
            .copied()
            .unwrap_or(padded as i64)
            .clamp(0, padded as i64) as usize;
        let enc = enc_data.to_vec();
        drop(outputs);

        let (blank_id, state_len) = (
            self.meta.blank_id,
            self.meta.pred_layers * self.meta.pred_hidden,
        );
        let (mut dec_out, mut h, mut c) =
            self.run_decoder(blank_id, vec![0.0f32; state_len], vec![0.0f32; state_len])?;
        let mut hyp = Vec::new();
        let mut col = vec![0.0f32; d];
        let mut t = 0usize;
        let mut stalled = 0usize;
        while t < valid {
            // Channel-major: enc[ch * T + t].
            for (ch, slot) in col.iter_mut().enumerate() {
                *slot = enc[ch * padded + t];
            }
            let (token, skip) = self.run_joiner(&col, &dec_out)?;
            if token != blank_id {
                hyp.push(token);
                let step =
                    self.run_decoder(token, std::mem::take(&mut h), std::mem::take(&mut c))?;
                dec_out = step.0;
                h = step.1;
                c = step.2;
            }
            if skip > 0 {
                t += skip;
                stalled = 0;
            } else {
                // Duration 0 keeps the same frame so another symbol can come from it.
                stalled += 1;
                if stalled >= MAX_SYMBOLS_PER_FRAME {
                    t += 1;
                    stalled = 0;
                }
            }
        }
        Ok(hyp)
    }

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
        // [0]=decoder_out, [2]=h_next, [3]=c_next (index-3 name unstable).
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

    /// One joint step → (token id, frames to skip). A TDT joiner emits the vocabulary logits
    /// followed by one logit per configured duration, and the argmax over that tail IS the
    /// duration in frames (the export lists durations 0..n, so index and value coincide).
    fn run_joiner(&mut self, enc_col: &[f32], dec_out: &[f32]) -> Result<(i32, usize), String> {
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
        let vocab = (self.meta.blank_id as usize) + 1;
        if logits.len() <= vocab {
            return Err(format!(
                "joiner emitted {} logits, need > vocab {vocab} (token + duration halves)",
                logits.len()
            ));
        }
        let (tokens, durations) = logits.split_at(vocab);
        Ok((argmax(tokens) as i32, argmax(durations)))
    }

    /// One segment's tokens detokenized (BPE `▁` → space).
    fn detokenize(&self, hyp: &[i32]) -> String {
        let mut s = String::new();
        for &t in hyp {
            if let Some(tok) = self.tokens.get(t as usize) {
                s.push_str(tok);
            } else {
                log::warn!(target: "stt", "STT emitted out-of-range token id {t}");
            }
        }
        s.replace('\u{2581}', " ").trim().to_string()
    }

    /// Everything decoded so far, segments joined by a space.
    fn text(&self, state: &TranscribeState) -> String {
        state.committed.join(" ")
    }
}

/// NeMo `normalize_batch` with `per_feature`: each bin zero-mean and unit-variance over the
/// segment, using the unbiased standard deviation plus [`NORMALIZE_EPS`].
fn normalize_per_feature(audio: &mut [f32], bins: usize, frames: usize) {
    let n = frames as f32;
    for bin in 0..bins {
        let row = &mut audio[bin * frames..(bin + 1) * frames];
        let mean = row.iter().sum::<f32>() / n;
        let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / (n - 1.0);
        let scale = 1.0 / (var.sqrt() + NORMALIZE_EPS);
        for v in row.iter_mut() {
            *v = (*v - mean) * scale;
        }
    }
}

/// Index of the largest value; ties take the first.
fn argmax(values: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in values.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best
}

/// `tokens.txt`: lines `token<space>id`; index = id.
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

// ── Shared streaming layer (ONNX here; MLX in `crate::mlx`) ──────────────────
// Only per-backend inference differs (`StreamingStt`); resampling, tail-withholding,
// StreamSession accounting, helper drain→partial→finalize + STTSTATS are common.

/// Incremental 16 kHz mono STT. The ONE backend-specific surface — resampling,
/// cadence, partial/STTSTATS emission are shared.
pub trait StreamingStt: Send {
    /// New utterance: clear caches/hypothesis/timers; keep model resident.
    fn reset(&mut self) -> Result<(), String>;
    /// Feed 16 kHz mono (may be empty); hypothesis so far.
    fn accept_16k(&mut self, pcm_16k: &[f32]) -> Result<String, String>;
    /// Flush → final transcript.
    fn finalize(&mut self) -> Result<String, String>;
    /// Cumulative model time (ms) for STTSTATS.
    fn transcribe_ms(&self) -> f64 {
        0.0
    }
    fn provider(&self) -> ds_config::RealizedProvider {
        ds_config::RealizedProvider::Cpu
    }
}

/// Wall-clock ms alongside `call`'s result. Shared by FFI-backed [`StreamingStt`]
/// impls (STTSTATS `transcribe_ms`). Callers accumulate only on success (mirrors early `?` skip in
/// `OnnxStreamer::run_encoder_step`).
pub fn timed<T>(call: impl FnOnce() -> Result<T, String>) -> (Result<T, String>, f64) {
    let t0 = Instant::now();
    let out = call();
    (out, t0.elapsed().as_secs_f64() * 1000.0)
}

/// ONNX backend as one owner (model + utterance state) for [`StreamingStt`]. Audio streams
/// in; text comes out a segment at a time (see the module docs).
pub struct OnnxStreamer {
    model: TransducerModel,
    state: TranscribeState,
}

impl OnnxStreamer {
    /// Load the model from `dir` (int8 by default) and seed a fresh utterance.
    pub fn load(dir: &Path, int8: bool) -> Result<Self, String> {
        let mut model = TransducerModel::load(dir, int8)?;
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
/// MLX/System backends run behind this, so only inference differs.
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
    /// The tail-flush `accept_16k` is best-effort: on the MLX/system backends,
    /// `backend.finalize()` is what tears down the Swift-side session
    /// (`ds_mlx_asr_stream_finish`/`ds_mlx_sys_stream_finish`) — skipping it after a failed tail
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
                    log::warn!(
                        target: "stt",
                        "StreamSession::finalize: tail flush failed, finalizing anyway: {e}"
                    );
                }
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!(
                    target: "stt",
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

    /// 30 ms frames at 16 kHz, the endpointer's granularity.
    const FRAME: usize = 480;

    fn state() -> TranscribeState {
        TranscribeState {
            boundary: VadBoundaryDetector::new(16_000),
            pending: Vec::new(),
            dropped: 0,
            committed: Vec::new(),
            transcribe_ms: 0.0,
        }
    }

    #[test]
    fn closed_segments_carry_the_audio_between_boundaries() {
        // Speech long enough to open a region, then silence past the hangover closes it.
        let speech = vec![0.2f32; FRAME * 10];
        let silence = vec![0.0f32; FRAME * 30];
        let mut st = state();

        assert!(
            st.take_closed_segments(&speech).is_empty(),
            "an open region must not be handed out early"
        );
        let closed = st.take_closed_segments(&silence);
        assert_eq!(closed.len(), 1, "the pause must close exactly one segment");
        let first = &closed[0];
        assert!(
            first.len() >= speech.len(),
            "a closed segment carries its speech plus the hangover, got {}",
            first.len()
        );
        assert_eq!(
            st.dropped,
            first.len(),
            "dropped must track what left `pending`"
        );

        // A second utterance stays pending until its own pause: what remains is the silence
        // the first boundary left behind plus the new speech.
        let closed = st.take_closed_segments(&speech);
        assert!(closed.is_empty());
        assert!(st.pending.len() >= speech.len());
        assert_eq!(&st.pending[st.pending.len() - speech.len()..], &speech[..]);
    }

    #[test]
    fn silence_alone_never_closes_a_segment() {
        let mut st = state();
        for _ in 0..4 {
            assert!(
                st.take_closed_segments(&vec![0.0f32; FRAME * 20])
                    .is_empty()
            );
        }
        assert_eq!(st.dropped, 0);
    }

    #[test]
    fn per_feature_normalization_zeroes_mean_and_unit_variance_per_bin() {
        // Channel-major [bins, frames]; bin 1 is a shifted, scaled copy of bin 0, so an
        // untouched or globally-normalized array would leave them different.
        let frames = 4;
        let mut audio = vec![1.0, 2.0, 3.0, 4.0, 101.0, 102.0, 103.0, 104.0];
        normalize_per_feature(&mut audio, 2, frames);
        for bin in 0..2 {
            let row = &audio[bin * frames..(bin + 1) * frames];
            let mean = row.iter().sum::<f32>() / frames as f32;
            let var =
                row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / (frames as f32 - 1.0);
            assert!(mean.abs() < 1e-4, "bin {bin} mean {mean}");
            assert!((var - 1.0).abs() < 1e-3, "bin {bin} variance {var}");
        }
        assert!(
            (audio[0] - audio[frames]).abs() < 1e-4,
            "bins differing only by offset/scale must normalize identically"
        );
    }

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

    #[test]
    fn parse_tokens_splits_on_last_space() {
        let v = parse_tokens("\u{2581}the 5\n<blk> 1024\n").unwrap();
        assert_eq!(v[5], "\u{2581}the");
        assert_eq!(v[1024], "<blk>");
    }

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
