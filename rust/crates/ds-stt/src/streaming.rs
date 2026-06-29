//! Cache-aware STREAMING Parakeet/FastConformer transducer over `ort`.
//!
//! The offline [`crate::parakeet`] path re-encodes the whole buffer on every preview tick
//! (`transcribe-rs` `ParakeetModel`, `supports_streaming: false`). This module instead feeds
//! audio to a *cache-aware* NeMo FastConformer encoder in fixed chunks, threading the encoder
//! cache so each frame is encoded EXACTLY ONCE — the fix planned in `docs/STREAMING-STT-PLAN.md`
//! and prototyped/validated in `scripts/streaming-stt/`.
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
    meta: Meta,
    tokens: Vec<String>,
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
    // Capture is at the mic's native rate; the fbank needs a continuous 16 kHz stream. We keep
    // the whole device-rate buffer and re-resample it each accept (one-shot, artifact-free),
    // feeding the fbank only the NEW frames up to a tail margin (the resampler's last samples
    // shift as more audio arrives, so the freshest ~`TAIL_MARGIN_16K` are withheld until they
    // become interior, or until `finalize`). Cheap relative to the encoder.
    in_rate: u32,
    dev_buf: Vec<f32>,  // all device-rate mono samples captured so far
    fed_16k: usize,     // count of 16 kHz samples already pushed to the fbank
    audio_ms: f64,      // 16 kHz audio fed (for STTSTATS)
    transcribe_ms: f64, // cumulative encoder+decode model time (for STTSTATS)
}

/// 16 kHz samples withheld from the fbank at the tail of each accept (the resampler's edge
/// samples are unstable until more input arrives). ~30 ms.
const TAIL_MARGIN_16K: usize = 480;

fn meta_str(s: &Session, key: &str) -> Option<String> {
    s.metadata().ok().and_then(|m| m.custom(key))
}
fn meta_usize(s: &Session, key: &str, default: usize) -> usize {
    meta_str(s, key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn build(path: &Path) -> Result<Session, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    Session::builder()
        .map_err(|e| format!("ort builder: {e}"))?
        .commit_from_memory(&bytes)
        .map_err(|e| format!("ort load {}: {e}", path.display()))
}

impl StreamingModel {
    /// Load the encoder/decoder/joiner ONNX (int8 by default) + `tokens.txt` from `dir`.
    /// `int8` picks `*.int8.onnx`; otherwise the fp32 `*.onnx`.
    pub fn load(dir: &Path, int8: bool) -> Result<Self, String> {
        ds_model::ensure_ort_dylib()?;
        let sfx = if int8 { ".int8" } else { "" };
        let encoder = build(&dir.join(format!("encoder{sfx}.onnx")))?;
        let decoder = build(&dir.join(format!("decoder{sfx}.onnx")))?;
        let joiner = build(&dir.join(format!("joiner{sfx}.onnx")))?;

        let vocab = meta_usize(&encoder, "vocab_size", 1024);
        let meta = Meta {
            window_size: meta_usize(&encoder, "window_size", 65),
            chunk_shift: meta_usize(&encoder, "chunk_shift", 56),
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
        let dec_out_names = decoder
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();

        let tokens_path = dir.join("tokens.txt");
        let tokens = parse_tokens(
            &std::fs::read_to_string(&tokens_path)
                .map_err(|e| format!("read {}: {e}", tokens_path.display()))?,
        );
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
            meta,
            tokens,
        })
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
    /// step on the blank/SOS token (mirrors the reference).
    /// `in_rate` is the capture (mic) sample rate; audio fed to [`accept_audio`] is at this rate
    /// and resampled to 16 kHz internally (passthrough when already 16 kHz).
    pub fn new_state(&mut self, in_rate: u32) -> Result<StreamingState, String> {
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
            in_rate,
            dev_buf: Vec::new(),
            fed_16k: 0,
            audio_ms: 0.0,
            transcribe_ms: 0.0,
        })
    }

    /// Feed a chunk of mono audio at the capture rate (`new_state`'s `in_rate`); resamples to
    /// 16 kHz, feeds the fbank (withholding the unstable tail), runs any newly-available encoder
    /// windows, and returns the hypothesis text so far. Cheap to call often.
    pub fn accept_audio(
        &mut self,
        state: &mut StreamingState,
        pcm: &[f32],
    ) -> Result<String, String> {
        state.dev_buf.extend_from_slice(pcm);
        let full = crate::resample(&state.dev_buf, state.in_rate, 16_000);
        let stable = full.len().saturating_sub(TAIL_MARGIN_16K);
        if stable > state.fed_16k {
            let new = &full[state.fed_16k..stable];
            state.fbank.accept_waveform(16_000.0, new);
            state.audio_ms += new.len() as f64 / 16.0;
            state.fed_16k = stable;
            self.drain_windows(state, false)?;
        }
        Ok(self.text(state))
    }

    /// Flush: resample the whole buffer, feed the withheld tail, run the remaining (zero-padded)
    /// windows, and return the final text.
    pub fn finalize(&mut self, state: &mut StreamingState) -> Result<String, String> {
        let full = crate::resample(&state.dev_buf, state.in_rate, 16_000);
        if full.len() > state.fed_16k {
            let new = &full[state.fed_16k..];
            state.fbank.accept_waveform(16_000.0, new);
            state.audio_ms += new.len() as f64 / 16.0;
            state.fed_16k = full.len();
        }
        state.fbank.input_finished();
        self.drain_windows(state, true)?;
        Ok(self.text(state))
    }

    /// 16 kHz audio duration fed so far, in ms (for STTSTATS).
    pub fn audio_ms(state: &StreamingState) -> f64 {
        state.audio_ms
    }
    /// Cumulative encoder+decode model time, in ms (for STTSTATS).
    pub fn transcribe_ms(state: &StreamingState) -> f64 {
        state.transcribe_ms
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
        for t in 0..t_out {
            let mut col = vec![0.0f32; d];
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
                "targets" => targets,
                "target_length" => tlen,
                "states.1" => h_t,
                "onnx::LSTM_3" => c_t,
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
            }
        }
        s.replace('\u{2581}', " ").trim().to_string()
    }
}

/// Parse `tokens.txt`: each line `token<space>id`; index = id (line order is id order here).
fn parse_tokens(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.rsplit_once(' ').map(|(t, _)| t).unwrap_or(l).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let v = parse_tokens("\u{2581}the 5\n<blk> 1024\n");
        assert_eq!(v[0], "\u{2581}the");
        assert_eq!(v[1], "<blk>");
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
        let mut st = model.new_state(16_000).expect("state");
        model.accept_audio(&mut st, &pcm).expect("accept");
        let text = model.finalize(&mut st).expect("finalize");
        eprintln!("streaming oracle => {text:?}");
        assert!(
            text.contains("after early nightfall the yellow lamps"),
            "unexpected transcript: {text:?}"
        );
    }
}
