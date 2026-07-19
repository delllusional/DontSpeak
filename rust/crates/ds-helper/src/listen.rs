//! STT capture: [`run_listen`] / [`run_concurrent_listen`], gain, silence trim.
//! Steady path is [`try_streaming`] ([`StreamingStt`]); [`transcribe_loop`] only if
//! streaming backend fails to build.

use std::sync::{Mutex, OnceLock};

use ds_aec::CaptureHandle;
use ds_helper_proto as proto;
use ds_stt::{OnnxStreamer, StreamSession, StreamingStt};

const AUTO_GAIN_NOISE_FLOOR: f32 = 0.02;
const AUTO_GAIN_TARGET_PEAK: f32 = 0.9;
const AUTO_GAIN_MIN: f32 = 0.5;
const AUTO_GAIN_MAX: f32 = 15.0;

fn generation_stopped(stopped_through: &std::sync::atomic::AtomicU64, generation: u64) -> bool {
    use std::sync::atomic::Ordering;
    stopped_through.load(Ordering::SeqCst) >= generation
}

/// Full-duplex listen thread control (reader sets; thread polls). `start` opens; `quit` tears down.
/// Stop is via generation `stopped_through` (`lstop`).
#[derive(Default)]
pub(crate) struct ListenSig {
    pub(crate) start: Option<u64>,
    pub(crate) quit: bool,
}

/// Concurrent listen alongside TTS render (coexist). Terminal is `LDONE` (never `DONE`)
/// so the engine demuxes dictation from speak on shared stdout.
pub(crate) fn concurrent_listen_loop(
    capture: CaptureHandle,
    transcriber: std::sync::Arc<std::sync::Mutex<ds_stt::LocalTranscriber>>,
    sig: std::sync::Arc<(std::sync::Mutex<ListenSig>, std::sync::Condvar)>,
    stopped_through: std::sync::Arc<std::sync::atomic::AtomicU64>,
    capturing_diarize: std::sync::Arc<std::sync::atomic::AtomicBool>,
    listening: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;

    crate::priority::elevate_current_thread();
    loop {
        let generation = {
            let (m, cv) = &*sig;
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            while s.start.is_none() && !s.quit {
                s = cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            if s.quit {
                return;
            }
            s.start.take().expect("listen generation set")
        };
        listening.store(true, Ordering::SeqCst);
        while capturing_diarize.load(Ordering::SeqCst)
            && !generation_stopped(&stopped_through, generation)
        {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        if generation_stopped(&stopped_through, generation) {
            emit_empty_final();
        } else {
            run_concurrent_listen(&capture, &transcriber, &sig, &stopped_through, generation);
        }
        listening.store(false, Ordering::SeqCst);
    }
}

fn emit_empty_final() {
    use std::io::Write as _;
    println!("{}", proto::FINAL_PREFIX);
    println!("{}", proto::LDONE);
    let _ = std::io::stdout().flush();
}

/// One-shot peak-normalize for PTT (`capture_gain = "auto"`). Noise-floor gate: never amplify silence.
fn auto_gain(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 1.0;
    }
    let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak < AUTO_GAIN_NOISE_FLOOR {
        return 1.0;
    }
    (AUTO_GAIN_TARGET_PEAK / peak).clamp(AUTO_GAIN_MIN, AUTO_GAIN_MAX)
}

/// Min 16 kHz segment length (0.3 s = FluidAudio `minimumAudioDurationSeconds` / Parakeet guard).
const MIN_TRANSCRIBE_SAMPLES_16K: usize = 4800;

/// Live-preview open-tail budget = VAD force-split ([`ds_stt::boundary::MAX_SEGMENT_SECS`]).
/// Do not hardcode seconds: separate preview cap vs split used to blank the overlay mid-phrase.
fn tail_preview_budget_samples(rate: u32) -> usize {
    ds_stt::boundary::MAX_SEGMENT_SECS * rate as usize
}

fn tail_previewable(tail_len: usize, rate: u32) -> bool {
    tail_len > 0 && tail_len <= tail_preview_budget_samples(rate)
}

/// Streaming capture gain: manual exact; auto decays peak, cuts hot fast / raises quiet slow (no pump).
struct StreamingGain {
    mode: ds_config::CaptureGain,
    peak: f32,
    gain: f32,
}

impl StreamingGain {
    fn from_config() -> Self {
        let mode = ds_config::Paths::resolve()
            .map(|p| ds_config::VoiceConfig::load(&p).capture_gain)
            .unwrap_or(ds_config::CaptureGain::Auto);
        Self {
            mode,
            peak: 0.0,
            gain: 1.0,
        }
    }

    fn apply(&mut self, input: &[f32]) -> Vec<f32> {
        let block_peak = input.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        let gain = if let Some(manual) = self.mode.manual() {
            manual
        } else {
            if block_peak < AUTO_GAIN_NOISE_FLOOR {
                return input.to_vec();
            }
            self.peak = (self.peak * 0.995).max(block_peak);
            let wanted = (AUTO_GAIN_TARGET_PEAK / self.peak).clamp(AUTO_GAIN_MIN, AUTO_GAIN_MAX);
            let blend = if wanted < self.gain { 0.65 } else { 0.15 };
            self.gain += (wanted - self.gain) * blend;
            self.gain
        };
        if (gain - 1.0).abs() <= f32::EPSILON {
            return input.to_vec();
        }
        input.iter().map(|s| (s * gain).clamp(-1.0, 1.0)).collect()
    }
}

/// Pure PARTIAL line: committed segments + optional tail; `None` if unchanged/empty.
fn next_overlay(committed: &[String], tail: Option<&str>, last_text: &str) -> Option<String> {
    let mut shown: Vec<&str> = committed.iter().map(String::as_str).collect();
    if let Some(t) = tail {
        shown.push(t);
    }
    let merged = shown.join(" ");
    if merged != last_text && !merged.trim().is_empty() {
        Some(merged)
    } else {
        None
    }
}

/// Shared offline-fallback listen loop for half- and full-duplex (no drift in cadence/trim/partials).
/// Callers supply rate/timeout/drain/stopped/transcribe. Emits LISTENING → PARTIAL* → STTSTATS/FINAL/LDONE.
/// Partials = closed VAD segments + open-tail re-pass; final = remaining segment only.
fn transcribe_loop(
    rate: u32,
    timeout: std::time::Duration,
    label: &str,
    mut drain: impl FnMut() -> Vec<f32>,
    stopped: impl Fn() -> bool,
    mut transcribe: impl FnMut(&[f32]) -> Option<String>,
    // Final 16 kHz only (partials unfiltered): speaker-lock; identity when off.
    filter_final: impl Fn(&[f32]) -> Vec<f32>,
) {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let flush = || {
        let _ = std::io::stdout().flush();
    };
    // Make-up gain (config `capture_gain`, read once per listen): "auto" (default)
    // normalizes each utterance to a target level — machine- AND mode-independent, so it
    // gives the half-duplex path the level-consistency VPIO's AGC provides in full-duplex
    // — or a fixed manual multiplier. Applied to the WHOLE buffer at transcribe time
    // (auto needs the full buffer to measure its peak), so we accumulate RAW below.
    let gain_cfg = ds_config::Paths::resolve()
        .map(|p| ds_config::VoiceConfig::load(&p).capture_gain)
        .unwrap_or(ds_config::CaptureGain::Auto);
    // Gain for `buf`: the fixed manual multiplier, or the auto-normalizer's peak-to-
    // target factor (1.0 for a buffer below the noise floor — never amplify silence).
    let gain_of = |buf: &[f32]| -> f32 { gain_cfg.manual().unwrap_or_else(|| auto_gain(buf)) };
    let apply_gain = |buf: &[f32]| -> Vec<f32> {
        let g = gain_of(buf);
        if (g - 1.0).abs() <= f32::EPSILON {
            return buf.to_vec();
        }
        buf.iter().map(|s| (s * g).clamp(-1.0, 1.0)).collect()
    };

    let _ = drain(); // drop stale pre-listen audio
    println!("{}", proto::LISTENING);
    flush();

    // Streaming dictation: keep the full capture buffer but cut it at speech→silence
    // boundaries and transcribe each CLOSED segment WHILE the user keeps talking, so at
    // stop only the short final segment is left. The old code re-ran Parakeet over the
    // WHOLE growing buffer every 350 ms AND once more at stop — O(n²) work and a stop-
    // latency of rtf × full-duration (the lag felt on the second Caps tap). Because we
    // still own every sample (`accum`) and only slice it, a session where the detector
    // never fires degrades to one whole-buffer pass — never worse than before. See
    // `VadBoundaryDetector`.
    let mut accum: Vec<f32> = Vec::new(); // raw capture, device rate
    let mut detector = ds_stt::VadBoundaryDetector::new(rate);
    let mut committed_until = 0usize; // accum index transcribed+committed so far
    let mut committed: Vec<String> = Vec::new(); // finalized segment texts, in order
    let started = Instant::now();
    let mut last_partial = Instant::now();
    let mut last_text = String::new();
    let mut partials = 0u32;
    let mut total_transcribe_ms = 0f64;
    // Live-preview transcription time (the tail re-pass). Kept SEPARATE from
    // `total_transcribe_ms` (committed-segment + final work only) so the debug STTSTATS line
    // shows how much GPU/CPU the streaming overlay costs vs the real transcript.
    let mut total_preview_ms = 0f64;
    // Live-preview pacing. Base cadence for a short tail, with an ADAPTIVE BACK-OFF: the next
    // re-pass waits at least as long as the last one took (clamped), so an expensive pass on a
    // long tail self-throttles instead of re-running the whole tail dozens of times — preview
    // can't exceed ~half the session. The tail is still previewed WHOLE (no length cap), so the
    // overlay never blanks (see `tail_preview_budget_samples`). `last_preview_at` fingerprints
    // the audio so an unchanged tail (no new samples) isn't re-transcribed at all.
    let base_cadence = Duration::from_millis(180);
    // Upper bound on the adaptive back-off: even on a long open tail the overlay refreshes at
    // least this often, so the per-word blur stays reasonably fluid instead of snapping in ~1.5 s
    // chunks. Lowered from 1500 ms — the trade is a little more GPU on long, pause-free phrases.
    let preview_ceiling = Duration::from_millis(700);
    let mut preview_cadence = base_cadence;
    let mut last_preview_at = (usize::MAX, 0usize); // (committed_until, tail_len) of the last pass

    // Transcribe one device-rate segment through the SAME pipeline the old final pass
    // used (gain → resample → speaker-lock → trim → model), now applied per segment.
    // Returns single-line trimmed text, or None for empty/silence. Accrues `timer` ms.
    let mut segment_text = |seg: &[f32], timer: &mut f64| -> Option<String> {
        if seg.is_empty() {
            return None;
        }
        let pcm = ds_stt::resample_to_16k(&apply_gain(seg), rate);
        let pcm = filter_final(&pcm); // speaker lock (identity when off)
        let pcm = trim_silence_16k(&pcm);
        // Below the model's minimum input length there's nothing to transcribe — and FEEDING it
        // is actively harmful: FluidAudio's Parakeet REJECTS clips under `minimumAudioDurationSeconds`
        // (0.3 s) with `invalidAudioData`, so a short/silence-heavy tail re-pass would just log an
        // error and waste the pass (no overlay update → choppier blur). Skip it. 0.3 s @ 16 kHz.
        if pcm.len() < MIN_TRANSCRIBE_SAMPLES_16K {
            return None;
        }
        let t0 = Instant::now();
        let text = transcribe(pcm);
        *timer += t0.elapsed().as_secs_f64() * 1000.0;
        text.map(|t| t.trim().replace('\n', " "))
            .filter(|t| !t.is_empty())
    };

    while !stopped() && started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(50));
        let block = drain();
        if !block.is_empty() {
            accum.extend_from_slice(&block);
            for b in detector.feed(&block) {
                let b = b.min(accum.len());
                if b > committed_until {
                    if let Some(text) =
                        segment_text(&accum[committed_until..b], &mut total_transcribe_ms)
                    {
                        committed.push(text);
                    }
                    committed_until = b;
                }
            }
        }
        // Live partial: finalized segments, plus a re-pass of the still-open tail (force-split
        // bounds its length). The tail is NOT committed here. The cadence is adaptive (see the
        // `preview_cadence` setup above): a short tail keeps the snappy 180 ms beat so the
        // overlay tracks speech; a long tail throttles so the GPU isn't burned on repeated
        // full-tail re-passes. The dedup below (`merged != last_text`) still drops no-change
        // repeats, and the `last_preview_at` fingerprint skips re-running an unchanged tail.
        if last_partial.elapsed() >= preview_cadence {
            let tail = &accum[committed_until.min(accum.len())..];
            let fingerprint = (committed_until, tail.len());
            if tail_previewable(tail.len(), rate) && fingerprint != last_preview_at {
                let t0 = Instant::now();
                let tail_text = segment_text(tail, &mut total_preview_ms);
                // Back-off: next re-pass waits at least this pass's duration (≤ half the time
                // on previews), clamped to a responsiveness ceiling so the overlay still
                // refreshes at least every `preview_ceiling`.
                preview_cadence = t0.elapsed().clamp(base_cadence, preview_ceiling);
                last_preview_at = fingerprint;
                if let Some(merged) = next_overlay(&committed, tail_text.as_deref(), &last_text) {
                    println!("{}{merged}", proto::PARTIAL_PREFIX);
                    flush();
                    last_text = merged;
                    partials += 1;
                }
            } else {
                // Nothing new to preview (empty/over-budget tail, or no new audio since the
                // last pass): relax to the base cadence and try again next tick.
                preview_cadence = base_cadence;
            }
            last_partial = Instant::now();
        }
    }

    // Final pass: drain the tail, then finalize only the SHORT remaining segment past the
    // last boundary (not the whole buffer). Timed from here (`final_start`) so STTSTATS can
    // report the stop→FINAL latency — the lag felt on the second Caps tap — apart from the
    // capture phase.
    let final_start = Instant::now();
    accum.extend_from_slice(&drain());
    let final_gain = gain_of(&accum);
    // DONTSPEAK_LISTEN_DUMP=1 → write the full 16 kHz buffer Parakeet effectively saw.
    if std::env::var_os("DONTSPEAK_LISTEN_DUMP").is_some() {
        let dump = ds_stt::resample_to_16k(&apply_gain(&accum), rate);
        let path = std::env::temp_dir().join("ds-listen.wav");
        match ds_tts::wav::write_wav16(&path, &dump, 16_000) {
            Ok(()) => log::info!(target: "helper", "{label}: dumped → {}", path.display()),
            Err(e) => log::warn!(target: "helper", "{label}: wav dump failed: {e}"),
        }
    }
    if committed_until < accum.len()
        && let Some(text) = segment_text(&accum[committed_until..], &mut total_transcribe_ms)
    {
        committed.push(text);
    }
    let text = committed.join(" ");
    let final_ms = final_start.elapsed().as_secs_f64() * 1000.0;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;

    // Diagnostics (→ helper log): RMS of the captured audio, sample counts, segment +
    // partial counts, and the resolved gain. A near-zero RMS means silence reached the
    // mic path (AEC over-cancelling, or no mic grant) — the empty-transcript case.
    let audio_ms = accum.len() as f64 / rate as f64 * 1000.0;
    let rms = if accum.is_empty() {
        0.0
    } else {
        (accum.iter().map(|x| x * x).sum::<f32>() / accum.len() as f32).sqrt()
    };
    // Per-session summary — fires once per listen turn, routine — same DONTSPEAK_DEBUG gate
    // as the engine's own Debug lines, off by default.
    log::debug!(
        target: "helper",
        "{label}: rate={rate} accum={} segments={} rms={rms:.4} partials={partials} gain={final_gain:.1}",
        accum.len(),
        committed.len(),
    );
    // STTSTATS → the engine's stats + (in DONTSPEAK_DEBUG) the activity log. The first two
    // fields feed the in-app stats; the rest are diagnostics for the speech-IN latency budget
    // (the engine parser ignores unknown tokens, so adding fields is backward-compatible):
    //   wall_ms     total LISTENING→FINAL wall time (the felt latency)
    //   final_ms    stop→FINAL (drain + last-segment transcribe) — the second-Caps-tap lag
    //   preview_ms  GPU/CPU spent on the live overlay re-passes (not part of the transcript)
    //   partials/segments/gain/rms/rate  capture-side context (mirrors the helper-log line)
    println!(
        "{}transcribe_ms={total_transcribe_ms:.1} audio_ms={audio_ms:.1} \
         wall_ms={wall_ms:.1} final_ms={final_ms:.1} preview_ms={total_preview_ms:.1} \
         partials={partials} segments={} gain={final_gain:.2} rms={rms:.4} rate={rate}",
        proto::STTSTATS_PREFIX,
        committed.len(),
    );
    println!("{}{text}", proto::FINAL_PREFIX);
    println!("{}", proto::LDONE);
    flush();
}

/// One full-duplex listen session on the concurrent thread (see
/// [`concurrent_listen_loop`]): reads the echo-cancelled VPIO [`CaptureHandle`] and
/// stops on the `lstop`/`quit` signal (not the speak `cancel`).
fn run_concurrent_listen(
    capture: &CaptureHandle,
    transcriber: &std::sync::Mutex<ds_stt::LocalTranscriber>,
    sig: &(std::sync::Mutex<ListenSig>, std::sync::Condvar),
    stopped_through: &std::sync::atomic::AtomicU64,
    generation: u64,
) {
    let stopped = || {
        let s = sig.0.lock().unwrap_or_else(|e| e.into_inner());
        s.quit || generation_stopped(stopped_through, generation)
    };
    // Streaming path first; falls through to the offline loop when unavailable.
    if try_streaming(
        capture.capture_rate(),
        std::time::Duration::from_secs(120),
        "coexist-listen",
        &mut || capture.drain(),
        &stopped,
    ) {
        return;
    }
    transcribe_loop(
        capture.capture_rate(),
        std::time::Duration::from_secs(120),
        "coexist-listen",
        || capture.drain(),
        stopped,
        |pcm| {
            transcriber
                .lock()
                .unwrap()
                .transcribe_pcm_16k(pcm)
                .ok()
                .map(|t| t.replace('\n', " "))
        },
        speaker_locked_pcm,
    );
}

/// Trim leading/trailing silence from 16 kHz mono PCM. Parakeet HALLUCINATES on
/// silence (repeated tokens like "Yes Yes Yes"), so feeding it only the voiced
/// span both fixes that and cuts transcription work. Returns the voiced slice with
/// a small context margin, or empty if the whole buffer is below the floor.
fn trim_silence_16k(pcm: &[f32]) -> &[f32] {
    const WIN: usize = 320; // 20 ms @ 16 kHz
    const THRESH: f32 = 0.012; // above the (AGC-off) noise floor, below speech
    const MARGIN: usize = 3; // ~60 ms of context kept each side
    let n = pcm.len();
    if n == 0 {
        return pcm;
    }
    let voiced = |i: usize| -> bool {
        let c = &pcm[i * WIN..((i + 1) * WIN).min(n)];
        !c.is_empty() && (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt() >= THRESH
    };
    let frames = n.div_ceil(WIN);
    let first = (0..frames).find(|&i| voiced(i));
    let last = (0..frames).rev().find(|&i| voiced(i));
    match (first, last) {
        (Some(f), Some(l)) => {
            let start = f.saturating_sub(MARGIN) * WIN;
            let end = ((l + 1 + MARGIN) * WIN).min(n);
            &pcm[start.min(end)..end]
        }
        _ => &[],
    }
}

/// Run one STT (listen) session on the helper's playback thread (HALF-duplex): open
/// a fresh cpal mic and run the shared [`transcribe_loop`] until `cancel` (a `stop`
/// / new request). The cpal `Capture` is dropped when this returns, stopping the
/// stream. (Full-duplex listens go through the concurrent thread, not here.)
pub(crate) fn run_listen(
    transcriber: &mut ds_stt::LocalTranscriber,
    stopped_through: &std::sync::atomic::AtomicU64,
    generation: u64,
) {
    if generation_stopped(stopped_through, generation) {
        emit_empty_final();
        return;
    }
    // Fresh cpal mic. On open failure there's nothing to listen to — report and end.
    let capture = match ds_stt::Capture::open() {
        Ok(c) => c,
        Err(e) => {
            println!("{}{}", proto::STTERR_PREFIX, e.replace('\n', " "));
            println!("{}", proto::LDONE);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            return;
        }
    };
    let stopped = || generation_stopped(stopped_through, generation);
    // Streaming path first; falls through to the offline loop when unavailable.
    if try_streaming(
        capture.input_rate(),
        std::time::Duration::from_secs(60),
        "listen-stream",
        &mut || capture.drain_new(),
        &stopped,
    ) {
        return;
    }
    transcribe_loop(
        capture.input_rate(),
        std::time::Duration::from_secs(60),
        "listen-debug",
        || capture.drain_new(),
        stopped,
        |pcm| {
            transcriber
                .transcribe_pcm_16k(pcm)
                .ok()
                .map(|t| t.replace('\n', " "))
        },
        speaker_locked_pcm,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Cache-aware streaming dictation (the built-in ONNX "parakeet" engine).
// ─────────────────────────────────────────────────────────────────────────────

/// Process-wide cache of the loaded streaming backend, keyed by the provider it was built for
/// (`cpu`/`cuda` → ONNX, `ane` → Core ML). One mic / one listen at a time, so a single
/// cached instance is fine; the heavy model stays resident and each listen just `reset`s it.
///
/// `TtsManager` holds an exclusive listen lease for the helper's untagged output stream, while
/// this mutex serializes preload/use/unload inside the child. `active` records a backend that a
/// full-duplex listen temporarily owns; an unload received during that interval is remembered
/// and completed as soon as the session returns it, rather than silently leaving the model warm.
type CachedBackend = (String, Box<dyn StreamingStt>);
#[derive(Default)]
struct BackendCache {
    backend: Option<CachedBackend>,
    active: Option<(String, ds_config::RealizedProvider)>,
    unload_requested: bool,
}

fn backend_cell() -> &'static Mutex<BackendCache> {
    static CELL: OnceLock<Mutex<BackendCache>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(BackendCache::default()))
}

/// Build + cache the streaming backend for `provider` right now instead of waiting for the
/// first real listen to build it lazily — so `serve.rs`'s STT preload / `load stt` can warm the
/// SAME cache a real listen actually uses for the streaming-capable providers (`cpu`/`cuda`
/// ONNX, macOS `ane`), not just the separate offline `LocalTranscriber` cache it also warms.
/// Returns the realized provider if the cache now holds a resident instance for `provider`
/// (already did, or just built one); `None` if this provider doesn't stream or its assets are missing
/// ([`build_backend`] returned `None`).
pub(crate) fn preload_streaming(provider: &str) -> Option<ds_config::RealizedProvider> {
    let cell = backend_cell();
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    if guard.backend.as_ref().is_some_and(|(p, _)| p == provider) {
        guard.unload_requested = false;
        return guard
            .backend
            .as_ref()
            .map(|(_, backend)| backend.provider());
    }
    if let Some((active_provider, realized)) = &guard.active {
        if active_provider == provider {
            let realized = *realized;
            guard.unload_requested = false;
            return Some(realized);
        }
        return None;
    }
    guard.backend = build_backend(provider).map(|b| (provider.to_string(), b));
    guard.unload_requested = false;
    guard
        .backend
        .as_ref()
        .map(|(_, backend)| backend.provider())
}

/// Drop the cached streaming backend, freeing its resident model memory. Returns `true` if
/// something was actually freed. Companion to [`preload_streaming`] — lets `serve.rs`'s
/// `unload stt` free the SAME cache a real listen uses, not just the separate offline
/// `LocalTranscriber` cache (which roughly doubles STT memory if left resident).
pub(crate) fn unload_streaming() -> bool {
    let mut guard = backend_cell().lock().unwrap_or_else(|e| e.into_inner());
    let freed = guard.backend.take().is_some();
    let active = guard.active.is_some();
    guard.unload_requested = active;
    freed || active
}

/// Build the streaming backend for `provider`, or `None` when this provider doesn't stream / its
/// assets are missing (→ caller falls back to the offline `transcribe_loop`). ONNX
/// (`cpu`/`cuda`) → [`OnnxStreamer`]; macOS `ane` → the FluidAudio Core ML streamer. Both
/// implement [`StreamingStt`], so everything downstream is shared.
fn build_backend(provider: &str) -> Option<Box<dyn StreamingStt>> {
    if provider.eq_ignore_ascii_case("cpu") || provider.eq_ignore_ascii_case("cuda") {
        if !ds_model::is_parakeet_present() {
            return None;
        }
        let dir = ds_model::parakeet_dir()?;
        return match OnnxStreamer::load(&dir, true) {
            Ok(s) => Some(Box::new(s)),
            Err(e) => {
                log::warn!(target: "helper", "streaming: ONNX load failed, using offline: {e}");
                None
            }
        };
    }
    #[cfg(target_os = "macos")]
    if provider.eq_ignore_ascii_case("ane") {
        return match ds_stt::coreml::CoremlStreamer::new() {
            Ok(s) => Some(Box::new(s)),
            Err(e) => {
                log::warn!(target: "helper", "streaming: Core ML streamer unavailable, using offline: {e}");
                None
            }
        };
    }
    #[cfg(target_os = "macos")]
    if provider.eq_ignore_ascii_case("system") {
        return match ds_stt::sysspeech::SystemStreamer::new() {
            Ok(s) => Some(Box::new(s)),
            Err(e) => {
                log::warn!(target: "helper", "streaming: System streamer unavailable, using offline: {e}");
                None
            }
        };
    }
    None
}

/// Try to run this listen via the streaming path. Returns `true` if it handled the session
/// (emitting PARTIAL/STTSTATS/FINAL/LDONE), `false` if streaming is unavailable so the caller
/// should fall back to the offline [`transcribe_loop`]. The backend is chosen by the resolved STT
/// provider and CACHED across listens; the loop, resampling, and STTSTATS are backend-agnostic.
fn try_streaming(
    rate: u32,
    timeout: std::time::Duration,
    label: &str,
    drain: &mut dyn FnMut() -> Vec<f32>,
    stopped: &dyn Fn() -> bool,
) -> bool {
    use std::io::Write;
    use std::time::{Duration, Instant};
    // Speaker-lock currently needs the complete final mixture for separation + embedding.
    // Until that filter has a streaming form, route an explicitly-enabled lock through the
    // existing segment/final fallback instead of silently bypassing the safety control.
    let speaker_lock = ds_config::Paths::resolve()
        .map(|p| {
            let cfg = ds_config::VoiceConfig::load(&p);
            cfg.stt_speaker_lock && cfg.is_diarization_on()
        })
        .unwrap_or(false);
    if speaker_lock {
        return false;
    }
    let provider = std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default();
    let cell = backend_cell();
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    if guard.active.is_some() {
        log::warn!(target: "helper", "{label}: a streaming backend is already active");
        return false;
    }
    // (Re)build when absent or the provider changed since last listen.
    if guard
        .backend
        .as_ref()
        .map(|(p, _)| p != &provider)
        .unwrap_or(true)
    {
        guard.backend = build_backend(&provider).map(|b| (provider.clone(), b));
    }
    // Take ownership for this session (restored to the cache at the end).
    let Some((p, mut backend)) = guard.backend.take() else {
        return false;
    };
    guard.active = Some((p.clone(), backend.provider()));
    drop(guard);
    if let Err(e) = backend.reset() {
        log::warn!(target: "helper", "{label}: streaming reset failed, using offline: {e}");
        cell.lock().unwrap_or_else(|e| e.into_inner()).active = None;
        return false; // broken backend dropped, not re-cached
    }
    let mut session = match StreamSession::new(backend, rate) {
        Ok(s) => s,
        Err(e) => {
            log::warn!(target: "helper", "{label}: streaming resampler init failed, using offline: {e}");
            cell.lock().unwrap_or_else(|e| e.into_inner()).active = None;
            return false;
        }
    };
    let mut gain = StreamingGain::from_config();
    let flush = || {
        let _ = std::io::stdout().flush();
    };
    let _ = drain(); // drop stale pre-listen audio
    println!("{}", proto::LISTENING);
    flush();
    let started = Instant::now();
    let mut last_text = String::new();
    let mut partials = 0u32;
    // Gained capture @ device rate — exactly what `session.accept` sees. The only source for
    // the empty-transcript RMS diagnostic and DONTSPEAK_LISTEN_DUMP wav on THIS path: unlike
    // the offline fallback below, this is the steady-state path for every backend (see module
    // docs), so without this buffer those two diagnostics were silently unreachable in practice.
    let mut accum: Vec<f32> = Vec::new();
    while !stopped() && started.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(50));
        let block = drain();
        if block.is_empty() {
            continue;
        }
        let block = gain.apply(&block);
        accum.extend_from_slice(&block);
        match session.accept(&block) {
            Ok(text) => {
                if text != last_text && !text.trim().is_empty() {
                    println!("{}{text}", proto::PARTIAL_PREFIX);
                    flush();
                    last_text = text;
                    partials += 1;
                }
            }
            Err(e) => log::warn!(target: "helper", "{label}: streaming accept: {e}"),
        }
    }
    let final_start = Instant::now();
    // Capture can advance between the loop's last drain and the stop flag becoming visible.
    // Feed that tail before flushing so the final syllable is never dropped.
    let tail = drain();
    if !tail.is_empty() {
        let tail = gain.apply(&tail);
        accum.extend_from_slice(&tail);
        if let Err(e) = session.accept(&tail) {
            log::warn!(target: "helper", "{label}: streaming final drain: {e}");
        }
    }
    let text = session.finalize().unwrap_or_else(|e| {
        log::warn!(target: "helper", "{label}: streaming finalize: {e}");
        String::new()
    });
    let final_ms = final_start.elapsed().as_secs_f64() * 1000.0;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let audio_ms = session.audio_ms();
    let transcribe_ms = session.transcribe_ms();
    // Same empty-transcript diagnostics as the offline path (module docs): a near-zero RMS
    // means silence reached the mic path (AEC over-cancelling, or no mic grant); the dump is
    // the actual 16 kHz buffer this backend saw.
    let rms = if accum.is_empty() {
        0.0
    } else {
        (accum.iter().map(|x| x * x).sum::<f32>() / accum.len() as f32).sqrt()
    };
    log::debug!(
        target: "helper",
        "{label}: rate={rate} accum={} rms={rms:.4} partials={partials} streaming=1",
        accum.len(),
    );
    if std::env::var_os("DONTSPEAK_LISTEN_DUMP").is_some() {
        let dump = ds_stt::resample_to_16k(&accum, rate);
        let path = std::env::temp_dir().join("ds-listen.wav");
        match ds_tts::wav::write_wav16(&path, &dump, 16_000) {
            Ok(()) => log::info!(target: "helper", "{label}: dumped → {}", path.display()),
            Err(e) => log::warn!(target: "helper", "{label}: wav dump failed: {e}"),
        }
    }
    // STTSTATS schema shared with the offline path; `preview_ms=0` (no re-encode) + `streaming=1`
    // are the success markers in the activity-log `STT listen ...` line under DONTSPEAK_DEBUG.
    println!(
        "{}transcribe_ms={transcribe_ms:.1} audio_ms={audio_ms:.1} wall_ms={wall_ms:.1} \
         final_ms={final_ms:.1} preview_ms=0.0 partials={partials} streaming=1",
        proto::STTSTATS_PREFIX
    );
    println!("{}{}", proto::FINAL_PREFIX, text.replace('\n', " "));
    println!("{}", proto::LDONE);
    flush();
    // Restore the backend for the next listen unless an unload arrived while this
    // full-duplex session owned it. In that case dropping it here completes the unload.
    let backend = session.into_backend();
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    guard.active = None;
    if guard.unload_requested {
        guard.unload_requested = false;
    } else {
        guard.backend = Some((p, backend));
    }
    true
}

/// Resolve the SepFormer separator model, most-specific first: an explicit
/// `DONTSPEAK_SEPARATOR_PATH` (older app bundles shipped the model inside Resources and
/// point this at it), a dev copy in the config dir (so the lock can be exercised without
/// a full `.app` build), then the DOWNLOADED copy in the flat `model_dir()` — the normal
/// path since the model moved out of the repo/bundle and into the standard download
/// registry (`ds_model::urls::SEPFORMER`, auto-fetched by the engine when the speaker-lock
/// is on). `None` ⇒ no model present (lock fails open).
#[cfg(target_os = "macos")]
fn separator_model_path(paths: &ds_config::Paths) -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("DONTSPEAK_SEPARATOR_PATH") {
        let p = std::path::PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let dev = paths.config_dir.join("sepformer_int8.onnx");
    if dev.exists() {
        return Some(dev);
    }
    ds_model::model_path(ds_model::SEPFORMER_FILE).filter(|p| p.exists())
}

// The cached per-thread separator (the CoreML/ANE model compiles once on first load, which
// is slow — keep it resident across dictations instead of reloading per utterance). Holds
// the resolved model path too, so a changed `DONTSPEAK_SEPARATOR_PATH` reloads.
#[cfg(target_os = "macos")]
thread_local! {
    static SEPARATOR: std::cell::RefCell<Option<(std::path::PathBuf, ds_stt::Separator)>> =
        const { std::cell::RefCell::new(None) };
}

/// Speaker-lock for the FINAL dictation buffer: when `stt_speaker_lock` is on, diarization
/// is enabled, and ≥1 voice is enrolled, SEPARATE the mixture into its constituent voices
/// (SepFormer) and transcribe only the stream whose voiceprint matches the enrolled user —
/// removing a co-channel background voice (other person / TV / a video) that frame-gating
/// can't un-mix.
///
/// FAILS OPEN in every uncertain case — returns the mixture UNCHANGED (never empty) when the
/// lock is off, no model is present, separation errors, or no stream clears the match
/// threshold. So dictation is never silently dropped (the earlier "lock ate my words / paste
/// failed" bug); the worst case degrades to transcribing everything, exactly as lock-off.
#[cfg(target_os = "macos")]
fn speaker_locked_pcm(pcm: &[f32]) -> Vec<f32> {
    use ds_stt::diarize::{CoremlDiarizer, Diarizer, cosine};

    let Some(paths) = ds_config::Paths::resolve() else {
        return pcm.to_vec();
    };
    let cfg = ds_config::VoiceConfig::load(&paths);
    if !cfg.stt_speaker_lock || !cfg.is_diarization_on() {
        return pcm.to_vec();
    }
    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
    if store.speakers.is_empty() {
        return pcm.to_vec(); // nothing enrolled to lock to → fail open
    }
    let Some(model_path) = separator_model_path(&paths) else {
        log::warn!(target: "helper", "speaker-lock: no separator model; transcribing unfiltered");
        return pcm.to_vec();
    };

    // Separate into voices (cached session; (re)load if the model path changed).
    let streams = SEPARATOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().map(|(p, _)| p != &model_path).unwrap_or(true) {
            match ds_stt::Separator::load(&model_path) {
                Ok(s) => {
                    log::info!(target: "helper", "speaker-lock: separator loaded ({})", s.provider());
                    *slot = Some((model_path.clone(), s));
                }
                Err(e) => {
                    log::warn!(target: "helper", "speaker-lock: separator load failed ({e}); unfiltered");
                    return None;
                }
            }
        }
        match slot.as_mut().unwrap().1.separate_16k(pcm) {
            Ok(st) => Some(st),
            Err(e) => {
                log::warn!(target: "helper", "speaker-lock: separate failed ({e}); unfiltered");
                None
            }
        }
    });
    let Some(streams) = streams else {
        return pcm.to_vec(); // fail open
    };
    if streams.len() < 2 {
        return pcm.to_vec(); // nothing to choose between → fail open
    }

    // Embed each separated stream with the SAME WeSpeaker model used for enrollment, and
    // score it against the enrolled voiceprint(s).
    let mut diar = CoremlDiarizer::new();
    let mut scored: Vec<(usize, f32)> = Vec::with_capacity(streams.len());
    for (i, s) in streams.iter().enumerate() {
        let Ok(emb) = diar.embed(s) else { continue };
        let score = store
            .speakers
            .iter()
            .map(|sp| cosine(&emb, &sp.embedding))
            .fold(f32::MIN, f32::max);
        scored.push((i, score));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    // Per-utterance diagnostic — fires every dictation while speaker-lock is on, routine —
    // same DONTSPEAK_DEBUG gate as the engine's own Debug lines, off by default.
    log::debug!(
        target: "helper",
        "speaker-lock: stream scores {:?}",
        scored
            .iter()
            .map(|(i, s)| (*i, (s * 100.0).round() / 100.0))
            .collect::<Vec<_>>()
    );
    // RELATIVE selection: SepFormer always returns one stream per voice, so the user's
    // voice is "the stream that looks MORE like them than the other does". Pick the top
    // stream when it (a) clears a low absolute floor (not pure noise/silence) AND (b) beats
    // the runner-up by a margin (clearly the user, not a coin-flip). The absolute enrolled-
    // match threshold (`speaker_threshold`, tuned for CLEAN enrollment audio) is too strict
    // for separated streams, which carry mild artifacts and score lower. Anything uncertain
    // FAILS OPEN — transcribe the mixture, never drop the user.
    const FLOOR: f32 = 0.15; // below this the top stream isn't plausibly the user
    const MARGIN: f32 = 0.10; // top must beat runner-up by this to be unambiguous
    let top = scored.first().copied();
    let runner = scored.get(1).map(|(_, s)| *s).unwrap_or(f32::MIN);
    match top {
        Some((i, score)) if score >= FLOOR && score - runner >= MARGIN => {
            // PEAK-NORMALIZE the isolated stream before it reaches Parakeet. SepFormer
            // outputs the extracted voice at a REDUCED level (the masking removes energy),
            // so the raw stream — though it matched the voiceprint — can be too quiet to
            // transcribe (comes back "silence"). Scale its peak to ~0.95 full-scale, the
            // level a normal close-mic utterance presents, so STT sees a healthy signal.
            let mut out = streams[i].clone();
            let peak = out.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
            if peak > 1e-4 {
                let g = 0.95 / peak;
                for s in &mut out {
                    *s = (*s * g).clamp(-1.0, 1.0);
                }
            }
            // Per-utterance diagnostic — fires every dictation this lock resolves, routine —
            // same DONTSPEAK_DEBUG gate as the engine's own Debug lines, off by default.
            log::debug!(
                target: "helper",
                "speaker-lock: picked stream {i} (cos {score:.2}, +{:.2} over next, peak {peak:.3}→0.95) — background removed",
                score - runner
            );
            out
        }
        // Ambiguous (both streams similar) or too weak → fail OPEN, never drop.
        other => {
            let s = other.map(|(_, s)| s).unwrap_or(f32::NAN);
            log::warn!(target: "helper", "speaker-lock: no clear target (top cos {s:.2}); transcribing unfiltered");
            pcm.to_vec()
        }
    }
}

/// Off macOS the separator/diarizer isn't wired, so the lock is a no-op (transcribe all).
#[cfg(not(target_os = "macos"))]
fn speaker_locked_pcm(pcm: &[f32]) -> Vec<f32> {
    pcm.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn owned(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tail_budget_matches_force_split_bound() {
        // The preview budget MUST equal the VAD force-split bound, in samples — that
        // equality is what closes the "blank overlay on long pause-free speech" gap.
        let budget = tail_preview_budget_samples(RATE);
        let force_split = ds_stt::boundary::MAX_SEGMENT_SECS * RATE as usize;
        assert_eq!(budget, force_split);
    }

    #[test]
    fn tail_previewable_spans_zero_to_the_force_split_bound() {
        let budget = tail_preview_budget_samples(RATE);
        assert!(!tail_previewable(0, RATE), "empty tail is never previewed");
        assert!(tail_previewable(1, RATE), "a one-sample tail previews");
        assert!(
            tail_previewable(budget, RATE),
            "a tail exactly at the bound still previews"
        );
        // A tail one sample past the bound is rejected — but the VAD force-commits at the
        // same bound, so in practice the tail is committed before it can reach here. The
        // point: there is NO length that is both unpreviewable AND uncommitted (the bug).
        assert!(
            !tail_previewable(budget + 1, RATE),
            "an over-bound tail is skipped"
        );
    }

    #[test]
    fn overlay_joins_committed_with_tail_preview() {
        let got = next_overlay(&owned(&["hello", "there"]), Some("wor"), "");
        assert_eq!(got.as_deref(), Some("hello there wor"));
    }

    #[test]
    fn overlay_without_tail_shows_committed_only() {
        let got = next_overlay(&owned(&["hello", "there"]), None, "");
        assert_eq!(got.as_deref(), Some("hello there"));
    }

    #[test]
    fn overlay_skips_when_unchanged() {
        // Same text as last emission → None, so the helper doesn't spam identical PARTIALs.
        let got = next_overlay(&owned(&["hello"]), Some("there"), "hello there");
        assert_eq!(got, None);
    }

    #[test]
    fn overlay_skips_when_empty() {
        assert_eq!(next_overlay(&[], None, ""), None);
        assert_eq!(next_overlay(&owned(&["", "  "]), None, ""), None);
    }

    #[test]
    fn streaming_manual_gain_is_exact_and_clamped() {
        let mut gain = StreamingGain {
            mode: ds_config::CaptureGain::Manual(2.0),
            peak: 0.0,
            gain: 1.0,
        };
        assert_eq!(gain.apply(&[0.25, -0.75]), vec![0.5, -1.0]);
    }

    #[test]
    fn streaming_auto_gain_never_amplifies_silence() {
        let mut gain = StreamingGain {
            mode: ds_config::CaptureGain::Auto,
            peak: 0.0,
            gain: 1.0,
        };
        let silence = vec![0.005f32; 128];
        assert_eq!(gain.apply(&silence), silence);
        let quiet_speech = vec![0.05f32; 128];
        let conditioned = gain.apply(&quiet_speech);
        assert!(conditioned[0] > quiet_speech[0]);
        assert!(conditioned.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn listen_stop_is_monotonic_and_cancels_only_through_its_generation() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let stopped = AtomicU64::new(0);
        assert!(!generation_stopped(&stopped, 4));
        stopped.fetch_max(4, Ordering::SeqCst);
        assert!(generation_stopped(&stopped, 3));
        assert!(generation_stopped(&stopped, 4));
        assert!(!generation_stopped(&stopped, 5));
        stopped.fetch_max(2, Ordering::SeqCst);
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            4,
            "an older stop cannot roll cancellation back"
        );
    }
}
