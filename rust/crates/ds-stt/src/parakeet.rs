//! Parakeet (ONNX) — the portable local on-device STT engine: mic capture (cpal)
//! → mono → 16 kHz (rubato) → `transcribe-rs` `ParakeetModel` (TDT 0.6b v2 int8)
//! over the shared `ort` (load-dynamic) runtime, through the [`crate::Stt`] trait. The
//! cross-platform sibling of the macOS-only Core ML / ANE Parakeet backend.
//!
//! Unlike [`crate::claude_native::ClaudeNative`] (which drives Claude Code's
//! built-in voice via a push-to-talk tap), this engine records the audio itself and
//! INJECTS the transcript via the platform's clipboard-paste (`KeyInjector::type_text`),
//! focus-gated so a transcript never leaks outside a terminal.
//!
//! Lifecycle on the engine's Caps-Lock edges:
//!   * `start()`  — open the default input device and begin buffering mono PCM.
//!   * `stop()`   — stop capture, resample to 16 kHz, run Parakeet, paste the text.
//!   * `abort()`  — stop capture and DISCARD (the §F long-press reset must not
//!     inject).
//!
//! Everything fail-quiets: any device/model/inference error logs and drops the
//! capture without injecting. The model is loaded LAZILY on the first transcription
//! (~660 MB int8 ONNX), so selecting Parakeet never blocks the config hot-reload and
//! the first dictation pays the one-time load.
//!
//! The reusable pieces — [`Capture`] (mic → 16 kHz mono PCM) and
//! [`ParakeetTranscriber`] (PCM → text) — are public so the engine's in-process
//! "test recognition" path can drive the SAME engine without the paste step.
//!
//! `Stt` is non-`Send`, and so is this engine: it borrows the engine-owned
//! platform through an `Rc`. `ParakeetTranscriber` IS `Send` (the engine's test
//! session holds it across threads), but the cpal `Stream` inside `Capture` is
//! `!Send` on macOS — a `Capture` is therefore created and consumed on one thread.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cpal::Sample as _;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, Indexing, PolynomialDegree, Resampler};
use transcribe_rs::onnx::Quantization;
use transcribe_rs::onnx::parakeet::ParakeetModel;
use transcribe_rs::{SpeechModel, TranscribeOptions};

/// The sample rate Parakeet expects (16 kHz mono, f32 in [-1, 1]).
const TARGET_RATE: u32 = 16_000;

/// rubato fixed input-chunk size (frames per `process` call). 1024 keeps the
/// resampler's internal buffers small while still amortizing the per-call cost.
const RESAMPLE_CHUNK: usize = 1024;

// ─────────────────────────────────────────────────────────────────────────────
// Capture — a live mic stream accumulating mono PCM, drained to 16 kHz on stop.
// ─────────────────────────────────────────────────────────────────────────────

/// An in-flight capture: the live cpal stream and the mono PCM it appends to, plus
/// the device's native sample rate (for the stop-time resample to 16 kHz).
///
/// `!Send` (the cpal `Stream` is `!Send` on macOS): open and consume it on the
/// same thread.
pub struct Capture {
    /// Held so the stream keeps running; dropping it stops capture.
    _stream: cpal::Stream,
    /// Mono f32 PCM accumulated by the cpal data callback (downmixed from however
    /// many channels the device delivers).
    buffer: Arc<Mutex<Vec<f32>>>,
    input_rate: u32,
}

impl Capture {
    /// Open the default input device and start buffering mono PCM. Returns an
    /// error string (for fail-quiet logging) on any device error.
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

        let buffer = Arc::new(Mutex::new(Vec::<f32>::new()));
        let stream = build_input_stream(
            &device,
            &stream_config,
            sample_format,
            channels,
            buffer.clone(),
        )?;
        stream.play().map_err(|e| format!("stream.play: {e}"))?;

        Ok(Capture {
            _stream: stream,
            buffer,
            input_rate,
        })
    }

    /// Drain the mono PCM accumulated since the last call (at the device's native
    /// rate), leaving the stream RUNNING. Empty when no new audio has arrived. The
    /// always-listening loop calls this each poll tick to feed the energy-VAD and
    /// accumulate the current utterance, instead of the one-shot `into_pcm_16k`.
    pub fn drain_new(&self) -> Vec<f32> {
        match self.buffer.lock() {
            Ok(mut b) => std::mem::take(&mut *b),
            Err(_) => Vec::new(),
        }
    }

    /// The device's native input sample rate — needed to resample a drained
    /// segment to 16 kHz (via [`resample_to_16k`]) and to time energy frames.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Stop capture (drops the stream) and return the recorded audio as 16 kHz mono
    /// f32 — ready for [`ParakeetTranscriber::transcribe_pcm_16k`].
    pub fn into_pcm_16k(self) -> Vec<f32> {
        let Capture {
            _stream,
            buffer,
            input_rate,
        } = self;
        drop(_stream);
        let samples = match buffer.lock() {
            Ok(b) => b.clone(),
            Err(e) => {
                warn(&format!("buffer poisoned: {e}"));
                return Vec::new();
            }
        };
        resample_to_16k(&samples, input_rate)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Transcriber — owns the lazily-loaded Parakeet model; PCM (16 kHz mono) → text.
// ─────────────────────────────────────────────────────────────────────────────

/// Holds the Parakeet model (loaded lazily on first use) and turns 16 kHz mono
/// PCM into text. `Send`, so the engine's test session can hold it across threads.
pub struct ParakeetTranscriber {
    /// The dir holding `encoder-model.int8.onnx` / `decoder_joint-model.int8.onnx`
    /// / `nemo128.onnx` / `vocab.txt` (the flat `model_dir()`).
    model_dir: PathBuf,
    /// Lazily loaded on first transcription; cached for subsequent calls.
    model: Option<ParakeetModel>,
}

impl ParakeetTranscriber {
    /// Build a transcriber for the Parakeet assets in `model_dir`. Cheap — the
    /// model is not loaded until the first [`transcribe_pcm_16k`](Self::transcribe_pcm_16k).
    pub fn new(model_dir: PathBuf) -> Self {
        Self {
            model_dir,
            model: None,
        }
    }

    /// Lazily load the Parakeet model (TDT 0.6b v2, int8) over the shared
    /// onnxruntime. Points `ort` (load-dynamic) at the dylib first. Returns a
    /// mutable ref to the cached model, or an error string (logged) if loading fails.
    fn model(&mut self) -> Result<&mut ParakeetModel, String> {
        if self.model.is_none() {
            // Point `ort` (load-dynamic) at the runtime via the SHARED GPU-aware bootstrap
            // — the SAME one the Kokoro-ONNX TTS path uses, so both engines run over one
            // ort runtime: the Windows CUDA GPU onnxruntime when STT prefers CUDA and its
            // runtime is present, else the version-checked CPU dylib. (In the warm helper
            // whichever engine loads first selects the dylib; this is idempotent, and
            // correct standalone.)
            let use_cuda = stt_wants_cuda();
            ds_model::ensure_ort_dylib_gpu(use_cuda)?;
            // transcribe-rs's accelerator is GLOBAL and best-effort: `Cuda` registers the
            // CUDA EP for Parakeet's sessions, falling back to CPU (with a log warning) if
            // the GPU runtime/driver is unavailable — so this never breaks dictation. Set it
            // EXPLICITLY in BOTH directions (kept consistent with the dylib chosen above):
            // `Cuda` only when the GPU dylib was selected, else `CpuOnly` so a CPU config
            // never probes for a GPU. This is the STT analogue of synth.rs's Kokoro CUDA EP.
            transcribe_rs::set_ort_accelerator(if use_cuda {
                transcribe_rs::OrtAccelerator::Cuda
            } else {
                transcribe_rs::OrtAccelerator::CpuOnly
            });
            let m = ParakeetModel::load(&self.model_dir, &Quantization::Int8)
                .map_err(|e| format!("model load: {e}"))?;
            self.model = Some(m);
        }
        Ok(self.model.as_mut().expect("model just loaded"))
    }

    /// Force-load the model now (idempotent) so it's resident before the first
    /// transcription — the eager counterpart to [`unload`](Self::unload). Lets the
    /// helper preload Parakeet the moment it's the selected engine, so "loaded"
    /// reflects residency instead of waiting for the first dictation.
    pub fn preload(&mut self) -> Result<(), String> {
        self.model().map(|_| ())
    }

    /// Free the cached model if loaded, returning whether anything was freed. The
    /// next [`transcribe_pcm_16k`](Self::transcribe_pcm_16k) lazily reloads it. Lets
    /// the warm helper reclaim the STT model's RAM when dictation switches off
    /// Parakeet while the helper stays warm for TTS.
    pub fn unload(&mut self) -> bool {
        self.model.take().is_some()
    }

    /// Transcribe 16 kHz mono f32 PCM to trimmed text. Empty input → empty string.
    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let model = self.model()?;
        let result = model
            .transcribe(pcm, &TranscribeOptions::default())
            .map_err(|e| format!("transcribe: {e}"))?;
        Ok(result.text.trim().to_string())
    }
}

/// Whether the Parakeet STT path should run on the CUDA GPU: the RESOLVED STT provider
/// (carried via `DONTSPEAK_STT_PROVIDER`, set by the engine from `Provider::resolved_stt`) is
/// `ort_cuda` AND the GPU runtime is fetched. Gated to Windows/Linux x86_64 — the platforms
/// with a downloadable CUDA runtime; everywhere else the CPU (and, on macOS, the native ANE)
/// paths handle STT. Mirrors the Kokoro `want_gpu` check in the TTS loader, reading the STT
/// env instead of `DONTSPEAK_PROVIDER`.
fn stt_wants_cuda() -> bool {
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
    {
        std::env::var("DONTSPEAK_STT_PROVIDER")
            .map(|p| p.eq_ignore_ascii_case("ort_cuda"))
            .unwrap_or(false)
            && ds_model::cuda_runtime_present()
    }
    #[cfg(not(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64")))]
    {
        false
    }
}

fn warn(msg: &str) {
    eprintln!("dontspeak/parakeet: {msg}");
}

/// Build a cpal input stream for `sample_format`, downmixing every frame to mono
/// f32 and appending it to `buffer`. Dispatches the generic builder on the device
/// sample format (the common PCM widths cpal can deliver).
fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, String> {
    use cpal::SampleFormat as F;
    let r = match sample_format {
        F::F32 => build_typed::<f32>(device, config, channels, buffer),
        F::I16 => build_typed::<i16>(device, config, channels, buffer),
        F::U16 => build_typed::<u16>(device, config, channels, buffer),
        F::I32 => build_typed::<i32>(device, config, channels, buffer),
        F::I8 => build_typed::<i8>(device, config, channels, buffer),
        F::U8 => build_typed::<u8>(device, config, channels, buffer),
        other => return Err(format!("unsupported sample format {other:?}")),
    };
    r.map_err(|e| format!("build_input_stream: {e}"))
}

/// The monomorphized input-stream builder: each interleaved frame is averaged
/// across channels into one mono f32 sample.
fn build_typed<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let chans = channels.max(1);
    // cpal 0.18 takes `StreamConfig` by value and errors with `cpal::Error`.
    device.build_input_stream(
        *config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if let Ok(mut buf) = buffer.lock() {
                buf.reserve(data.len() / chans + 1);
                for frame in data.chunks(chans) {
                    let mut acc = 0.0f32;
                    for &s in frame {
                        acc += f32::from_sample(s);
                    }
                    buf.push(acc / chans as f32);
                }
            }
        },
        |e| warn(&format!("cpal stream error: {e}")),
        None,
    )
}

/// Resample a mono f32 buffer from `in_rate` to 16 kHz with rubato's polynomial
/// (`PolyFixedInput`) resampler. Returns the input unchanged when already at
/// 16 kHz. On any rubato error it logs and returns what was produced so far
/// (fail-quiet — a partial transcript beats none).
///
/// Follows rubato 3.0's canonical whole-clip pattern (the `process_f64` example):
/// drive fixed-size input chunks via `process_into_buffer` advancing an `Indexing`
/// offset, finish with one `partial_len` chunk for the tail, then trim the
/// resampler's `output_delay()` priming frames off the front.
pub fn resample_to_16k(input: &[f32], in_rate: u32) -> Vec<f32> {
    resample(input, in_rate, TARGET_RATE)
}

/// Resample a mono f32 buffer from `in_rate` to an arbitrary `out_rate` (the generic
/// form of [`resample_to_16k`]). Used by the speaker-separation path to go 16 kHz →
/// 8 kHz (the separator's native rate) and back. Same fail-quiet rubato pattern.
pub fn resample(input: &[f32], in_rate: u32, out_rate: u32) -> Vec<f32> {
    if in_rate == out_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = out_rate as f64 / in_rate as f64;
    const CHANNELS: usize = 1; // mono

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

    let in_frames = input.len(); // mono ⇒ frames == samples
    // Generous output capacity (ideal count + a couple of chunks of slack for the
    // priming delay + the final partial chunk).
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

    // Full fixed-size input chunks.
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
    // Final partial chunk (< one input chunk): the resampler zero-pads the rest.
    indexing.partial_len = Some(frames_left);
    match resampler.process_into_buffer(&input_adapter, &mut output_adapter, Some(&indexing)) {
        Ok((_nin, nout)) => indexing.output_offset += nout,
        Err(e) => warn(&format!("resample tail: {e}")),
    }

    // Trim the resampler's priming delay off the front; the rest is real audio.
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
        // 48 kHz → 16 kHz is a 1:3 decimation; a 1 s ramp (~48000 samples) should
        // come out ~16000 samples (allow a small resampler edge tolerance).
        let n = 48_000usize;
        let pcm: Vec<f32> = (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect();
        let out = resample_to_16k(&pcm, 48_000);
        let expected = n / 3;
        // The polynomial resampler emits a few hundred extra delay/edge frames on
        // flush; allow ~5% slack around the ideal 1:3 decimation.
        let tol = expected / 20;
        assert!(
            (out.len() as i64 - expected as i64).unsigned_abs() as usize <= tol,
            "got {} samples, expected ~{expected} (±{tol})",
            out.len()
        );
    }

    #[test]
    fn transcriber_empty_pcm_is_empty_text() {
        // Empty PCM short-circuits before any model load, so this is network/model
        // free and safe in tests.
        let mut t = ParakeetTranscriber::new(PathBuf::from("/nonexistent"));
        assert_eq!(t.transcribe_pcm_16k(&[]).unwrap(), "");
    }
}
