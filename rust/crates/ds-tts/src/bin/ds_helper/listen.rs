//! STT capture/transcribe cluster: the shared [`transcribe_loop`] and its two
//! callers ([`run_listen`] half-duplex, [`run_concurrent_listen`] full-duplex),
//! plus the make-up gain ([`auto_gain`]) and silence trim ([`trim_silence_16k`]).

use ds_aec::CaptureHandle;

/// Control flags for the full-duplex concurrent listen thread (set by the stdin
/// reader, polled by the thread). `start` opens a listen, `stop` (the `lstop` op)
/// ends it, `quit` tears the thread down.
#[derive(Default)]
pub(crate) struct ListenSig {
    pub(crate) start: bool,
    pub(crate) stop: bool,
    pub(crate) quit: bool,
}

/// The full-duplex CONCURRENT listen thread. Idles until `start`, runs a listen
/// session (drain the echo-cancelled mic + stream partials) until `stop`, then
/// idles again. Runs ALONGSIDE the playback thread's TTS render — that's coexist:
/// the user dictates while the voice speaks, the AEC keeps the capture clean. Emits
/// `LDONE` (not `DONE`) as its terminal so the engine can demux it from speak
/// output, which shares the same stdout (println! is line-atomic across threads).
pub(crate) fn concurrent_listen_loop(
    capture: CaptureHandle,
    transcriber: std::sync::Arc<std::sync::Mutex<ds_stt::LocalTranscriber>>,
    sig: std::sync::Arc<(std::sync::Mutex<ListenSig>, std::sync::Condvar)>,
) {
    loop {
        {
            let (m, cv) = &*sig;
            let mut s = m.lock().unwrap();
            while !s.start && !s.quit {
                s = cv.wait(s).unwrap();
            }
            if s.quit {
                return;
            }
            s.start = false;
            s.stop = false;
        }
        run_concurrent_listen(&capture, &transcriber, &sig);
    }
}

/// Auto make-up gain for one captured utterance (`capture_gain = "auto"`): peak-
/// normalize to a target so quiet AND hot mics both land where Parakeet recognizes
/// best, with a noise-floor gate so pure silence / room hum is never amplified.
/// Dictation is push-to-talk, so the full buffer is in hand — this is a one-shot
/// measurement, no streaming AGC needed.
fn auto_gain(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 1.0;
    }
    let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    const NOISE_FLOOR: f32 = 0.02; // below this it's silence — leave it alone
    const TARGET_PEAK: f32 = 0.9; // headroom under full-scale to avoid clipping
    if peak < NOISE_FLOOR {
        return 1.0;
    }
    (TARGET_PEAK / peak).clamp(0.5, 15.0)
}

/// Max open-tail length (in device-rate samples) we re-transcribe for the live preview.
///
/// SINGLE source of truth: the VAD's force-split bound, [`ds_stt::boundary::MAX_SEGMENT_SECS`].
/// The tail is force-committed once it reaches that length, so previewing up to *exactly*
/// that length leaves no gap between "too long to preview cheaply" and "long enough to be
/// committed". Those two used to be separate numbers (an 8 s preview cap vs a 20 s split);
/// a pause-free phrase between them grew a tail too long to preview but too short to
/// commit, so the overlay went blank until stop. Deriving the budget here keeps them one
/// value — do NOT reintroduce a hardcoded seconds literal.
fn tail_preview_budget_samples(rate: u32) -> usize {
    ds_stt::boundary::MAX_SEGMENT_SECS * rate as usize
}

/// Whether the still-open tail (length in device-rate samples) is worth re-transcribing
/// for this tick's live preview: non-empty and within [`tail_preview_budget_samples`].
fn tail_previewable(tail_len: usize, rate: u32) -> bool {
    tail_len > 0 && tail_len <= tail_preview_budget_samples(rate)
}

/// Build the live `PARTIAL` overlay line from the already-committed segment texts plus an
/// optional preview of the still-open tail, returning the text to emit — or `None` to skip
/// this tick because nothing changed or the line is empty. Pure (no audio, no IPC) so the
/// streaming-overlay contract is unit-testable.
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

/// The SHARED listen loop, used by BOTH the half-duplex serve-loop listen
/// ([`run_listen`]) and the full-duplex concurrent listen ([`run_concurrent_listen`])
/// so the cadence / silence-trim / partial logic can't drift between modes. The two
/// callers supply only what differs:
///
/// * `rate`       — the capture sample rate (cpal device-native vs VPIO 48 kHz),
/// * `timeout`    — the hard session cap,
/// * `label`      — the helper-log diagnostic tag,
/// * `drain`      — pull newly-captured samples (cpal `drain_new` vs VPIO `drain`),
/// * `stopped`    — end the session (speak-loop `cancel` flag vs the `lstop` sig),
/// * `transcribe` — run Parakeet on trimmed 16 kHz PCM → flattened text (callers
///   differ in `&mut` vs `&Mutex` access to the model).
///
/// Emits `LISTENING`, periodic `PARTIAL <text>` (~every 180 ms, de-duped), then a
/// final `STTSTATS` + `FINAL <text>` + `LDONE`. The partial is the segments already
/// finalized at speech→silence boundaries plus a cheap re-pass of the still-open tail;
/// the final pass only transcribes the short remaining segment, not the whole buffer
/// (see [`ds_stt::VadBoundaryDetector`]). `LDONE` (not `DONE`) lets the engine
/// demux a listen from concurrent speak output.
fn transcribe_loop(
    rate: u32,
    timeout: std::time::Duration,
    label: &str,
    mut drain: impl FnMut() -> Vec<f32>,
    stopped: impl Fn() -> bool,
    mut transcribe: impl FnMut(&[f32]) -> Option<String>,
    // Applied to the FINAL 16 kHz buffer only (partials stream unfiltered for latency):
    // the speaker-lock filter that drops non-enrolled voices. Identity when lock is off.
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
    println!("LISTENING");
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
        if pcm.is_empty() {
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
        // Live partial: finalized segments, plus a cheap re-pass of the still-open tail
        // when it's short enough (force-split bounds it). The tail is NOT committed here.
        // 180 ms cadence (was 350): partials stream in roughly twice as often so the
        // overlay tracks speech smoothly instead of landing in visible ~⅓-second chunks.
        // The extra cost is one more bounded tail re-pass per emission (warm Parakeet),
        // and the dedup below (`merged != last_text`) still drops no-change repeats.
        if last_partial.elapsed() >= Duration::from_millis(180) {
            let tail = &accum[committed_until.min(accum.len())..];
            let tail_text = if tail_previewable(tail.len(), rate) {
                let mut scratch = 0f64;
                segment_text(tail, &mut scratch)
            } else {
                None
            };
            if let Some(merged) = next_overlay(&committed, tail_text.as_deref(), &last_text) {
                println!("PARTIAL {merged}");
                flush();
                last_text = merged;
                partials += 1;
            }
            last_partial = Instant::now();
        }
    }

    // Final pass: drain the tail, then finalize only the SHORT remaining segment past the
    // last boundary (not the whole buffer).
    accum.extend_from_slice(&drain());
    let final_gain = gain_of(&accum);
    // DONTSPEAK_LISTEN_DUMP=1 → write the full 16 kHz buffer Parakeet effectively saw.
    if std::env::var_os("DONTSPEAK_LISTEN_DUMP").is_some() {
        let dump = ds_stt::resample_to_16k(&apply_gain(&accum), rate);
        let path = std::env::temp_dir().join("ds-listen.wav");
        match ds_tts::wav::write_wav16(&path, &dump, 16_000) {
            Ok(()) => eprintln!("{label}: dumped → {}", path.display()),
            Err(e) => eprintln!("{label}: wav dump failed: {e}"),
        }
    }
    if committed_until < accum.len()
        && let Some(text) = segment_text(&accum[committed_until..], &mut total_transcribe_ms)
    {
        committed.push(text);
    }
    let text = committed.join(" ");

    // Diagnostics (→ helper log): RMS of the captured audio, sample counts, segment +
    // partial counts, and the resolved gain. A near-zero RMS means silence reached the
    // mic path (AEC over-cancelling, or no mic grant) — the empty-transcript case.
    let audio_ms = accum.len() as f64 / rate as f64 * 1000.0;
    let rms = if accum.is_empty() {
        0.0
    } else {
        (accum.iter().map(|x| x * x).sum::<f32>() / accum.len() as f32).sqrt()
    };
    eprintln!(
        "{label}: rate={rate} accum={} segments={} rms={rms:.4} partials={partials} gain={final_gain:.1}",
        accum.len(),
        committed.len(),
    );
    println!("STTSTATS transcribe_ms={total_transcribe_ms:.1} audio_ms={audio_ms:.1}");
    println!("FINAL {text}");
    println!("LDONE");
    flush();
}

/// One full-duplex listen session on the concurrent thread (see
/// [`concurrent_listen_loop`]): reads the echo-cancelled VPIO [`CaptureHandle`] and
/// stops on the `lstop`/`quit` signal (not the speak `cancel`).
fn run_concurrent_listen(
    capture: &CaptureHandle,
    transcriber: &std::sync::Mutex<ds_stt::LocalTranscriber>,
    sig: &(std::sync::Mutex<ListenSig>, std::sync::Condvar),
) {
    transcribe_loop(
        capture.capture_rate(),
        std::time::Duration::from_secs(120),
        "coexist-listen",
        || capture.drain(),
        || {
            let s = sig.0.lock().unwrap();
            s.stop || s.quit
        },
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
    cancel: &std::sync::atomic::AtomicBool,
) {
    use std::sync::atomic::Ordering;
    // Fresh cpal mic. On open failure there's nothing to listen to — report and end.
    let capture = match ds_stt::Capture::open() {
        Ok(c) => c,
        Err(e) => {
            println!("STTERR {}", e.replace('\n', " "));
            println!("LDONE");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            return;
        }
    };
    transcribe_loop(
        capture.input_rate(),
        std::time::Duration::from_secs(60),
        "listen-debug",
        || capture.drain_new(),
        || cancel.load(Ordering::SeqCst),
        |pcm| {
            transcriber
                .transcribe_pcm_16k(pcm)
                .ok()
                .map(|t| t.replace('\n', " "))
        },
        speaker_locked_pcm,
    );
}

/// Resolve the bundled SepFormer separator model: the app sets `DONTSPEAK_SEPARATOR_PATH`
/// to the file in its app bundle; a dev fallback checks the data dir so the lock can be
/// exercised without a full `.app` build. `None` ⇒ no model present (lock fails open).
#[cfg(target_os = "macos")]
fn separator_model_path(paths: &ds_config::Paths) -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("DONTSPEAK_SEPARATOR_PATH") {
        let p = std::path::PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let dev = paths.config_dir.join("sepformer_int8.onnx");
    dev.exists().then_some(dev)
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
    if !cfg.stt_speaker_lock || !cfg.diarization_on() {
        return pcm.to_vec();
    }
    let store = ds_config::SpeakerStore::load(&paths.speakers_json);
    if store.speakers.is_empty() {
        return pcm.to_vec(); // nothing enrolled to lock to → fail open
    }
    let Some(model_path) = separator_model_path(&paths) else {
        eprintln!("speaker-lock: no separator model; transcribing unfiltered");
        return pcm.to_vec();
    };

    // Separate into voices (cached session; (re)load if the model path changed).
    let streams = SEPARATOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().map(|(p, _)| p != &model_path).unwrap_or(true) {
            match ds_stt::Separator::load(&model_path) {
                Ok(s) => {
                    eprintln!("speaker-lock: separator loaded ({})", s.provider());
                    *slot = Some((model_path.clone(), s));
                }
                Err(e) => {
                    eprintln!("speaker-lock: separator load failed ({e}); unfiltered");
                    return None;
                }
            }
        }
        match slot.as_mut().unwrap().1.separate_16k(pcm) {
            Ok(st) => Some(st),
            Err(e) => {
                eprintln!("speaker-lock: separate failed ({e}); unfiltered");
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
    eprintln!(
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
            eprintln!(
                "speaker-lock: picked stream {i} (cos {score:.2}, +{:.2} over next, peak {peak:.3}→0.95) — background removed",
                score - runner
            );
            out
        }
        // Ambiguous (both streams similar) or too weak → fail OPEN, never drop.
        other => {
            let s = other.map(|(_, s)| s).unwrap_or(f32::NAN);
            eprintln!("speaker-lock: no clear target (top cos {s:.2}); transcribing unfiltered");
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
        assert!(tail_previewable(budget, RATE), "a tail exactly at the bound still previews");
        // A tail one sample past the bound is rejected — but the VAD force-commits at the
        // same bound, so in practice the tail is committed before it can reach here. The
        // point: there is NO length that is both unpreviewable AND uncommitted (the bug).
        assert!(!tail_previewable(budget + 1, RATE), "an over-bound tail is skipped");
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
}
