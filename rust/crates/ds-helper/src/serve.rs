//! `--serve` warm-child loop: load the model ONCE, then read JSON requests on
//! stdin (one object per line) and synth+play / listen / (un)load each. Owns the
//! `State`/`Job` machine and the op dispatch (`listen`/`lstop`/`load`/`unload`/
//! `speak`/etc.).

use ds_aec::DuplexAudio;
use ds_helper_proto as proto;
use ds_tts::g2p::{self, PhonemeBatchesOutcome};
use serde::Deserialize;
use std::time::Duration;

use crate::_exit;
use crate::listen::{ListenSig, concurrent_listen_loop, run_listen};
use crate::oneshot::{Backend, load_backend};
use crate::prepare::{PrepareOutcome, PreparedAudio, prepare_audio};

const TTS_OUTPUT_UNAVAILABLE: &str = "helper started without TTS output; restart required";

fn tts_output_available(tts_wanted: bool, render_via_duplex: bool) -> bool {
    tts_wanted || render_via_duplex
}

fn barge_then_publish<T>(
    shared: &(std::sync::Mutex<T>, std::sync::Condvar),
    barge: impl FnOnce(),
    publish: impl FnOnce(&mut T),
) {
    barge();
    let (mutex, cv) = shared;
    let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
    publish(&mut state);
    drop(state);
    cv.notify_one();
}

#[derive(Default)]
struct CueGate {
    muted: bool,
    next_generation: u64,
    active: Option<u64>,
}

impl CueGate {
    fn begin(&mut self) -> Option<(u64, Option<u64>)> {
        if self.muted {
            return None;
        }
        let previous = self.active.take();
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.active = Some(self.next_generation);
        Some((self.next_generation, previous))
    }

    fn accepts_handle(&self, generation: u64) -> bool {
        !self.muted && self.active == Some(generation)
    }

    fn finish(&mut self, generation: u64) {
        if self.active == Some(generation) {
            self.active = None;
        }
    }

    fn set_muted(&mut self, on: bool) -> Option<u64> {
        self.muted = on;
        if on { self.active.take() } else { None }
    }

    fn cancel(&mut self) -> Option<u64> {
        self.active.take()
    }
}

#[derive(Default)]
struct CuePlayback {
    gate: std::sync::Mutex<CueGate>,
    player: std::sync::Mutex<Option<(u64, std::sync::Arc<rodio::Player>)>>,
    #[cfg(target_os = "macos")]
    afplay: std::sync::Mutex<Option<(u64, std::process::Child)>>,
}

impl CuePlayback {
    fn begin(&self) -> Option<u64> {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let (generation, previous) = gate.begin()?;
        if let Some(previous) = previous {
            self.stop_handles(previous);
        }
        Some(generation)
    }

    fn install_player(&self, generation: u64, player: std::sync::Arc<rodio::Player>) -> bool {
        let gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if !gate.accepts_handle(generation) {
            player.stop();
            return false;
        }
        *self.player.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, player));
        true
    }

    #[cfg(target_os = "macos")]
    fn install_afplay(&self, generation: u64, mut child: std::process::Child) -> bool {
        let gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if !gate.accepts_handle(generation) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        *self.afplay.lock().unwrap_or_else(|e| e.into_inner()) = Some((generation, child));
        true
    }

    #[cfg(target_os = "macos")]
    fn wait_afplay(&self, generation: u64) {
        loop {
            let done = {
                let mut active = self.afplay.lock().unwrap_or_else(|e| e.into_inner());
                match active.as_mut() {
                    Some((active_generation, child)) if *active_generation == generation => {
                        match child.try_wait() {
                            Ok(Some(_)) => {
                                *active = None;
                                true
                            }
                            Ok(None) => false,
                            Err(_) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                *active = None;
                                true
                            }
                        }
                    }
                    _ => true,
                }
            };
            if done {
                return;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    fn finish(&self, generation: u64) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        gate.finish(generation);
        let mut player = self.player.lock().unwrap_or_else(|e| e.into_inner());
        if player
            .as_ref()
            .is_some_and(|(active_generation, _)| *active_generation == generation)
        {
            *player = None;
        }
        #[cfg(target_os = "macos")]
        {
            let mut afplay = self.afplay.lock().unwrap_or_else(|e| e.into_inner());
            if afplay
                .as_ref()
                .is_some_and(|(active_generation, _)| *active_generation == generation)
                && let Some((_, mut child)) = afplay.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn set_muted(&self, on: bool) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(generation) = gate.set_muted(on) {
            self.stop_handles(generation);
        }
    }

    fn cancel(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(generation) = gate.cancel() {
            self.stop_handles(generation);
        }
    }

    /// Called with `gate` held, keeping admission/handle installation ordered with stops.
    fn stop_handles(&self, generation: u64) {
        let mut player = self.player.lock().unwrap_or_else(|e| e.into_inner());
        if player
            .as_ref()
            .is_some_and(|(active_generation, _)| *active_generation == generation)
            && let Some((_, player)) = player.take()
        {
            player.stop();
        }
        #[cfg(target_os = "macos")]
        {
            let mut afplay = self.afplay.lock().unwrap_or_else(|e| e.into_inner());
            if afplay
                .as_ref()
                .is_some_and(|(active_generation, _)| *active_generation == generation)
                && let Some((_, mut child)) = afplay.take()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn emit_cue_done() {
    use std::io::Write as _;
    println!("{}", proto::CUEDONE);
    let _ = std::io::stdout().flush();
}

/// Flatten an error for a protocol line: the engine's reader parses helper stdout strictly
/// line-by-line, so a multi-line message (ort/ONNX Runtime `Display` can be) would truncate
/// the `ERR`/`TTSLOADERR` terminal at its first line and leak the rest as stray lines.
fn one_line(e: &str) -> String {
    e.lines().collect::<Vec<_>>().join(" ")
}
use crate::stt_residency::SttResidencySlot;

/// One stdin request in `--serve` mode (one JSON object per line).
#[derive(Debug, Deserialize)]
struct ServeReq {
    op: String,
    #[serde(default)]
    voice: String,
    #[serde(default = "default_rate")]
    rate: f32,
    #[serde(default)]
    text: String,
    /// For `op:"unload"` — which cached model to free: "tts" (Kokoro) or "stt"
    /// (Parakeet). Ignored by other ops.
    #[serde(default)]
    engine: String,
    /// For `op:"diarize"` / `op:"enroll"` — how many seconds of mic to record first.
    #[serde(default)]
    seconds: Option<u64>,
    /// Monotonic daemon-owned identity for `listen`/`lstop`. A stop cancels this
    /// generation even when it reaches the helper before the queued start runs.
    #[serde(default)]
    session: Option<u64>,
}
fn default_rate() -> f32 {
    1.0
}

/// Record a fixed `seconds`-long window of mic audio, resampled to 16 kHz mono — the
/// shared capture step for one-shot `diarize` and `enroll`. `Err` if the mic won't open.
#[cfg(target_os = "macos")]
fn record_16k(seconds: u64, cancel: &std::sync::atomic::AtomicBool) -> Result<Vec<f32>, String> {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let capture = ds_stt::Capture::open()?;
    let rate = capture.input_rate();
    let _ = capture.drain_new(); // drop stale pre-record audio
    let mut accum: Vec<f32> = Vec::new();
    let started = Instant::now();
    while !cancel.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(50));
        accum.extend_from_slice(&capture.drain_new());
    }
    accum.extend_from_slice(&capture.drain_new()); // tail
    let pcm = ds_stt::resample_to_16k(&accum, rate);
    // Per-capture diagnostic (fires once per diarize/enroll job) — routine, so it's gated
    // behind DONTSPEAK_DEBUG like the engine's own DEBUG lines, not shown by default.
    log::debug!(
        target: "helper",
        "capture: rate={rate} accum={} pcm16k={} secs={seconds}",
        accum.len(),
        pcm.len()
    );
    Ok(pcm)
}

/// The Core ML / ANE backend is the only diarizer wired today. Resolves the config's
/// provider ladder, then delegates the provider → backend gate to
/// [`ds_stt::diarize::ensure_coreml_backend`] — THE single mapping site — so this helper
/// can't drift from it. Returns that gate's `Err` (a user-facing message) when the
/// resolved provider is anything Core ML can't serve; `Ok` ⇒ Core ML is the right backend.
#[cfg(target_os = "macos")]
fn ensure_coreml_diarizer(cfg: &ds_config::VoiceConfig) -> Result<(), String> {
    ds_stt::diarize::ensure_coreml_backend(cfg.resolved_diarizer_provider())
}

/// One-shot diarization: record `seconds`, then diarize with the config's clustering
/// threshold. Gated on diarization being ON (non-empty `diarizer_provider`) + a Core ML-resolvable rung.
/// Emits `DIAR <json>` ({segments,speakers}) then `DDONE`, or `DIARERR <msg>`/`DDONE`.
/// The engine does enrolled-name matching.
#[cfg(target_os = "macos")]
fn run_diarize(seconds: u64, cancel: &std::sync::atomic::AtomicBool) {
    use ds_stt::diarize::{CoremlDiarizer, Diarizer};
    use std::io::Write as _;

    let emit_err = |msg: &str| {
        println!("{}{}", proto::DIARERR_PREFIX, msg.replace('\n', " "));
        println!("{}", proto::DDONE);
        let _ = std::io::stdout().flush();
    };

    // Read config fresh (mirrors capture_gain); gate + threshold come from it.
    let cfg = ds_config::Paths::resolve().map(|p| ds_config::VoiceConfig::load(&p));
    let Some(cfg) = cfg else {
        return emit_err("config unavailable");
    };
    if !cfg.is_diarization_on() {
        return emit_err("diarization is disabled (set diarizer_provider to a non-empty ladder)");
    }
    if let Err(e) = ensure_coreml_diarizer(&cfg) {
        return emit_err(&e);
    }

    let pcm = match record_16k(seconds, cancel) {
        Ok(p) => p,
        Err(e) => return emit_err(&e),
    };
    let mut diarizer = CoremlDiarizer::with_threshold(cfg.clustering_threshold);
    match diarizer.diarize_pcm_16k_full(&pcm) {
        Ok(out) => {
            let segments: Vec<_> = out
                .segments
                .iter()
                .map(
                    |s| serde_json::json!({ "speaker": s.speaker, "start": s.start, "end": s.end }),
                )
                .collect();
            let json = serde_json::json!({ "segments": segments, "speakers": out.speakers });
            println!("{}{json}", proto::DIAR_PREFIX);
            println!("{}", proto::DDONE);
            let _ = std::io::stdout().flush();
        }
        Err(e) => emit_err(&e),
    }
}

/// One-shot enrollment: record `seconds`, extract one WeSpeaker voiceprint, emit
/// `EMB <json-floats>` then `EDONE` (or `ENROLLERR <msg>`/`EDONE`). The engine persists
/// it under the user-supplied name (the name never reaches the helper). Gated the same
/// way as `diarize` (enabled + a Core ML-resolvable provider) so the two stay consistent
/// and enrollment can't silently fetch models while diarization is off.
#[cfg(target_os = "macos")]
fn run_enroll(seconds: u64, cancel: &std::sync::atomic::AtomicBool) {
    use ds_stt::diarize::{CoremlDiarizer, Diarizer};
    use std::io::Write as _;

    let emit_err = |msg: &str| {
        println!("{}{}", proto::ENROLLERR_PREFIX, msg.replace('\n', " "));
        println!("{}", proto::EDONE);
        let _ = std::io::stdout().flush();
    };

    let cfg = ds_config::Paths::resolve().map(|p| ds_config::VoiceConfig::load(&p));
    let Some(cfg) = cfg else {
        return emit_err("config unavailable");
    };
    if !cfg.is_diarization_on() {
        return emit_err("diarization is disabled (set diarizer_provider to a non-empty ladder)");
    }
    if let Err(e) = ensure_coreml_diarizer(&cfg) {
        return emit_err(&e);
    }

    let pcm = match record_16k(seconds, cancel) {
        Ok(p) => p,
        Err(e) => return emit_err(&e),
    };
    let mut diarizer = CoremlDiarizer::new();
    match diarizer.embed(&pcm) {
        Ok(emb) => {
            let json = serde_json::json!(emb);
            println!("{}{json}", proto::EMB_PREFIX);
            println!("{}", proto::EDONE);
            let _ = std::io::stdout().flush();
        }
        Err(e) => emit_err(&e),
    }
}

// The warm synth/STT server is cross-platform: the body is rodio + ds_stt +
// ds_model (all portable) and `_exit` is an extern-C symbol present on every libc.
// Audio that can't open degrades via the `ERR audio` path below — there is no
// platform that needs a blanket "unsupported" stub.
//
// NOTE: the helper does NOT download models — not even the apple-native Core ML sets it
// loads (FluidAudio runs in `offlineMode`). EVERY model fetch goes through the engine's
// single-flight download manager (`dontspeakd::downloads`, targets `kokoro_coreml` /
// `parakeet_coreml` / …), which owns progress, failure surfacing, and the warm-child
// restart once the files land. A load attempted while the files are still absent simply
// fails; the engine restarts this helper after the fetch completes (the shared self-heal).

/// Leading silence prepended to EACH utterance's rodio sink, so the output-stream RESUME
/// latency (rodio pauses the CoreAudio output when idle) is absorbed by the silence instead of
/// clipping the speech onset — the "first speak, purple icon, no sound" fix. Pure + unit-tested
/// so it can't silently regress to 0 samples (which would re-break the onset).
const LEAD_SILENCE_MS: u32 = 80;

/// Feed VPIO in 100 ms source-rate chunks so mute/cancel state is observed throughout a
/// committed phoneme batch instead of only once for its synthesized PCM.
const DUPLEX_RENDER_CHUNK_SAMPLES: usize = ds_tts::SAMPLE_RATE as usize / 10;
/// Normal VPIO lookahead: enough to absorb scheduler jitter without buffering a whole reply.
const DUPLEX_RENDER_AHEAD: Duration = Duration::from_secs(2);
/// Keep muted silence shallow so unmuting resumes real audio promptly.
const DUPLEX_MUTED_RENDER_AHEAD: Duration = Duration::from_millis(100);
const DUPLEX_RENDER_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
struct DuplexRenderState {
    cancelled: bool,
    muted: bool,
    buffered: Duration,
}

/// Pace one already-transactional phoneme batch into VPIO while re-reading live mute/cancel
/// state. Returns `false` when cancellation stops the batch before every chunk is pushed.
fn push_duplex_pcm(
    pcm: Vec<f32>,
    mut read_state: impl FnMut() -> DuplexRenderState,
    mut push: impl FnMut(&[f32]),
    mut wait: impl FnMut(),
) -> bool {
    let silence = vec![0.0; DUPLEX_RENDER_CHUNK_SAMPLES];
    for chunk in pcm.chunks(DUPLEX_RENDER_CHUNK_SAMPLES) {
        let state = loop {
            let state = read_state();
            if state.cancelled {
                return false;
            }
            let limit = if state.muted {
                DUPLEX_MUTED_RENDER_AHEAD
            } else {
                DUPLEX_RENDER_AHEAD
            };
            if state.buffered < limit {
                break state;
            }
            wait();
        };
        push(if state.muted {
            &silence[..chunk.len()]
        } else {
            chunk
        });
    }
    true
}

/// `LEAD_SILENCE_MS` of mono silence at `srate_hz`. See [`LEAD_SILENCE_MS`].
fn leading_silence_pcm(srate_hz: u32) -> Vec<f32> {
    vec![0.0f32; srate_hz as usize * LEAD_SILENCE_MS as usize / 1000]
}

/// Block until a full-duplex dictation session releases the mic (`full_duplex_listening`
/// clears) OR shutdown fires (`capture_cancel`) — the full-duplex mutual exclusion between
/// dictation (the concurrent listen thread, reading the VPIO capture handle) and a
/// one-shot diarize/enroll job (its own independent cpal stream): half-duplex gets the
/// "one capture thread" guarantee for free (Listen/Diarize/Enroll dispatch one at a time
/// on the same playback-loop thread), but full-duplex routes `listen` to a SEPARATE
/// concurrent-listen thread — nothing else serializes the two capture streams.
/// `full_duplex_listening` is set the moment a full-duplex `listen` is requested and
/// cleared on `lstop`; a diarize/enroll job calls this BEFORE opening its own cpal stream.
/// Returns `false` the instant `capture_cancel` fires (including if it was ALREADY set
/// before this was even called) — the caller must then skip opening its own capture
/// rather than racing the process exit; `true` once the mic is genuinely free. Polls every
/// 30ms — cheap, and this only gates the rare diarize/enroll one-shot jobs. Unused (and so
/// `#[cfg]`'d out) on non-macOS: diarize/enroll never open a capture there.
#[cfg(target_os = "macos")]
fn wait_for_mic_free(
    full_duplex_listening: &std::sync::atomic::AtomicBool,
    capture_cancel: &std::sync::atomic::AtomicBool,
) -> bool {
    use std::sync::atomic::Ordering;
    while full_duplex_listening.load(Ordering::SeqCst) {
        if capture_cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    !capture_cancel.load(Ordering::SeqCst)
}

pub(crate) fn serve() -> ! {
    use std::io::{BufRead, Write};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    let tts_wanted = std::env::var_os("DONTSPEAK_TTS_PRELOAD").is_some();

    // ── STT (Parakeet) preloads in PARALLEL with the TTS load below ──────────────────────
    // Construct the transcriber first (cheap — no model load yet) and, when STT is wanted
    // (DONTSPEAK_STT_PRELOAD, set by the engine only for the built-in engine), preload it on its
    // OWN thread. So STT and TTS download/warm INDEPENDENTLY — each reports its own lifecycle
    // and neither blocks the other. The ONNX bootstrap's `ORT_DYLIB_PATH` write is serialized
    // by a Once in the model layer, so the two parallel loads don't race the env.
    // DONTSPEAK_STT_PROVIDER picks the local backend: "ane" → FluidAudio Core ML / ANE,
    // "cpu" → portable ONNX Parakeet. Shared (Arc<Mutex>) so the preload thread, the
    // full-duplex concurrent-listen thread, and the request loop all reach it.
    let parakeet_dir = ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE)
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let stt_provider = std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default();
    let transcriber = Arc::new(Mutex::new(ds_stt::LocalTranscriber::for_provider(
        &stt_provider,
        parakeet_dir,
    )));
    // The `transcriber` cache is managed by preload/load/unload stt. Real listen falls back to
    // it only for non-streaming backends. Streaming providers use a separate `backend_cell` (in
    // listen.rs) that is also preloaded/unloaded — every call site below tries it FIRST. `unload
    // stt` drives both. `STTLOADED`/`STT_PROVIDER` are reported from whichever cache actually
    // loaded (`loaded`, a few lines down), not hardcoded to `transcriber`. See `SttResidencySlot`.
    // Claimed the MOMENT the STT load starts — by the parallel preload below OR a later
    // `load stt` request — so the two can't BOTH load the model concurrently. See
    // `SttResidencySlot`: `Idle -> Loading -> Loaded`, with `Loading`/`Loaded` only ever
    // exiting via `resolve_ok`/`mark_unloaded`, so the claim can't get stuck.
    let stt_claimed = Arc::new(SttResidencySlot::new());
    if std::env::var_os("DONTSPEAK_STT_PRELOAD").is_some() {
        // Claim BEFORE spawning so a `load stt` that races in skips its own load. Nothing
        // else has claimed yet at this point in startup, so this always succeeds.
        stt_claimed.try_claim();
        let transcriber = transcriber.clone();
        let stt_provider = stt_provider.clone();
        let stt_claimed = stt_claimed.clone();
        std::thread::spawn(move || {
            // Loading + warming (the model files were fetched by the ENGINE's download
            // manager before this helper was (re)started; preload() loads them offline).
            // "Starting…" until STTLOADED (preload runs a warmup inference, so STTLOADED
            // honestly means resident + warm).
            println!("{}stt", proto::WARMING_PREFIX);
            let _ = std::io::stdout().flush();
            log::info!(target: "helper", "load stt attempting (provider={stt_provider})");
            let loaded = if let Some(provider) = crate::listen::preload_streaming(&stt_provider) {
                Ok(provider)
            } else {
                let mut t = transcriber.lock().unwrap_or_else(|e| e.into_inner());
                t.preload().map(|()| t.provider())
            };
            match loaded {
                Ok(provider) => {
                    println!("{}", proto::STTLOADED);
                    println!("{}{}", proto::STT_PROVIDER_PREFIX, provider.as_str());
                    let _ = std::io::stdout().flush();
                    stt_claimed.resolve_ok();
                }
                Err(e) => {
                    // Release the claim on failure (e.g. the model isn't downloaded yet on a
                    // fresh install) so a LATER `load stt` — sent whenever the engine reconciles
                    // the helper's models again — can actually retry. Without this, the claim
                    // stayed true forever: the very first failed attempt permanently wedged
                    // STT off for this process's whole lifetime, recoverable only by the daemon
                    // happening to fully RESTART this helper (mirrors Job::LoadTts below, which
                    // rechecks `synth.is_none()` fresh each time instead of a one-shot latch).
                    stt_claimed.mark_unloaded();
                    println!("{}{e}", proto::STTLOADERR_PREFIX);
                    let _ = std::io::stdout().flush();
                    log::warn!(target: "helper", "preload stt failed: {e}");
                }
            }
        });
    }

    // LOADING + warming the synth (the model files — ONNX or Core ML — were fetched by the
    // ENGINE's download manager; load_backend only loads them offline). Tell the engine so
    // the dot reads "Starting…" through the load + warmup (which can be slow on the first
    // ANE compile), instead of a premature green, until READY below.
    if tts_wanted {
        println!("{}tts", proto::WARMING_PREFIX);
        let _ = std::io::stdout().flush();
    }
    // Load once. READY/ERR let the UI know the model is warm.
    // Held as Option so a `unload tts` can free the Kokoro model while the helper
    // stays warm for STT; the next speak lazily reloads it (below).
    let mut synth = if tts_wanted {
        match load_backend() {
            Ok(s) => {
                // PROVIDER (before READY) lets the engine report the active execution provider.
                // READY is emitted LATER — only after the audio OUTPUT is opened + primed below —
                // so green honestly means "warm AND able to make sound", not just "model loaded".
                println!("{}{}", proto::PROVIDER_PREFIX, s.provider().as_str());
                let _ = std::io::stdout().flush();
                Some(s)
            }
            Err(e) => {
                println!("{} {}", proto::ERR, one_line(&e));
                let _ = std::io::stdout().flush();
                // SAFETY: `_exit` takes only an exit code and never returns; skipping Rust
                // destructors is this crate's teardown convention (see main.rs's top
                // comment — ort/cpal abort on teardown).
                unsafe { _exit(1) };
            }
        }
    } else {
        None
    };

    /// A playback request the loop will synth + play.
    struct PlayReq {
        voice: String,
        rate: f32,
        text: String,
    }
    struct State {
        req: Option<PlayReq>,
        /// A `listen` (STT) job was requested. Mutually exclusive with TTS playback
        /// (the engine never speaks and listens at once — the mic-barge gates them).
        listen: Option<u64>,
        quit: bool,
        /// `unload` requests: free the cached Kokoro (tts) / Parakeet (stt) model
        /// when the engine no longer needs it but the helper stays warm for the other.
        unload_tts: bool,
        unload_stt: bool,
        /// `load` requests: eagerly (pre)load a model so it's resident the moment its
        /// engine is selected — keeps "loaded" honest BEFORE first use (Parakeet is
        /// otherwise lazy), so the UI's green dot matches actual residency.
        load_tts: bool,
        load_stt: bool,
        /// A one-shot `diarize` job: record this many seconds, then diarize. Like
        /// `listen` it's mutually exclusive with TTS playback (single capture thread).
        diarize: Option<u64>,
        /// A one-shot `enroll` job: record this many seconds, then extract a voiceprint.
        enroll: Option<u64>,
    }
    let shared = Arc::new((
        Mutex::new(State {
            req: None,
            listen: None,
            quit: false,
            unload_tts: false,
            unload_stt: false,
            load_tts: false,
            load_stt: false,
            diarize: None,
            enroll: None,
        }),
        Condvar::new(),
    ));
    // (`transcriber` + `stt_provider` were constructed at the top of `serve()` so STT could
    // preload in parallel with the TTS load; both are in scope here for the request loop.)
    // Diarize/enroll construct their own CoremlDiarizer per call (reading the config's
    // clustering threshold fresh), so there is no persistent diarizer local here.
    // Full-duplex AEC (macOS VPIO): when DONTSPEAK_FULL_DUPLEX is set we render TTS
    // AND capture STT through ONE echo-cancelled unit, so STT never hears the TTS.
    // Falls back to the half-duplex rodio + cpal path when unset or the unit won't
    // open. Coexist is LIVE: a dedicated concurrent-listen thread (below) drains the
    // echo-cancelled mic WHILE this thread renders TTS, so the user dictates over
    // the voice. There is no implicit voice-barge — stopping it is an explicit `stop` /
    // Caps long-press.
    let duplex: Option<DuplexAudio> = if std::env::var_os("DONTSPEAK_FULL_DUPLEX").is_some() {
        match DuplexAudio::open() {
            Ok(d) => {
                log::info!(target: "helper", "full-duplex AEC active ({} Hz capture)", d.capture_rate());
                Some(d)
            }
            Err(e) => {
                log::warn!(target: "helper", "full-duplex unavailable ({e}); half-duplex");
                None
            }
        }
    } else {
        None
    };
    // Whether the duplex backend owns the render path (macOS VPIO: TTS is rendered
    // THROUGH the unit as the AEC reference, so we skip rodio). Capture-side backends
    // (Windows WASAPI Communications, Linux module-echo-cancel) return false: rodio
    // still renders and the duplex only supplies the echo-cancelled capture.
    let render_via_duplex = duplex.as_ref().is_some_and(|d| d.owns_render());
    let tts_output_available = tts_output_available(tts_wanted, render_via_duplex);
    // One persistent audio device (the cpal stream is !Send → it must stay on THIS
    // playback thread). `log_on_drop(false)` + `_exit` on quit avoid the macOS-26
    // CoreAudio teardown abort. Per-request `Player`s are created on its mixer.
    // Skipped only when the duplex backend owns render (macOS VPIO); a capture-only
    // duplex keeps rodio for output.
    let device = if render_via_duplex || !tts_wanted {
        None
    } else {
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(mut d) => {
                d.log_on_drop(false);
                Some(d)
            }
            Err(e) => {
                println!("{} audio: {}", proto::ERR, one_line(&e.to_string()));
                let _ = std::io::stdout().flush();
                // SAFETY: `_exit` takes only an exit code and never returns; skipping
                // Rust destructors is this crate's teardown convention (see main.rs's
                // top comment).
                unsafe { _exit(1) };
            }
        }
    };
    // An OWNED, `Send` clone of the device mixer (it's an `Arc` handle) for ordered one-shot
    // EARCONS. `None` when the duplex backend owns render (macOS VPIO, no rodio mixer); the
    // tracked cue then uses `afplay` on macOS (see the `cue` op below).
    let cue_mixer = device.as_ref().map(|d| d.mixer().clone());
    let cue_playback = Arc::new(CuePlayback::default());
    // The model is loaded + warm and the output device is open → signal READY (green). The
    // audio-stream RESUME latency (rodio pauses the CoreAudio output when idle) is handled
    // per-utterance below — a brief leading silence absorbs the resume so the speech onset
    // isn't clipped (the "purple icon, no sound" first speak).
    println!("{}", proto::READY);
    let _ = std::io::stdout().flush();
    // A `Send` handle so the stdin reader can barge the VPIO render from its thread
    // (the unit itself is !Send and lives here on the playback thread).
    let duplex_barge: Option<std::sync::Arc<AtomicBool>> = duplex.as_ref().map(|d| d.barge_flag());
    let full_duplex_listening = Arc::new(AtomicBool::new(false));
    let capturing_diarize = Arc::new(AtomicBool::new(false));
    let listen_stopped_through = Arc::new(AtomicU64::new(0));
    let listen_latest_generation = Arc::new(AtomicU64::new(0));
    // Full-duplex COEXIST: spawn the concurrent listen thread (drains the
    // echo-cancelled mic + transcribes while this thread renders TTS). `listen_sig`
    // is the reader→thread control (Some only in full-duplex).
    let listen_sig: Option<Arc<(Mutex<ListenSig>, Condvar)>> = duplex.as_ref().and_then(|dx| {
        let sig = Arc::new((Mutex::new(ListenSig::default()), Condvar::new()));
        let capture = dx.capture_handle();
        let tr = transcriber.clone();
        let sig2 = sig.clone();
        let stopped = listen_stopped_through.clone();
        let capturing = capturing_diarize.clone();
        let listening = full_duplex_listening.clone();
        match std::thread::Builder::new()
            .name("ds-listen".into())
            .spawn(move || concurrent_listen_loop(capture, tr, sig2, stopped, capturing, listening))
        {
            Ok(_) => Some(sig),
            Err(e) => {
                eprintln!(
                    "dontspeak-helper: could not spawn full-duplex listener ({e}); falling back to half-duplex"
                );
                None
            }
        }
    });
    // Full-duplex mutual exclusion between dictation (the concurrent listen thread
    // above, reading the VPIO capture handle) and the one-shot diarize/enroll capture
    // (its own independent cpal stream, opened in `run_diarize`/`run_enroll` below):
    // half-duplex gets the "one capture thread" guarantee for free because
    // Listen/Diarize/Enroll are jobs dispatched one at a time on the SAME playback-
    // loop thread, but full-duplex routes `listen` to the SEPARATE concurrent-listen
    // thread — nothing serialized the two capture streams before (the documented
    // "one capture thread" mutual exclusion only held in half-duplex).
    // `full_duplex_listening` is set the moment a full-duplex `listen` is requested
    // (before the concurrent thread actually reads the mic) and cleared on `lstop`;
    // the diarize/enroll job below waits for it to clear before opening its own cpal
    // stream. `capturing_diarize` is the reverse guard: set while that capture is in
    // flight, so a `listen` request arriving mid-capture waits it out (off the reader
    // thread, so a slow diarize doesn't stall stdin reading) before waking the
    // concurrent thread. Plain flags, not a perfectly atomic handoff — there is a
    // short window at the boundary while the other side's stream is actually
    // finishing/tearing down — but this closes the "whole session" overlap the audit
    // flagged. No-ops in half-duplex (`listen_sig` is `None`, so
    // `full_duplex_listening` is never set).
    // The CURRENT request's player, shared with the reader thread for INSTANT barge
    // (`stop()` is a non-blocking flag; the player is discarded after each request).
    let cur_player: Arc<Mutex<Option<Arc<rodio::Player>>>> = Arc::new(Mutex::new(None));
    // Set by the reader on `stop` OR a newer request; the playback loop checks it
    // between phoneme batches and during afplay polling so a barge-in interrupts
    // even mid-synthesis. Reset to false when the loop dequeues a fresh request.
    let cancel = Arc::new(AtomicBool::new(false));
    // A SEPARATE cancel for the one-shot CAPTURE jobs (diarize/enroll). Unlike `cancel`,
    // it is NOT tripped by a TTS barge (`speak`/`stop`/`stopfade`) — those routinely
    // arrive mid-recording (warm-engine pings, narration, record-barges) and must NOT
    // abort a diarize/enroll capture. It trips only on engine shutdown (stdin EOF), so a
    // killed engine still ends the recording. (`listen` keeps using `cancel`: a `stop`
    // SHOULD end a dictation.)
    let capture_cancel = Arc::new(AtomicBool::new(false));
    // The half-duplex `listen` (dictation) cancel. Like `capture_cancel` it is NOT tripped
    // by a TTS `speak` barge — the engine "never speaks and listens at once", so a `speak`
    // arriving mid-listen (narration) must QUEUE behind the dictation, not abort it (which
    // truncated the capture). It trips on the INTENDED stops only: `stop` / `lstop` (the
    // seconds-timer + Caps release) and shutdown (stdin EOF).
    // MUTE: speech keeps draining silently, while one-shot cues are suppressed or stopped.
    // Read by the paced VPIO feeder and applied instantly to the sounding rodio player.
    let muted = Arc::new(AtomicBool::new(false));

    // Reader thread: parse JSON requests. speak/preview enqueue (newest wins) and
    // cancel any current playback; stop only cancels (no enqueue, no DONE).
    {
        let shared = shared.clone();
        let cur_player = cur_player.clone();
        let cancel = cancel.clone();
        let capture_cancel = capture_cancel.clone();
        let listen_stopped_through = listen_stopped_through.clone();
        let listen_latest_generation = listen_latest_generation.clone();
        let duplex_barge = duplex_barge.clone();
        let listen_sig = listen_sig.clone();
        let muted = muted.clone();
        let cue_mixer = cue_mixer.clone();
        let cue_playback = cue_playback.clone();
        let full_duplex_listening = full_duplex_listening.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(req) = serde_json::from_str::<ServeReq>(line) else {
                    continue; // ignore malformed lines rather than desync
                };
                let voice = if req.voice.trim().is_empty() {
                    ds_config::DEFAULT_KOKORO_VOICE.to_string()
                } else {
                    req.voice
                };
                let cancel_current = || {
                    // Signal the playback loop to stop the in-flight request, then
                    // stop the player sounding right now (non-blocking flag). In
                    // full-duplex mode there is no rodio player — drain the VPIO
                    // render ring via its barge flag instead.
                    cancel.store(true, Ordering::SeqCst);
                    if let Some(p) = cur_player
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                    {
                        p.stop();
                    }
                    if let Some(f) = &duplex_barge {
                        f.store(true, Ordering::SeqCst);
                    }
                    cue_playback.cancel();
                };
                // Graceful variant: ramp the rodio player's volume to zero over a SHORT
                // window so NO explicit barge is a hard cut/click — used by every user-
                // facing stop (mic record-barge, the caps long-press reset, per-window
                // clear-on-submit / window close / newest-reply preempt). The helper's
                // INTERNAL block-to-block preempt keeps using the instant `cancel_current`
                // so sequential narration has no gap between blocks. Full-duplex has no
                // rodio player, so this degrades to the instant VPIO-ring drain below.
                // ~60 ms is short enough to stay responsive and limit bleed into the mic
                // on a record-barge, yet long enough to de-click.
                let cancel_current_fade = || {
                    cancel.store(true, Ordering::SeqCst);
                    cue_playback.cancel();
                    // Clone the Arc out so the ramp does NOT hold the `cur_player` lock
                    // (the playback loop touches it too).
                    let player = cur_player
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_ref()
                        .cloned();
                    let duplex_barge = duplex_barge.clone();
                    let _ = std::thread::Builder::new()
                        .name("ds-stopfade".into())
                        .spawn(move || {
                            if let Some(p) = player {
                                const STEPS: u32 = 12;
                                let start = p.volume();
                                let step = std::time::Duration::from_millis(60) / STEPS;
                                for i in 1..=STEPS {
                                    p.set_volume(
                                        (start * (1.0 - i as f32 / STEPS as f32)).max(0.0),
                                    );
                                    std::thread::sleep(step);
                                }
                                p.stop();
                            }
                            if let Some(f) = &duplex_barge {
                                f.store(true, Ordering::SeqCst);
                            }
                        });
                };
                match req.op.as_str() {
                    "stop" => {
                        cancel_current(); // silent: no enqueue, no DONE
                        let generation = listen_latest_generation.load(Ordering::SeqCst);
                        listen_stopped_through.fetch_max(generation, Ordering::SeqCst);
                        if let Some(sig) = &listen_sig {
                            sig.1.notify_one();
                        }
                    }
                    "mute" => {
                        // Speech keeps draining silently. Cues are one-shot signals, so mute
                        // stops an active one and suppresses later ones rather than resurrecting
                        // them on unmute.
                        let on = matches!(
                            req.text.trim().to_ascii_lowercase().as_str(),
                            "on" | "true" | "1" | "yes"
                        );
                        muted.store(on, Ordering::SeqCst);
                        cue_playback.set_muted(on);
                        if let Some(p) = cur_player
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                        {
                            p.set_volume(if on { 0.0 } else { 1.0 });
                        }
                    }
                    "stopfade" => cancel_current_fade(), // graceful per-window barge (fade then stop)
                    "cue" => {
                        // Cue playback stays off the stdin reader so mute/stop remain live, but
                        // its CUEDONE terminal makes the engine queue wait before starting the
                        // next action. The tracked handle lets mute stop an already-sounding cue.
                        let path = req.text.clone();
                        let Some(generation) = cue_playback.begin() else {
                            emit_cue_done();
                            continue;
                        };
                        if let Some(mixer) = cue_mixer.clone() {
                            let cue_playback = cue_playback.clone();
                            std::thread::spawn(move || {
                                let Ok(file) = std::fs::File::open(&path) else {
                                    cue_playback.finish(generation);
                                    emit_cue_done();
                                    return;
                                };
                                let Ok(decoder) =
                                    rodio::Decoder::new(std::io::BufReader::new(file))
                                else {
                                    cue_playback.finish(generation);
                                    emit_cue_done();
                                    return;
                                };
                                let player = Arc::new(rodio::Player::connect_new(&mixer));
                                // Append BEFORE install: install_player's rejection path stops
                                // the player, so a mute/cancel racing this thread kills the
                                // appended source instead of stopping an empty player and
                                // letting the later append play in full.
                                player.append(decoder);
                                if !cue_playback.install_player(generation, player.clone()) {
                                    cue_playback.finish(generation);
                                    emit_cue_done();
                                    return;
                                }
                                player.sleep_until_end();
                                cue_playback.finish(generation);
                                emit_cue_done();
                            });
                        } else {
                            #[cfg(target_os = "macos")]
                            {
                                let cue_playback = cue_playback.clone();
                                std::thread::spawn(move || {
                                    if let Ok(child) =
                                        std::process::Command::new("afplay").arg(&path).spawn()
                                        && cue_playback.install_afplay(generation, child)
                                    {
                                        cue_playback.wait_afplay(generation);
                                    }
                                    cue_playback.finish(generation);
                                    emit_cue_done();
                                });
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                cue_playback.finish(generation);
                                emit_cue_done();
                            }
                        }
                    }
                    "speak" => {
                        let text = req.text;
                        barge_then_publish(&shared, cancel_current, |state| {
                            state.req = Some(PlayReq {
                                voice,
                                rate: req.rate,
                                text,
                            });
                        });
                    }
                    "listen" => {
                        let generation = req.session.unwrap_or(1);
                        listen_latest_generation.fetch_max(generation, Ordering::SeqCst);
                        if let Some(sig) = &listen_sig {
                            // Full-duplex COEXIST: wake the concurrent listen thread;
                            // do NOT cancel an in-flight speak. Claim the mic for
                            // dictation IMMEDIATELY (blocks a diarize/enroll job from
                            // opening its own capture — see `full_duplex_listening`
                            // above), then — off THIS thread, so a slow diarize/enroll
                            // capture doesn't stall stdin reading — wait out any such
                            // capture already in flight before actually waking the
                            // concurrent listen thread, so the two capture streams
                            // never run concurrently on the same mic.
                            full_duplex_listening.store(true, Ordering::SeqCst);
                            let (m, cv) = &**sig;
                            m.lock().unwrap_or_else(|e| e.into_inner()).start = Some(generation);
                            cv.notify_one();
                        } else {
                            // Half-duplex: serve-loop listen, mutually exclusive w/ speak.
                            barge_then_publish(&shared, cancel_current, |state| {
                                state.listen = Some(generation)
                            });
                        }
                    }
                    "lstop" => {
                        let generation = req.session.unwrap_or(u64::MAX);
                        listen_stopped_through.fetch_max(generation, Ordering::SeqCst);
                        // End the listen WITHOUT touching the speak (coexist). In
                        // half-duplex it's the serve-loop listen, ended via listen_cancel
                        // (NOT the TTS `cancel`, so a queued speak isn't disturbed).
                        if let Some(sig) = &listen_sig {
                            sig.1.notify_one();
                        }
                    }
                    "diarize" => {
                        // One-shot record-then-diarize. Runs on the serve loop's single
                        // capture thread, so it's mutually exclusive with speak/listen —
                        // cancel any in-flight playback, then queue the job.
                        let secs = req.seconds.unwrap_or(10).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap_or_else(|e| e.into_inner()).diarize = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    "enroll" => {
                        // One-shot record-then-extract-voiceprint (same capture thread).
                        let secs = req.seconds.unwrap_or(15).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap_or_else(|e| e.into_inner()).enroll = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    "unload" => {
                        // Free a cached model the engine no longer needs while the
                        // OTHER engine keeps the helper warm. Idle-only (the playback
                        // loop runs it between jobs); no cancel.
                        let (m, cv) = &*shared;
                        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
                        match req.engine.as_str() {
                            "tts" => s.unload_tts = true,
                            "stt" => s.unload_stt = true,
                            _ => {}
                        }
                        cv.notify_one();
                    }
                    "load" => {
                        // Eagerly (pre)load a model so it's resident before first use.
                        let (m, cv) = &*shared;
                        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
                        match req.engine.as_str() {
                            "tts" => s.load_tts = true,
                            "stt" => s.load_stt = true,
                            _ => {}
                        }
                        cv.notify_one();
                    }
                    _ => {} // unknown op: ignore
                }
            }
            // stdin closed (engine/UI quit, or the engine was killed — the OS
            // closes the pipe either way): STOP IMMEDIATELY. Cancel the in-flight
            // playback and kill the afplay actually sounding, drop any pending
            // request, and tell the loop to exit. Without this the child drained
            // the current reply (and queue) before exiting, so a killed/quit
            // engine kept talking — the "playback continues after exit / queue not
            // cleared on kill" bug. Nothing here survives the process, so there is
            // no stale queue to replay on the next engine start.
            cancel.store(true, Ordering::SeqCst);
            cue_playback.cancel();
            // End an in-flight diarize/enroll capture AND a half-duplex listen too (both
            // ignore the TTS `cancel`).
            capture_cancel.store(true, Ordering::SeqCst);
            listen_stopped_through.store(u64::MAX, Ordering::SeqCst);
            // Release the full-duplex mic claim too, so anything still waiting on
            // `full_duplex_listening` (see above) unblocks via `capture_cancel` rather
            // than hanging — the wait loops also check `capture_cancel` directly, this
            // is just belt-and-suspenders.
            if let Some(p) = cur_player
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                p.stop();
            }
            if let Some(f) = &duplex_barge {
                f.store(true, Ordering::SeqCst);
            }
            if let Some(sig) = &listen_sig {
                let (m, cv) = &**sig;
                let mut ls = m.lock().unwrap_or_else(|e| e.into_inner());
                ls.quit = true;
                cv.notify_one();
            }
            let (m, cv) = &*shared;
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            s.req = None; // do NOT drain a pending request on quit
            s.listen = None;
            // Also drop every OTHER pending job kind — previously only `req`/`listen`
            // were cleared here, so a diarize/enroll/(un)load queued at the same moment
            // as EOF would still run AFTER quit was signaled: diarize/enroll would open
            // the mic post-shutdown, and a load/unload would spend multi-second model
            // (re)load time the engine is no longer around to see the result of.
            s.diarize = None;
            s.enroll = None;
            s.unload_tts = false;
            s.unload_stt = false;
            s.load_tts = false;
            s.load_stt = false;
            s.quit = true;
            cv.notify_one();
        });
    }

    // Playback loop (owns the synth; single-threaded synthesis). Synth + play one
    // phoneme batch at a time, checking `cancel` between batches and during
    // playback, so a barge-in cuts in promptly instead of after the whole reply.
    loop {
        // Wait for a speak OR listen job (or quit). One job at a time — TTS and STT
        // are mutually exclusive on this single thread (Capture's cpal stream is
        // !Send, and the engine never speaks + listens at once).
        enum Job {
            Speak(PlayReq),
            Listen(u64),
            Diarize(u64),
            Enroll(u64),
            UnloadTts,
            UnloadStt,
            LoadTts,
            LoadStt,
        }
        let job = {
            let (m, cv) = &*shared;
            let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
            while s.req.is_none()
                && s.listen.is_none()
                && s.diarize.is_none()
                && s.enroll.is_none()
                && !s.quit
                && !s.unload_tts
                && !s.unload_stt
                && !s.load_tts
                && !s.load_stt
            {
                s = cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            // Drain a pending job even if `quit` also arrived; exit only when idle+quit.
            if let Some(r) = s.req.take() {
                Job::Speak(r)
            } else if let Some(generation) = s.listen.take() {
                Job::Listen(generation)
            } else if let Some(secs) = s.diarize.take() {
                Job::Diarize(secs)
            } else if let Some(secs) = s.enroll.take() {
                Job::Enroll(secs)
            } else if s.unload_tts {
                s.unload_tts = false;
                Job::UnloadTts
            } else if s.unload_stt {
                s.unload_stt = false;
                Job::UnloadStt
            } else if s.load_tts {
                s.load_tts = false;
                Job::LoadTts
            } else if s.load_stt {
                s.load_stt = false;
                Job::LoadStt
            } else {
                drop(s);
                // SAFETY: `_exit` takes only an exit code and never returns; skipping
                // Rust destructors is this crate's teardown convention (see main.rs's
                // top comment).
                unsafe { _exit(0) };
            }
        };
        // A fresh playback job clears playback barge state. Listen cancellation is
        // generation-based and deliberately never reset here: an early lstop must survive
        // queueing and prevent the matching capture from opening.
        cancel.store(false, Ordering::SeqCst);

        // STT job: capture + stream partials + final, then back to waiting.
        let PlayReq { voice, rate, text } = match job {
            Job::Speak(r) => r,
            Job::Listen(generation) => {
                // Half-duplex only (full-duplex routes to the concurrent thread, so
                // `duplex` is always None here — its old VPIO capture path is gone).
                // Uses `listen_cancel` so a TTS speak barge can't truncate the dictation;
                // only stop/lstop (the seconds-timer / Caps release) and EOF end it.
                run_listen(
                    &mut transcriber.lock().unwrap_or_else(|e| e.into_inner()),
                    &listen_stopped_through,
                    generation,
                );
                continue;
            }
            Job::Diarize(secs) => {
                // One-shot: record `secs` of mic, then diarize. macOS-only (FluidAudio
                // Core ML); off macOS the cross-platform ONNX backend isn't wired yet.
                // Uses `capture_cancel` so a TTS barge can't abort the recording.
                #[cfg(target_os = "macos")]
                {
                    // Full-duplex mutual exclusion: don't open our own cpal capture
                    // while a full-duplex dictation is reading the VPIO handle — the
                    // two would otherwise be independent streams on the same mic (see
                    // `full_duplex_listening` above). No-op in half-duplex, where this
                    // job already can't overlap a listen (single dispatch thread).
                    if wait_for_mic_free(&full_duplex_listening, &capture_cancel) {
                        capturing_diarize.store(true, Ordering::SeqCst);
                        run_diarize(secs, &capture_cancel);
                        capturing_diarize.store(false, Ordering::SeqCst);
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = secs;
                    use std::io::Write as _;
                    println!(
                        "{}diarization is only available on macOS",
                        proto::DIARERR_PREFIX
                    );
                    println!("{}", proto::DDONE);
                    let _ = std::io::stdout().flush();
                }
                continue;
            }
            Job::Enroll(secs) => {
                // One-shot: record `secs` of mic, then extract a voiceprint. macOS-only.
                // Uses `capture_cancel` so a TTS barge can't abort the recording.
                #[cfg(target_os = "macos")]
                {
                    // See the matching full-duplex mutual-exclusion comment on
                    // `Job::Diarize` above.
                    if wait_for_mic_free(&full_duplex_listening, &capture_cancel) {
                        capturing_diarize.store(true, Ordering::SeqCst);
                        run_enroll(secs, &capture_cancel);
                        capturing_diarize.store(false, Ordering::SeqCst);
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = secs;
                    use std::io::Write as _;
                    println!(
                        "{}enrollment is only available on macOS",
                        proto::ENROLLERR_PREFIX
                    );
                    println!("{}", proto::EDONE);
                    let _ = std::io::stdout().flush();
                }
                continue;
            }
            Job::UnloadTts => {
                // Drop the cached Kokoro model; the next speak lazily reloads it.
                let freed = synth.take().is_some();
                log::info!(target: "helper", "unloaded tts (kokoro), freed={freed}");
                continue;
            }
            Job::UnloadStt => {
                // Free BOTH caches: `transcriber` (constructed in `serve()` above) and, for
                // streaming-capable providers (cpu/cuda/ane), the SEPARATE resident model a
                // real listen actually runs on (listen.rs's `backend_cell`) — otherwise that
                // one stayed loaded, roughly doubling STT memory after the first listen.
                let mut t = transcriber.lock().unwrap_or_else(|e| e.into_inner());
                let freed_offline = t.unload();
                // Release the claim so a LATER `load stt` (a real off→on toggle) can
                // actually claim + attempt a fresh load — `mark_unloaded` is valid from
                // ANY state, so this can't leave the claim stuck from the original
                // successful load the way the old raw flag could. Kept inside the same
                // `transcriber` guard as the unload itself, matching the other three
                // `SttResidencySlot` call sites (the startup preload thread, `Job::LoadStt`'s
                // success/failure arms) so all four are structurally consistent.
                stt_claimed.mark_unloaded();
                drop(t);
                let freed_streaming = crate::listen::unload_streaming();
                log::info!(
                    target: "helper",
                    "unloaded stt (parakeet), freed={} (offline={freed_offline}, streaming={freed_streaming})",
                    freed_offline || freed_streaming
                );
                continue;
            }
            Job::LoadTts => {
                // Mirror Job::LoadStt: (re)load Kokoro if a prior `unload tts` freed it, then
                // CONFIRM residency with `TTSLOADED` so the engine greens the dot only after the
                // model is truly resident — never on the mere `load` request (the old optimistic
                // green). Already-resident ⇒ still confirm, so a reconcile after startup keeps the
                // flag honest.
                if !tts_output_available {
                    use std::io::Write as _;
                    println!("{}{}", proto::TTSLOADERR_PREFIX, TTS_OUTPUT_UNAVAILABLE);
                    let _ = std::io::stdout().flush();
                    continue;
                }
                let mut resident = synth.is_some();
                if synth.is_none() {
                    match load_backend() {
                        Ok(s) => {
                            synth = Some(s);
                            resident = true;
                        }
                        Err(e) => {
                            use std::io::Write as _;
                            println!("{}{}", proto::TTSLOADERR_PREFIX, one_line(&e));
                            let _ = std::io::stdout().flush();
                            log::warn!(target: "helper", "preload tts failed: {e}");
                        }
                    }
                }
                if resident {
                    use std::io::Write as _;
                    println!("{}", proto::TTSLOADED);
                    let _ = std::io::stdout().flush();
                }
                continue;
            }
            Job::LoadStt => {
                // STT is normally preloaded in PARALLEL at startup (the thread in `serve()`), so
                // this engine-sent `load stt` is usually redundant. SINGLE-FLIGHT it: if the
                // load is already claimed (by that preload, or a prior `load stt`), skip — else
                // claim it and load HERE (STT became wanted after startup, or preload was off).
                if !stt_claimed.try_claim() {
                    log::info!(target: "helper", "load stt skipped — a load is already claimed/in flight");
                    continue;
                }
                log::info!(target: "helper", "load stt attempting (provider={stt_provider})");
                let loaded = if let Some(provider) = crate::listen::preload_streaming(&stt_provider)
                {
                    Ok(provider)
                } else {
                    let mut t = transcriber.lock().unwrap_or_else(|e| e.into_inner());
                    t.preload().map(|()| t.provider())
                };
                match loaded {
                    Ok(provider) => {
                        println!("{}", proto::STTLOADED);
                        println!("{}{}", proto::STT_PROVIDER_PREFIX, provider.as_str());
                        let _ = std::io::stdout().flush();
                        stt_claimed.resolve_ok();
                    }
                    Err(e) => {
                        // Release the claim on failure so the NEXT `load stt` (e.g. the engine
                        // reconciling again once the file it's still fetching actually lands)
                        // can retry instead of silently no-op'ing on the stuck claim forever.
                        stt_claimed.mark_unloaded();
                        println!("{}{e}", proto::STTLOADERR_PREFIX);
                        let _ = std::io::stdout().flush();
                        log::warn!(target: "helper", "preload stt failed: {e}");
                    }
                }
                continue;
            }
        };

        // A helper started for STT only deliberately owns no playback sink. Refuse the whole
        // request before frontend/backend work so it cannot discard PCM and report STATS+DONE.
        if !tts_output_available {
            println!("{} {}", proto::ERR, TTS_OUTPUT_UNAVAILABLE);
            let _ = std::io::stdout().flush();
            continue;
        }

        // Run the frontend before touching the backend. Empty/image/emoji/punctuation-only
        // requests are successful no-ops even after `unload tts`; they must not pay for a model
        // reload or turn a missing model into TTSLOADERR + ERR when no synthesis was requested.
        let phoneme_batches = match g2p::phoneme_batches_for_cancellable(&text, &voice, || {
            cancel.load(Ordering::SeqCst)
        }) {
            Ok(PhonemeBatchesOutcome::Finished(batches)) => batches,
            Ok(PhonemeBatchesOutcome::Cancelled) => {
                println!("{}", proto::DONE);
                let _ = std::io::stdout().flush();
                continue;
            }
            Err(e) => {
                log::warn!(target: "helper", "Kokoro frontend failed: {e}");
                println!("{} {}", proto::ERR, one_line(&e));
                let _ = std::io::stdout().flush();
                continue;
            }
        };
        if phoneme_batches.is_empty() {
            println!("{}", proto::DONE);
            let _ = std::io::stdout().flush();
            continue;
        }
        // A stop/barge that landed during the frontend phase (Markdown → IPA, including
        // per-OOV BART inference — potentially seconds on identifier-heavy text) must not
        // start a backend load or synthesis it no longer needs.
        if cancel.load(Ordering::SeqCst) {
            println!("{}", proto::DONE);
            let _ = std::io::stdout().flush();
            continue;
        }

        // Lazily (re)load the Kokoro synth if a prior `unload tts` freed it.
        if synth.is_none() {
            match load_backend() {
                Ok(s) => {
                    synth = Some(s);
                    // Confirm residency to the engine (mirrors Job::LoadTts's success arm)
                    // so `dontspeakd`'s reader_loop clears any stale `tts_load_error` and
                    // marks the engine loaded again — without this the lazy reload silently
                    // resurrected the model but never told the engine it happened.
                    println!("{}", proto::TTSLOADED);
                    let _ = std::io::stdout().flush();
                }
                Err(e) => {
                    log::warn!(target: "helper", "synth reload failed: {e}");
                    println!("{}{}", proto::TTSLOADERR_PREFIX, one_line(&e));
                    println!("{} {}", proto::ERR, one_line(&e));
                    let _ = std::io::stdout().flush();
                    continue;
                }
            }
        }
        let synth = synth.as_mut().expect("synth loaded above");

        // ONE Rust frontend normalizes, phonemizes, and bounds the request before the backend
        // split. ONNX and Core ML consume these exact IPA batches. Each complete IPA batch is
        // synthesized and validated before it is committed, then playback can overlap inference
        // for the remaining batches — see `prepare`.
        let t_req = std::time::Instant::now();
        let channels = std::num::NonZero::new(1u16).expect("1 channel");
        let srate = std::num::NonZero::new(24_000u32).expect("24 kHz");
        let mut player: Option<Arc<rodio::Player>> = None;
        let mut synth_nanos = 0u128;
        let mut total_samples = 0usize;
        let mut first_ms = 0.0f64;
        let mut commit = |audio: PreparedAudio| -> Result<(), String> {
            synth_nanos = synth_nanos.saturating_add(audio.synth_nanos);
            if total_samples == 0 {
                first_ms = t_req.elapsed().as_secs_f64() * 1000.0;
                // Output sink: a fresh per-request rodio `Player` on the persistent mixer,
                // shared via `cur_player` for barge — OR the duplex render queue when the
                // backend owns render (macOS VPIO), barged via `duplex_barge`.
                player = match &device {
                    Some(dev) => {
                        let p = Arc::new(rodio::Player::connect_new(dev.mixer()));
                        p.set_volume(if muted.load(Ordering::SeqCst) {
                            0.0
                        } else {
                            1.0
                        });
                        *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
                        Some(p)
                    }
                    None => None,
                };
                // Prepend a brief silence only after the first batch commits. It absorbs an
                // idle rodio stream's resume latency without making a failed transaction touch
                // the player.
                if let Some(p) = &player {
                    p.append(rodio::buffer::SamplesBuffer::new(
                        channels,
                        srate,
                        leading_silence_pcm(srate.get()),
                    ));
                }
            }
            total_samples = total_samples.saturating_add(audio.total_samples);
            if render_via_duplex {
                if let Some(dx) = &duplex {
                    let _ = push_duplex_pcm(
                        audio.pcm,
                        || DuplexRenderState {
                            cancelled: cancel.load(Ordering::SeqCst),
                            muted: muted.load(Ordering::SeqCst),
                            buffered: dx.render_buffered(),
                        },
                        |pcm| dx.render_push(pcm),
                        || std::thread::sleep(DUPLEX_RENDER_POLL),
                    );
                }
            } else if let Some(p) = &player
                && !cancel.load(Ordering::SeqCst)
            {
                p.append(rodio::buffer::SamplesBuffer::new(
                    channels, srate, audio.pcm,
                ));
            }
            Ok(())
        };
        let outcome = match synth {
            Backend::Ort(synth) => prepare_audio(
                &phoneme_batches,
                || cancel.load(Ordering::SeqCst),
                |batch| synth.synthesize(batch.as_str(), &voice, rate),
                &mut commit,
            ),
            #[cfg(target_os = "macos")]
            Backend::Coreml(c) => prepare_audio(
                &phoneme_batches,
                || cancel.load(Ordering::SeqCst),
                |batch| c.synthesize_phonemes(batch.as_str(), &voice, rate),
                &mut commit,
            ),
        };
        match outcome {
            Ok(PrepareOutcome::Finished) => {}
            Ok(PrepareOutcome::Cancelled) => {
                // Committed batches were already stopped by the reader's barge (`cur_player` /
                // `duplex_barge`); clear defensively for the between-batches window and reset.
                if render_via_duplex {
                    if let Some(dx) = &duplex {
                        dx.render_clear();
                    }
                } else if let Some(p) = &player {
                    p.stop();
                }
                *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = None;
                println!("{}", proto::DONE);
                let _ = std::io::stdout().flush();
                continue;
            }
            Err(e) => {
                // A failed batch must not leave the already-committed prefix speaking under an
                // ERR reply: stop playback as soon as the failure is known.
                if render_via_duplex {
                    if let Some(dx) = &duplex {
                        dx.render_clear();
                    }
                } else if let Some(p) = &player {
                    p.stop();
                }
                *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = None;
                log::warn!(target: "helper", "batch synthesis failed: {e}");
                println!("{} {}", proto::ERR, one_line(&e));
                let _ = std::io::stdout().flush();
                continue;
            }
        }
        // Wait for playback to finish, then clear on barge.
        //  • rodio: sleep_until_end() (NOT an empty() poll — on WASAPI `empty()`
        //    reports true before the mixer consumed the freshly appended buffers,
        //    so the poll exited immediately and the Player was dropped before any
        //    sound played; the reader's stop() on cur_player makes it return on
        //    barge).
        //  • VPIO: poll render_pending() until the render ring drains or a barge
        //    sets `cancel` (the reader also drains the ring via duplex_barge).
        if !cancel.load(Ordering::SeqCst) {
            if render_via_duplex {
                // VPIO owns render: just wait for it to finish (or an explicit
                // `stop`/cancel). Dictation is TAP-driven and COEXISTS — the
                // concurrent listen thread owns the mic — so there is no implicit
                // talk-over barge here (stopping the voice is a long-press / `stop`).
                if let Some(dx) = &duplex {
                    while dx.render_pending() && !cancel.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(15));
                    }
                }
            } else if let Some(p) = &player {
                p.sleep_until_end();
            }
        }
        if cancel.load(Ordering::SeqCst) {
            if render_via_duplex {
                if let Some(dx) = &duplex {
                    dx.render_clear(); // barge: drop queued render audio
                }
            } else if let Some(p) = &player {
                p.stop(); // barge: drop anything still queued/playing
            }
        }
        *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // Stats BEFORE DONE (skip cancelled/empty utterances — they'd skew the RTF).
        if !cancel.load(Ordering::SeqCst) {
            let synth_ms = synth_nanos as f64 / 1e6;
            let audio_ms = total_samples as f64 / 24_000.0 * 1000.0;
            println!(
                "{}synth_ms={synth_ms:.1} audio_ms={audio_ms:.1} first_ms={first_ms:.1}",
                proto::STATS_PREFIX
            );
        }
        // Terminal DONE for a successful or cancelled request; failure paths above already
        // terminated with ERR instead (see ds-helper-proto's DONE/ERR contract).
        println!("{}", proto::DONE);
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod audio_tests {
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    use super::{
        DUPLEX_RENDER_AHEAD, DUPLEX_RENDER_CHUNK_SAMPLES, DuplexRenderState, LEAD_SILENCE_MS,
        leading_silence_pcm, push_duplex_pcm, tts_output_available,
    };

    #[test]
    fn tts_output_requires_preload_or_render_owning_duplex() {
        assert!(!tts_output_available(false, false));
        assert!(tts_output_available(true, false));
        assert!(tts_output_available(false, true));
    }

    /// Regression guard for the "first speak, no sound" fix: every utterance must be preceded by
    /// a NON-EMPTY, fully-SILENT leading buffer so the rodio output-stream resume is absorbed
    /// instead of clipping the speech onset. If someone drops the prepend or zeroes its duration,
    /// this fails.
    #[test]
    fn leading_silence_is_nonempty_and_pure_silence() {
        let pcm = leading_silence_pcm(24_000);
        // ~80 ms @ 24 kHz mono = 1920 samples — and NEVER empty (empty re-breaks the onset).
        assert_eq!(pcm.len(), 24_000 * LEAD_SILENCE_MS as usize / 1000);
        assert_eq!(pcm.len(), 1_920);
        assert!(
            !pcm.is_empty(),
            "leading silence must not regress to 0 samples"
        );
        // Pure silence — a non-zero lead would be an audible click before every reply.
        assert!(pcm.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn leading_silence_scales_with_sample_rate() {
        // Duration is fixed; sample count tracks the rate.
        assert_eq!(
            leading_silence_pcm(48_000).len(),
            48_000 * LEAD_SILENCE_MS as usize / 1000
        );
        // Compile-time invariant (not a runtime check on a constant): too little lead
        // won't cover the rodio output-stream resume latency.
        const _: () = assert!(LEAD_SILENCE_MS >= 40);
    }

    #[test]
    fn duplex_feeder_rechecks_mute_between_bounded_chunks() {
        let reads = Cell::new(0usize);
        let pushed = RefCell::new(Vec::<Vec<f32>>::new());
        let finished = push_duplex_pcm(
            vec![1.0; DUPLEX_RENDER_CHUNK_SAMPLES * 2 + 1],
            || {
                let read = reads.get();
                reads.set(read + 1);
                DuplexRenderState {
                    cancelled: false,
                    muted: read > 0,
                    buffered: Duration::ZERO,
                }
            },
            |pcm| pushed.borrow_mut().push(pcm.to_vec()),
            || panic!("an empty render ring must not wait"),
        );

        assert!(finished);
        let pushed = pushed.borrow();
        assert_eq!(pushed.len(), 3);
        assert!(pushed[0].iter().all(|sample| *sample == 1.0));
        assert!(pushed[1..].iter().flatten().all(|sample| *sample == 0.0));
        assert!(
            pushed
                .iter()
                .all(|chunk| chunk.len() <= DUPLEX_RENDER_CHUNK_SAMPLES)
        );
    }

    #[test]
    fn duplex_feeder_waits_at_the_watermark_and_cancels_during_wait() {
        let buffered = Cell::new(DUPLEX_RENDER_AHEAD);
        let waits = Cell::new(0usize);
        let pushed = Cell::new(0usize);
        let finished = push_duplex_pcm(
            vec![1.0],
            || DuplexRenderState {
                cancelled: false,
                muted: false,
                buffered: buffered.get(),
            },
            |_| pushed.set(pushed.get() + 1),
            || {
                waits.set(waits.get() + 1);
                buffered.set(Duration::ZERO);
            },
        );
        assert!(finished);
        assert_eq!(waits.get(), 1);
        assert_eq!(pushed.get(), 1);

        let cancelled = Cell::new(false);
        let pushed = Cell::new(false);
        let finished = push_duplex_pcm(
            vec![1.0],
            || DuplexRenderState {
                cancelled: cancelled.get(),
                muted: false,
                buffered: DUPLEX_RENDER_AHEAD,
            },
            |_| pushed.set(true),
            || cancelled.set(true),
        );
        assert!(!finished);
        assert!(!pushed.get());
    }
}

#[cfg(test)]
mod request_order_tests {
    use super::barge_then_publish;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};

    /// A queued job must not become visible until its barge is complete, or the playback loop
    /// can dequeue it, clear the cancel flag, and then have the late barge cancel the fresh job.
    #[test]
    fn barges_before_publishing_a_fresh_job() {
        let shared = (Mutex::new(false), Condvar::new());
        let cancelled = AtomicBool::new(false);

        barge_then_publish(
            &shared,
            || cancelled.store(true, Ordering::SeqCst),
            |published| {
                assert!(
                    cancelled.load(Ordering::SeqCst),
                    "the fresh job became visible before the prior job was cancelled"
                );
                *published = true;
            },
        );

        assert!(*shared.0.lock().unwrap());
    }
}

#[cfg(test)]
mod cue_gate_tests {
    use super::CueGate;

    #[test]
    fn mute_cancels_the_active_generation_without_resurrection() {
        let mut gate = CueGate::default();
        let (old, previous) = gate.begin().unwrap();
        assert_eq!(previous, None);
        assert!(gate.accepts_handle(old));

        assert_eq!(gate.set_muted(true), Some(old));
        assert!(!gate.accepts_handle(old));
        assert!(
            gate.begin().is_none(),
            "new cues are suppressed while muted"
        );

        assert_eq!(gate.set_muted(false), None);
        assert!(
            !gate.accepts_handle(old),
            "unmute must not revive the old cue"
        );
        let (later, previous) = gate.begin().unwrap();
        assert!(later > old);
        assert_eq!(previous, None);
        assert!(gate.accepts_handle(later));
    }

    #[test]
    fn explicit_cancel_only_removes_the_current_cue() {
        let mut gate = CueGate::default();
        let (first, _) = gate.begin().unwrap();
        assert_eq!(gate.cancel(), Some(first));
        assert!(!gate.accepts_handle(first));
        assert!(gate.begin().is_some(), "later cues remain playable");
    }
}

#[cfg(all(test, target_os = "macos"))]
mod mic_gate_tests {
    use super::wait_for_mic_free;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn returns_true_immediately_when_mic_already_free() {
        let listening = AtomicBool::new(false);
        let cancel = AtomicBool::new(false);
        let t0 = Instant::now();
        assert!(wait_for_mic_free(&listening, &cancel));
        assert!(
            t0.elapsed() < Duration::from_millis(20),
            "an already-free mic must not poll"
        );
    }

    /// A cancel that arrived BEFORE the wait even started must still be honored.
    #[test]
    fn returns_false_when_capture_cancel_was_already_set() {
        let listening = AtomicBool::new(false);
        let cancel = AtomicBool::new(true);
        assert!(!wait_for_mic_free(&listening, &cancel));
    }

    /// The real mutual-exclusion property, AND pins the 30ms poll interval: the flip is
    /// timed to land strictly after the first sleep, so the elapsed lower bound proves at
    /// least one real 30ms sleep happened (a regression to busy-spin or a longer interval
    /// would fail this).
    #[test]
    fn returns_true_once_full_duplex_listening_clears() {
        let listening = Arc::new(AtomicBool::new(true));
        let cancel = Arc::new(AtomicBool::new(false));
        let f = listening.clone();
        let flipper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(45));
            f.store(false, Ordering::SeqCst);
        });
        let t0 = Instant::now();
        assert!(wait_for_mic_free(&listening, &cancel));
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= Duration::from_millis(30),
            "must have polled at least once: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must not hang: {elapsed:?}"
        );
        flipper.join().unwrap();
    }

    /// `capture_cancel` firing WHILE waiting must unblock promptly, even though
    /// `full_duplex_listening` never clears.
    #[test]
    fn returns_false_promptly_when_capture_cancel_fires_while_waiting() {
        let listening = Arc::new(AtomicBool::new(true)); // never clears
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let flipper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(45));
            c.store(true, Ordering::SeqCst);
        });
        let t0 = Instant::now();
        assert!(!wait_for_mic_free(&listening, &cancel));
        assert!(t0.elapsed() < Duration::from_secs(2));
        flipper.join().unwrap();
    }
}
