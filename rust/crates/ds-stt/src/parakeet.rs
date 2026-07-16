//! Parakeet — portable local STT: mic (cpal) → mono → 16 kHz (rubato) → cache-aware
//! streaming FastConformer ([`crate::streaming`]) over shared `ort`. Cross-platform
//! sibling of the macOS Core ML / ANE backend.
//!
//! This file owns mic [`Capture`] + [`resample`] and [`ParakeetTranscriber`] (one-shot
//! whole-buffer adapter). Live helper dictation drives [`StreamingModel`] INCREMENTALLY.
//!
//! Unlike ClaudeNative (PTT tap), this records audio and INJECTS via clipboard-paste
//! (`KeyInjector::type_text`), focus-gated so text never leaks outside a terminal.
//!
//! Caps edges: `start` opens mic; `stop` resamples + transcribes + pastes; `abort`
//! discards (§F long-press must not inject). Fail-quiets on device/model errors.
//! Model loads LAZILY on first transcription (~137 MB int8) so selecting Parakeet
//! never blocks config hot-reload.
//!
//! [`Capture`] and [`ParakeetTranscriber`] are public so "test recognition" can drive
//! the same engine without paste. `Capture` is `!Send` (cpal `Stream` on macOS);
//! `ParakeetTranscriber` is `Send`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::Sample as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler};

use crate::streaming::StreamingModel;

/// Parakeet expects 16 kHz mono f32 in [-1, 1].
const TARGET_RATE: u32 = 16_000;

/// rubato fixed input-chunk size (frames per `process`).
const RESAMPLE_CHUNK: usize = 1024;
/// Callback→consumer ring bound. Drains ~every 50 ms; 5 s room for hiccups without
/// unbounded growth.
const CAPTURE_BUFFER_SECS: usize = 5;

// ── Capture — live mic → mono PCM, drained / resampled on stop ───────────────

/// In-flight capture: cpal stream + mono PCM ring + native rate for stop-time resample.
/// `!Send` on macOS — open and consume on the same thread.
pub struct Capture {
    /// Dropping stops capture.
    _stream: cpal::Stream,
    /// Consumer of the lock-free callback ring. Mutex is consumer-only
    /// (`drain_new(&self)`); realtime producer never touches it.
    buffer: Mutex<HeapCons<f32>>,
    dropped: Arc<AtomicU64>,
    input_rate: u32,
}

impl Capture {
    /// Open default input and start buffering. Error string for fail-quiet logging.
    pub fn open() -> Result<Capture, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("default_input_config: {e}"))?;
        let sample_format = config.sample_format();
        // cpal 0.18: `SampleRate` is a plain `u32` alias (no tuple field).
        let input_rate = config.sample_rate();
        let channels = config.channels() as usize;
        let stream_config: cpal::StreamConfig = config.into();

        let capacity = input_rate as usize * CAPTURE_BUFFER_SECS;
        let (producer, consumer) = HeapRb::<f32>::new(capacity.max(1)).split();
        let dropped = Arc::new(AtomicU64::new(0));
        let stream = build_input_stream(
            &device,
            &stream_config,
            sample_format,
            channels,
            producer,
            dropped.clone(),
        )?;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;

        Ok(Capture {
            _stream: stream,
            buffer: Mutex::new(consumer),
            dropped,
            input_rate,
        })
    }

    /// Drain mono PCM since last call (device native rate); stream stays RUNNING.
    /// Always-listening loop uses this each poll tick (vs one-shot `into_pcm_16k`).
    pub fn drain_new(&self) -> Vec<f32> {
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            warn(&format!("capture ring overflow: dropped {dropped} samples"));
        }
        match self.buffer.lock() {
            Ok(mut b) => b.pop_iter().collect::<Vec<f32>>(),
            Err(e) => {
                warn(&format!("capture ring poisoned: {e}"));
                Vec::new()
            }
        }
    }

    /// Device native rate — for [`resample_to_16k`] and energy-frame timing.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Stop capture; return 16 kHz mono for [`ParakeetTranscriber::transcribe_pcm_16k`].
    pub fn into_pcm_16k(self) -> Vec<f32> {
        let Capture {
            _stream,
            buffer,
            dropped: _,
            input_rate,
        } = self;
        drop(_stream);
        let samples = match buffer.lock() {
            Ok(mut b) => b.pop_iter().collect::<Vec<f32>>(),
            Err(e) => {
                warn(&format!("buffer poisoned: {e}"));
                return Vec::new();
            }
        };
        resample_to_16k(&samples, input_rate)
    }
}

// ── Transcriber — lazy StreamingModel; 16 kHz mono PCM → text ────────────────

/// Cross-platform ONNX STT via cache-aware streaming FastConformer
/// ([`StreamingModel`]) — replaced the old whole-buffer `transcribe-rs` TDT engine.
/// Name kept for `built_in` engine / provider tokens / asset wiring. Lazy-load +
/// whole-buffer API; live helper drives incremental partials on the same model. `Send`.
pub struct ParakeetTranscriber {
    /// Flat dir: `encoder/decoder/joiner.int8.onnx` + `tokens.txt`.
    model_dir: PathBuf,
    /// Explicit provider for in-process users; `None` keeps helper env contract.
    provider: Option<String>,
    model: Option<StreamingModel>,
}

impl ParakeetTranscriber {
    /// Cheap: model not loaded until first preload/transcribe.
    pub fn new(model_dir: PathBuf) -> Self {
        Self {
            model_dir,
            provider: None,
            model: None,
        }
    }

    /// Pinned to a config-resolved provider token.
    pub fn for_provider(model_dir: PathBuf, provider: &str) -> Self {
        Self {
            model_dir,
            provider: Some(provider.to_string()),
            model: None,
        }
    }

    /// Lazy load int8 over shared ort (CPU EP — dynamic-quant ops aren't GPU-accelerated).
    fn model(&mut self) -> Result<&mut StreamingModel, String> {
        if self.model.is_none() {
            self.model = Some(match &self.provider {
                Some(provider) => {
                    StreamingModel::load_for_provider(&self.model_dir, true, provider)?
                }
                None => StreamingModel::load(&self.model_dir, true)?,
            });
        }
        Ok(self.model.as_mut().expect("model just loaded"))
    }

    /// Force-load now (idempotent).
    pub fn preload(&mut self) -> Result<(), String> {
        self.model().map(|_| ())
    }

    /// Realized ort EP; CPU before loaded.
    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.model
            .as_ref()
            .map(|m| m.provider())
            .unwrap_or(ds_config::RealizedProvider::Cpu)
    }

    /// Free cached model if loaded; returns whether anything was freed.
    pub fn unload(&mut self) -> bool {
        self.model.take().is_some()
    }

    /// Whole-buffer one-shot: accept all PCM then finalize. Empty → empty.
    /// Segment callers use this; live helper uses incremental path directly.
    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let model = self.model()?;
        let mut state = model.new_state()?;
        model.accept_16k(&mut state, pcm)?;
        model.finalize(&mut state)
    }
}

fn warn(msg: &str) {
    eprintln!("dontspeak/parakeet: {msg}");
}

/// cpal input stream: downmix each frame to mono f32 into the ring.
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    channels: usize,
    producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
) -> Result<cpal::Stream, String> {
    use cpal::SampleFormat as F;
    let r = match sample_format {
        F::F32 => build_typed::<f32>(device, config, channels, producer, dropped),
        F::I16 => build_typed::<i16>(device, config, channels, producer, dropped),
        F::U16 => build_typed::<u16>(device, config, channels, producer, dropped),
        F::I32 => build_typed::<i32>(device, config, channels, producer, dropped),
        F::I8 => build_typed::<i8>(device, config, channels, producer, dropped),
        F::U8 => build_typed::<u8>(device, config, channels, producer, dropped),
        other => return Err(format!("unsupported sample format {other:?}")),
    };
    r.map_err(|e| format!("build_input_stream: {e}"))
}

fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    dropped: Arc<AtomicU64>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let chans = channels.max(1);
    // cpal 0.18: StreamConfig by value; errors as cpal::Error.
    device.build_input_stream(
        *config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            for frame in data.chunks(chans) {
                let mut acc = 0.0f32;
                for &s in frame {
                    acc += f32::from_sample(s);
                }
                if producer.try_push(acc / frame.len() as f32).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        },
        |e| warn(&format!("cpal stream error: {e}")),
        None,
    )
}

/// Mono f32 `in_rate` → 16 kHz. Unchanged if already 16 kHz. Fail-quiet on rubato
/// error (partial beats none). rubato 3.0 whole-clip pattern: fixed chunks via
/// `process_into_buffer`, `partial_len` tail, then trim `output_delay()` priming.
pub fn resample_to_16k(input: &[f32], in_rate: u32) -> Vec<f32> {
    resample(input, in_rate, TARGET_RATE)
}

/// Mono f32 `in_rate` → `out_rate`. Separation path uses 16 kHz ↔ 8 kHz.
pub fn resample(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = out_rate as f64 / in_rate as f64;
    const CHANNELS: usize = 1;

    let mut resampler = match Async::<f32>::new_poly(
        ratio,
        1.1,
        PolynomialDegree::Septic,
        RESAMPLE_CHUNK,
        CHANNELS,
        FixedAsync::Input,
    ) {
        Ok(r) => r,
        Err(e) => {
            warn(&format!("resampler init: {e}"));
            return Vec::new();
        }
    };

    let in_frames = input.len();
    // Ideal count + slack for priming delay + final partial.
    let mut out = vec![0.0f32; (in_frames as f64 * ratio) as usize + 2 * RESAMPLE_CHUNK];
    let out_cap = out.len();

    let input_adapter = match InterleavedSlice::new(input, CHANNELS, in_frames) {
        Ok(a) => a,
        Err(e) => {
            warn(&format!("resample input adapter: {e}"));
            return Vec::new();
        }
    };
    let mut output_adapter = match InterleavedSlice::new_mut(&mut out, CHANNELS, out_cap) {
        Ok(a) => a,
        Err(e) => {
            warn(&format!("resample output adapter: {e}"));
            return Vec::new();
        }
    };

    let delay = resampler.output_delay();
    let mut indexing = Indexing {
        input_offset: 0,
        output_offset: 0,
        active_channels_mask: None,
        partial_len: None,
    };
    let mut frames_left = in_frames;
    let mut next_in = resampler.input_frames_next();

    while frames_left >= next_in {
        match resampler.process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing)) {
            Ok((nin, nout)) => {
                indexing.input_offset += nin;
                indexing.output_offset += nout;
                frames_left -= nin;
                next_in = resampler.input_frames_next();
            }
            Err(e) => {
                warn(&format!("resample: {e}"));
                break;
            }
        }
    }
    indexing.partial_len = Some(frames_left);
    match resampler.process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing)) {
        Ok((_nin, nout)) => indexing.output_offset += nout,
        Err(e) => warn(&format!("resample tail: {e}")),
    }

    let total = indexing.output_offset.min(out_cap);
    let start = delay.min(total);
    out.truncate(total);
    out.drain(..start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_passthrough_at_16k() {
        let pcm = vec![0.1f32, -0.2, 0.3, -0.4];
        assert_eq!(resample_to_16k(&pcm, TARGET_RATE), pcm);
    }

    #[test]
    fn resample_empty_is_empty() {
        assert!(resample_to_16k(&[], 48_000).is_empty());
    }

    #[test]
    fn resample_48k_to_16k_thirds_the_length() {
        // 48→16 kHz is 1:3; poly resampler has delay/edge frames — allow ~5% slack.
        let n = 48_000usize;
        let pcm: Vec<f32> = (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect();
        let out = resample_to_16k(&pcm, 48_000);
        let expected = n / 3;
        let tol = expected / 20;
        assert!(
            (out.len() as i64 - expected as i64).unsigned_abs() as usize <= tol,
            "got {} samples, expected ~{expected} (±{tol})",
            out.len()
        );
    }

    #[test]
    fn transcriber_empty_pcm_is_empty_text() {
        // Short-circuits before model load — safe without network/assets.
        let mut t = ParakeetTranscriber::new(PathBuf::from("/nonexistent"));
        assert_eq!(t.transcribe_pcm_16k(&[]).unwrap(), "");
    }
}
