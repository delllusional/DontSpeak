//! Warm-child loop: load once; NDJSON ops (`speak`/`listen`/`lstop`/`load`/…).

use ds_aec::DuplexAudio;
use ds_helper_proto as proto;
use ds_tts::g2p::{self, PhonemeBatchesOutcome};
use ds_tts::sink::IncrementalSink;
use serde::Deserialize;
use std::time::{Duration, Instant};

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

/// Flatten multi-line errors (ort `Display`) so protocol lines stay one-line terminals.
fn one_line(e: &str) -> String {
    e.lines().collect::<Vec<_>>().join(" ")
}
use crate::stt_residency::SttResidencySlot;

/// One stdin request (`--serve`, one JSON object per line).
#[derive(Debug, Deserialize)]
struct ServeReq {
    op: ds_helper_proto::HelperOp,
    #[serde(default)]
    voice: String,
    #[serde(default = "default_rate")]
    rate: f32,
    #[serde(default)]
    text: String,
    /// `unload`/`load` target.
    #[serde(default)]
    engine: Option<ds_helper_proto::HelperModel>,
    /// `diarize`/`enroll` capture length.
    #[serde(default)]
    seconds: Option<u64>,
    /// Daemon-owned `listen`/`lstop` generation (stop can beat a queued start).
    #[serde(default)]
    session: Option<u64>,
    /// Already-played frontend batches (engine echoes `PROGRESS`). Default 0; skew-safe
    /// both ways (no `deny_unknown_fields`).
    #[serde(default)]
    skip: usize,
}
fn default_rate() -> f32 {
    1.0
}

/// Fixed-window mic capture → 16 kHz mono for one-shot diarize/enroll.
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
    log::debug!(
        target: "helper",
        "capture: rate={rate} accum={} pcm16k={} secs={seconds}",
        accum.len(),
        pcm.len()
    );
    Ok(pcm)
}

/// Gate via [`ds_stt::diarize::ensure_coreml_backend`] (sole provider→backend mapping).
#[cfg(target_os = "macos")]
fn ensure_coreml_diarizer(cfg: &ds_config::VoiceConfig) -> Result<(), String> {
    ds_stt::diarize::ensure_coreml_backend(cfg.resolved_diarizer_provider())
}

/// One-shot diarize: record → cluster (config threshold). Emits `DIAR`/`DIARERR` + `DDONE`.
/// Engine does enrolled-name matching. Requires diarization on + Core ML-resolvable provider.
#[cfg(target_os = "macos")]
fn run_diarize(seconds: u64, cancel: &std::sync::atomic::AtomicBool) {
    use ds_stt::diarize::{CoremlDiarizer, Diarizer};
    use std::io::Write as _;

    let emit_err = |msg: &str| {
        println!("{}{}", proto::DIARERR_PREFIX, msg.replace('\n', " "));
        println!("{}", proto::DDONE);
        let _ = std::io::stdout().flush();
    };

    // Fresh config each call (mirrors capture_gain).
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

/// One-shot enroll: record → WeSpeaker embed → `EMB`/`ENROLLERR` + `EDONE`. Name stays in
/// the engine. Same gate as diarize so enroll can't fetch models while diarization is off.
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

// Helper never downloads: FluidAudio offlineMode; engine single-flight download manager
// owns fetch + warm-child restart. Absent files → load fails; engine restarts after fetch.

/// 100 ms source-rate chunks: cancel-responsive + fine-grained backpressure.
/// Mute is at VPIO render (`set_muted`), not here.
const DUPLEX_RENDER_CHUNK_SAMPLES: usize = ds_tts::SAMPLE_RATE as usize / 10;
/// VPIO lookahead (scheduler jitter, not a whole reply).
const DUPLEX_RENDER_AHEAD: Duration = Duration::from_secs(2);
const DUPLEX_RENDER_POLL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy)]
struct DuplexRenderState {
    cancelled: bool,
    buffered: Duration,
}

/// Pace a committed batch into VPIO; `false` if cancelled mid-batch.
fn push_duplex_pcm(
    pcm: &[f32],
    mut read_state: impl FnMut() -> DuplexRenderState,
    mut push: impl FnMut(&[f32]),
    mut wait: impl FnMut(),
) -> bool {
    for chunk in pcm.chunks(DUPLEX_RENDER_CHUNK_SAMPLES) {
        loop {
            let state = read_state();
            if state.cancelled {
                return false;
            }
            if state.buffered < DUPLEX_RENDER_AHEAD {
                break;
            }
            wait();
        }
        push(chunk);
    }
    true
}

/// Drain committed batches until sender closes (keeps consuming after cancel for fast join).
fn run_duplex_feeder(rx: std::sync::mpsc::Receiver<Vec<f32>>, mut push_batch: impl FnMut(&[f32])) {
    for pcm in rx {
        push_batch(&pcm);
    }
}

/// Remainder after `ServeReq::skip` (clamped; oversized skip → empty, never panic).
fn batches_after_skip<T>(batches: &[T], skip: usize) -> &[T] {
    &batches[skip.min(batches.len())..]
}

/// Full-duplex: wait until concurrent listen releases the mic (or `capture_cancel`).
/// Diarize/enroll open their own cpal stream; half-duplex serializes on one thread for free.
/// `false` if shutdown already set — skip open rather than race process exit.
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

    // STT preloads on its own thread in parallel with TTS. ORT_DYLIB_PATH write is Once-
    // serialized in ds-model. DONTSPEAK_STT_PROVIDER: system|ane|cuda|cpu. Shared Arc for preload +
    // concurrent-listen + request loop.
    let parakeet_dir = ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE)
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let stt_provider = std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default();
    let transcriber = Arc::new(Mutex::new(ds_stt::LocalTranscriber::for_provider(
        &stt_provider,
        parakeet_dir,
    )));
    // Offline `transcriber` cache for non-streaming; streaming uses `backend_cell` in
    // listen.rs. `unload stt` drives both. STTLOADED/STT_PROVIDER from whichever loaded.
    // Claim at load start so preload and `load stt` can't both load — see SttResidencySlot.
    let stt_claimed = Arc::new(SttResidencySlot::new());
    if std::env::var_os("DONTSPEAK_STT_PRELOAD").is_some() {
        // Claim before spawn so a racing `load stt` skips.
        stt_claimed.try_claim();
        let transcriber = transcriber.clone();
        let stt_provider = stt_provider.clone();
        let stt_claimed = stt_claimed.clone();
        std::thread::spawn(move || {
            // Engine already fetched files; preload is offline. STTLOADED = resident + warm.
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
                    // Release on failure so a later `load stt` can retry (pre-4ef3013: stuck true).
                    stt_claimed.mark_unloaded();
                    println!("{}{e}", proto::STTLOADERR_PREFIX);
                    let _ = std::io::stdout().flush();
                    log::warn!(target: "helper", "preload stt failed: {e}");
                }
            }
        });
    }

    // Engine fetched TTS assets; load_backend is offline. WARMING until READY (not premature green).
    if tts_wanted {
        println!("{}tts", proto::WARMING_PREFIX);
        let _ = std::io::stdout().flush();
    }
    // Option so unload tts frees Kokoro while STT stays warm; next speak reloads.
    let mut synth = if tts_wanted {
        match load_backend() {
            Ok(s) => {
                // PROVIDER before READY; READY only after audio open — green = warm AND can sound.
                println!("{}{}", proto::PROVIDER_PREFIX, s.provider().as_str());
                let _ = std::io::stdout().flush();
                Some(s)
            }
            Err(e) => {
                println!("{} {}", proto::ERR, one_line(&e));
                let _ = std::io::stdout().flush();
                // SAFETY: deliberate `_exit` teardown; see main.rs.
                unsafe { _exit(1) };
            }
        }
    } else {
        None
    };

    struct PlayReq {
        voice: String,
        rate: f32,
        text: String,
        /// Already-played batches (see `ServeReq::skip`).
        skip: usize,
    }
    struct State {
        req: Option<PlayReq>,
        /// Mutually exclusive with TTS (engine mic-barge).
        listen: Option<u64>,
        quit: bool,
        /// Free one model while the other stays warm.
        unload_tts: bool,
        unload_stt: bool,
        /// Eager residency so UI green matches before first use.
        load_tts: bool,
        load_stt: bool,
        /// One-shot; exclusive with TTS (single capture thread).
        diarize: Option<u64>,
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
    // Full-duplex AEC (DONTSPEAK_FULL_DUPLEX): one echo-cancelled unit for TTS render +
    // STT capture. Else half-duplex rodio+cpal. Coexist via concurrent-listen thread;
    // stop is explicit (`stop` / Caps long-press), not talk-over barge.
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
    // VPIO owns render (skip rodio); capture-only duplex still uses rodio for TTS.
    let render_via_duplex = duplex.as_ref().is_some_and(|d| d.owns_render());
    let tts_output_available = tts_output_available(tts_wanted, render_via_duplex);
    // Persistent device on this thread (cpal !Send). log_on_drop(false)+`_exit` avoid
    // macOS-26 CoreAudio teardown abort. Skipped when VPIO owns render.
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
                // SAFETY: deliberate `_exit` teardown; see main.rs.
                unsafe { _exit(1) };
            }
        }
    };
    // Send mixer clone for earcons; None under VPIO (macOS cue uses afplay).
    let cue_mixer = device.as_ref().map(|d| d.mixer().clone());
    let cue_playback = Arc::new(CuePlayback::default());
    // Warm + output open → READY. Resume latency (rodio pauses idle CoreAudio) is handled
    // per-utterance below — a brief leading silence absorbs the resume so the speech onset
    // isn't clipped (the "purple icon, no sound" first speak).
    println!("{}", proto::READY);
    let _ = std::io::stdout().flush();
    // A `Send` handle so the stdin reader can barge the VPIO render from its thread
    // (the unit itself is !Send and lives here on the playback thread).
    let duplex_barge: Option<std::sync::Arc<AtomicBool>> = duplex.as_ref().map(|d| d.barge_flag());
    // A `Send` render handle so the stdin reader's `mute` op can mute at RENDER time
    // (macOS VPIO owns render; the capture-side backends hand back a no-op handle).
    let duplex_render: Option<ds_aec::RenderHandle> = duplex.as_ref().map(|d| d.render_handle());
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
    // Full-duplex mic mutex: concurrent listen vs diarize/enroll cpal. Flags close
    // whole-session overlap (short tear-down window remains). Half-duplex no-ops.
    // cur_player: reader barge (non-blocking stop). cancel: stop/newer speak, checked mid-batch.
    let cur_player: Arc<Mutex<Option<Arc<rodio::Player>>>> = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    // Audible-stop instant for PROGRESS cap: min(now, stamp) so post-stop synth latency
    // doesn't over-skip unheard batch boundaries. Earliest stamp wins; ~60 ms fade under-skip OK.
    let cancel_stamp: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    // Diarize/enroll only: NOT tripped by TTS barge (pings/narration mid-record). EOF only.
    // Half-duplex listen: stop/lstop/EOF end it; a mid-listen speak must queue, not truncate.
    let capture_cancel = Arc::new(AtomicBool::new(false));
    // MUTE: speech drains silent; cues suppressed. Rodio instant; VPIO via set_muted.
    let muted = Arc::new(AtomicBool::new(false));

    // Reader: speak/preview enqueue (newest wins); stop cancels only (no DONE).
    {
        let shared = shared.clone();
        let cur_player = cur_player.clone();
        let cancel = cancel.clone();
        let cancel_stamp = cancel_stamp.clone();
        let capture_cancel = capture_cancel.clone();
        let listen_stopped_through = listen_stopped_through.clone();
        let listen_latest_generation = listen_latest_generation.clone();
        let duplex_barge = duplex_barge.clone();
        let duplex_render = duplex_render.clone();
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
                let voice = req.voice;
                let cancel_current = || {
                    // Instant barge: flag + rodio stop, or VPIO ring drain in full-duplex.
                    cancel.store(true, Ordering::SeqCst);
                    // Stamp just before stop for PROGRESS cap (see cancel_stamp).
                    cancel_stamp
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert_with(Instant::now);
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
                // User-facing stops: ~60 ms volume ramp (de-click, limit mic bleed).
                // Internal block preempt uses instant cancel_current (no gap). Full-duplex → VPIO drain.
                let cancel_current_fade = || {
                    cancel.store(true, Ordering::SeqCst);
                    // Stamp at fade start (later boundaries aren't "played").
                    cancel_stamp
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get_or_insert_with(Instant::now);
                    cue_playback.cancel();
                    // Clone Arc so ramp doesn't hold cur_player (playback loop also locks).
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
                match req.op {
                    proto::HelperOp::Stop => {
                        cancel_current(); // silent: no enqueue, no DONE
                        let generation = listen_latest_generation.load(Ordering::SeqCst);
                        listen_stopped_through.fetch_max(generation, Ordering::SeqCst);
                        if let Some(sig) = &listen_sig {
                            sig.1.notify_one();
                        }
                    }
                    proto::HelperOp::Mute => {
                        // Speech keeps draining silently. Cues are one-shot signals, so mute
                        // stops an active one and suppresses later ones rather than resurrecting
                        // them on unmute.
                        let on = matches!(
                            req.text.trim().to_ascii_lowercase().as_str(),
                            "on" | "true" | "1" | "yes"
                        );
                        muted.store(on, Ordering::SeqCst);
                        cue_playback.set_muted(on);
                        // Full-duplex renders through VPIO (no rodio volume): zero the
                        // output at render time, so buffered speech drains silently and
                        // unmute resumes real audio at the playhead instantly.
                        if let Some(r) = &duplex_render {
                            r.set_muted(on);
                        }
                        if let Some(p) = cur_player
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                        {
                            p.set_volume(if on { 0.0 } else { 1.0 });
                        }
                    }
                    proto::HelperOp::Stopfade => cancel_current_fade(), // graceful per-window barge
                    proto::HelperOp::Cue => {
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
                    proto::HelperOp::Speak => {
                        // Voice is required — no fallback voice exists; the engine always
                        // sends the caller's assigned pool voice.
                        if voice.trim().is_empty() {
                            use std::io::Write as _;
                            println!("{} voice id required", proto::ERR);
                            let _ = std::io::stdout().flush();
                            continue;
                        }
                        let text = req.text;
                        barge_then_publish(&shared, cancel_current, |state| {
                            state.req = Some(PlayReq {
                                voice,
                                rate: req.rate,
                                text,
                                skip: req.skip,
                            });
                        });
                    }
                    proto::HelperOp::Listen => {
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
                    proto::HelperOp::Lstop => {
                        let generation = req.session.unwrap_or(u64::MAX);
                        listen_stopped_through.fetch_max(generation, Ordering::SeqCst);
                        // End the listen WITHOUT touching the speak (coexist). In
                        // half-duplex it's the serve-loop listen, ended via listen_cancel
                        // (NOT the TTS `cancel`, so a queued speak isn't disturbed).
                        if let Some(sig) = &listen_sig {
                            sig.1.notify_one();
                        }
                    }
                    proto::HelperOp::Diarize => {
                        // One-shot record-then-diarize. Runs on the serve loop's single
                        // capture thread, so it's mutually exclusive with speak/listen —
                        // cancel any in-flight playback, then queue the job.
                        let secs = req.seconds.unwrap_or(10).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap_or_else(|e| e.into_inner()).diarize = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    proto::HelperOp::Enroll => {
                        // One-shot record-then-extract-voiceprint (same capture thread).
                        let secs = req.seconds.unwrap_or(15).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap_or_else(|e| e.into_inner()).enroll = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    proto::HelperOp::Unload => {
                        // Free a cached model the engine no longer needs while the
                        // OTHER engine keeps the helper warm. Idle-only (the playback
                        // loop runs it between jobs); no cancel.
                        let (m, cv) = &*shared;
                        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
                        match req.engine {
                            Some(proto::HelperModel::Tts) => s.unload_tts = true,
                            Some(proto::HelperModel::Stt) => s.unload_stt = true,
                            None => {}
                        }
                        cv.notify_one();
                    }
                    proto::HelperOp::Load => {
                        // Eagerly (pre)load a model so it's resident before first use.
                        let (m, cv) = &*shared;
                        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
                        match req.engine {
                            Some(proto::HelperModel::Tts) => s.load_tts = true,
                            Some(proto::HelperModel::Stt) => s.load_stt = true,
                            None => {}
                        }
                        cv.notify_one();
                    }
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
            cancel_stamp
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(Instant::now);
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
        // A fresh playback job clears playback barge state (the cancel flag AND the
        // audible-stop stamp — a stale stamp would cap the next request's resume mark
        // at a long-gone instant). Listen cancellation is generation-based and
        // deliberately never reset here: an early lstop must survive queueing and
        // prevent the matching capture from opening.
        cancel.store(false, Ordering::SeqCst);
        *cancel_stamp.lock().unwrap_or_else(|e| e.into_inner()) = None;

        // STT job: capture + stream partials + final, then back to waiting.
        let PlayReq {
            voice,
            rate,
            text,
            skip,
        } = match job {
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

        // Batch-granular resume: drop the batches an earlier run of this exact text
        // already played (the engine echoes our `PROGRESS` mark back as `skip`). Same
        // item + deterministic frontend ⇒ stable batch indices across runs. Applied
        // AFTER the frontend/empty/frontend-cancel checks and BEFORE the lazy synth
        // reload, so a fully-played remainder is a cheap no-op (no reload, no PROGRESS).
        let remainder = batches_after_skip(&phoneme_batches, skip);
        if remainder.is_empty() {
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
        let mut sink: Option<IncrementalSink> = None;
        let mut synth_nanos = 0u128;
        let mut total_samples = 0usize;
        let mut first_ms = 0.0f64;
        // Duplex render feeder: committing a batch must return immediately so the next
        // batch's inference overlaps pacing (otherwise the ~2 s lookahead wait would
        // serialize inference behind playback). The feeder thread owns the pacing loop;
        // `feed_abort` stops it on an ERR outcome, which does NOT set `cancel`.
        let feed_abort = Arc::new(AtomicBool::new(false));
        let (feed_tx, feeder) = if render_via_duplex && let Some(dx) = &duplex {
            let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
            let handle = dx.render_handle();
            let cancel = cancel.clone();
            let feed_abort = feed_abort.clone();
            match std::thread::Builder::new()
                .name("ds-duplex-feed".into())
                .spawn(move || {
                    run_duplex_feeder(rx, |pcm| {
                        let _ = push_duplex_pcm(
                            pcm,
                            || DuplexRenderState {
                                cancelled: cancel.load(Ordering::SeqCst)
                                    || feed_abort.load(Ordering::SeqCst),
                                buffered: handle.buffered(),
                            },
                            |chunk| handle.push(chunk),
                            || std::thread::sleep(DUPLEX_RENDER_POLL),
                        );
                    });
                }) {
                Ok(j) => (Some(tx), Some(j)),
                Err(e) => {
                    log::warn!(target: "helper", "duplex feeder spawn failed ({e}); pacing inline");
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
        let mut commit = |audio: PreparedAudio| -> Result<(), String> {
            synth_nanos = synth_nanos.saturating_add(audio.synth_nanos);
            if total_samples == 0 {
                first_ms = t_req.elapsed().as_secs_f64() * 1000.0;
                // Output sink: a fresh per-request incremental sink on the persistent
                // mixer, its player shared via `cur_player` for barge — OR the duplex
                // render queue when the backend owns render (macOS VPIO), barged via
                // `duplex_barge`. Mute POLICY stays here (the sink is transport only).
                if let Some(dev) = &device {
                    let s = IncrementalSink::connect_to(dev.mixer());
                    s.player().set_volume(if muted.load(Ordering::SeqCst) {
                        0.0
                    } else {
                        1.0
                    });
                    *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.player());
                    sink = Some(s);
                }
            }
            total_samples = total_samples.saturating_add(audio.pcm.len());
            if render_via_duplex {
                if let Some(tx) = &feed_tx {
                    // Send error = feeder died. If a barge landed, the barge owns the outcome
                    // (prepare's next cancelled() check returns Cancelled) — otherwise
                    // surface ERR.
                    if tx.send(audio.pcm).is_err() && !cancel.load(Ordering::SeqCst) {
                        return Err("duplex render feeder exited unexpectedly".to_string());
                    }
                } else if let Some(dx) = &duplex {
                    // Spawn-failure fallback: pace inline (commit blocks on the lookahead).
                    let _ = push_duplex_pcm(
                        &audio.pcm,
                        || DuplexRenderState {
                            cancelled: cancel.load(Ordering::SeqCst),
                            buffered: dx.render_buffered(),
                        },
                        |pcm| dx.render_push(pcm),
                        || std::thread::sleep(DUPLEX_RENDER_POLL),
                    );
                }
            } else if let Some(s) = &mut sink
                && !cancel.load(Ordering::SeqCst)
            {
                // The drained-sink re-lead (onset-clip fix) + played-batch accounting
                // live in `ds_tts::sink`. The duplex path needs no lead — the VPIO
                // render callback zero-fills any shortfall, so there is no resume
                // latency to absorb.
                s.append(audio.pcm);
            }
            Ok(())
        };
        let outcome = match synth {
            Backend::Ort(synth) => prepare_audio(
                remainder,
                || cancel.load(Ordering::SeqCst),
                |batch| synth.synthesize(batch.as_str(), &voice, rate),
                &mut commit,
            ),
            #[cfg(target_os = "macos")]
            Backend::Coreml(c) => prepare_audio(
                remainder,
                || cancel.load(Ordering::SeqCst),
                |batch| c.synthesize_phonemes(batch.as_str(), &voice, rate),
                &mut commit,
            ),
        };
        if outcome.is_err() {
            // ERR does NOT set `cancel`; without this abort the feeder would keep feeding
            // the committed prefix into the ring the Err arm is about to clear — audibly
            // playing it under an ERR reply.
            feed_abort.store(true, Ordering::SeqCst);
        }
        // Close the channel: on Finished the feeder pushes the remaining tail, then exits.
        drop(feed_tx);
        if let Some(j) = feeder {
            // After this join no push can race the render_clear/render_pending below.
            let _ = j.join();
        }
        // The resume mark for a CANCELLED request: batches PLAYED (not committed —
        // commits race ahead of the playhead), capped at the barge's audible-stop
        // stamp. See `cancel_stamp` for why the cap matters; ABSOLUTE = `skip` + this
        // run's played count, so the engine's high-water max stays monotone.
        let played_at_stop = |sink: &Option<IncrementalSink>| -> usize {
            let now = Instant::now();
            let capped = cancel_stamp
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .map_or(now, |stamp| stamp.min(now));
            skip + sink.as_ref().map_or(0, |s| s.played_batches(capped))
        };
        let emit_progress = |absolute_batches: usize| {
            println!("{}{absolute_batches}", proto::PROGRESS_PREFIX);
            let _ = std::io::stdout().flush();
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
                } else {
                    // Rodio path only: full-duplex never pauses/resumes, so it gets
                    // no resume mark. Emitted before the defensive stop.
                    emit_progress(played_at_stop(&sink));
                    if let Some(s) = &sink {
                        s.stop();
                    }
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
                } else if let Some(s) = &sink {
                    s.stop();
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
            } else if let Some(s) = &sink {
                s.wait();
            }
        }
        if cancel.load(Ordering::SeqCst) {
            if render_via_duplex {
                if let Some(dx) = &duplex {
                    dx.render_clear(); // barge: drop queued render audio
                }
            } else {
                // A barge landed during/after the playback wait: same capped resume
                // mark as the Cancelled arm, before the defensive stop.
                emit_progress(played_at_stop(&sink));
                if let Some(s) = &sink {
                    s.stop(); // barge: drop anything still queued/playing
                }
            }
        }
        *cur_player.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // Stats BEFORE DONE (skip cancelled/empty utterances — they'd skew the RTF).
        if !cancel.load(Ordering::SeqCst) {
            if !render_via_duplex {
                // Finished uncancelled: the whole remainder played — publish the
                // absolute high-water mark so a LATER barge of a requeued item can't
                // fall back to the top.
                emit_progress(skip + remainder.len());
            }
            // STATS covers the RESUMED TAIL only: synth_ms/audio_ms/first_ms account
            // solely for the post-`skip` batches this request actually synthesized.
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
        DUPLEX_RENDER_AHEAD, DUPLEX_RENDER_CHUNK_SAMPLES, DuplexRenderState, push_duplex_pcm,
        run_duplex_feeder, tts_output_available,
    };

    // The lead-silence + AppendClock (drain detection) tests moved WITH the code to
    // `ds_tts::sink` — this module keeps only the duplex feeder/pacing coverage.

    #[test]
    fn tts_output_requires_preload_or_render_owning_duplex() {
        assert!(!tts_output_available(false, false));
        assert!(tts_output_available(true, false));
        assert!(tts_output_available(false, true));
    }

    /// Chunked pacing pins CANCEL responsiveness: the state is re-read per chunk, so
    /// a cancel landing mid-batch stops the remaining chunks — and every push stays
    /// chunk-bounded, keeping the live `buffered` re-reads fine-grained. (Mute is no
    /// longer applied here: the VPIO render callback zero-fills at render time.)
    #[test]
    fn duplex_feeder_rechecks_cancel_between_bounded_chunks() {
        let reads = Cell::new(0usize);
        let pushed = RefCell::new(Vec::<Vec<f32>>::new());
        let finished = push_duplex_pcm(
            &[1.0; DUPLEX_RENDER_CHUNK_SAMPLES * 2 + 1],
            || {
                let read = reads.get();
                reads.set(read + 1);
                DuplexRenderState {
                    cancelled: read > 0,
                    buffered: Duration::ZERO,
                }
            },
            |pcm| pushed.borrow_mut().push(pcm.to_vec()),
            || panic!("an empty render ring must not wait"),
        );

        assert!(!finished, "a mid-batch cancel must report unfinished");
        let pushed = pushed.borrow();
        assert_eq!(pushed.len(), 1, "no chunk is pushed after the cancel");
        assert!(pushed[0].iter().all(|sample| *sample == 1.0));
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
            &[1.0],
            || DuplexRenderState {
                cancelled: false,
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
            &[1.0],
            || DuplexRenderState {
                cancelled: cancelled.get(),
                buffered: DUPLEX_RENDER_AHEAD,
            },
            |_| pushed.set(true),
            || cancelled.set(true),
        );
        assert!(!finished);
        assert!(!pushed.get());
    }

    /// Pins "join can't hang": the feeder must deliver every committed batch in order and
    /// terminate on its own the moment the sender side drops.
    #[test]
    fn feeder_delivers_batches_in_order_and_exits_when_sender_closes() {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        tx.send(vec![1.0]).unwrap();
        tx.send(vec![2.0, 2.0]).unwrap();
        tx.send(vec![3.0]).unwrap();
        drop(tx);

        let delivered = RefCell::new(Vec::<Vec<f32>>::new());
        run_duplex_feeder(rx, |pcm| delivered.borrow_mut().push(pcm.to_vec()));

        // Reaching this line at all proves termination on channel close.
        assert_eq!(
            *delivered.borrow(),
            vec![vec![1.0], vec![2.0, 2.0], vec![3.0]]
        );
    }

    /// Pins "join is fast after barge": a batch the push rejects (push_duplex_pcm returning
    /// false on cancel) must not stop the consume loop — the remaining queued batches are
    /// still offered (and cheaply rejected) so the channel empties and join returns.
    #[test]
    fn feeder_keeps_consuming_after_a_rejected_batch() {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<f32>>();
        for i in 0..4 {
            tx.send(vec![i as f32]).unwrap();
        }
        drop(tx);

        let offered = Cell::new(0usize);
        run_duplex_feeder(rx, |_| {
            // Simulate a cancel landing after the first batch: every later push is a
            // rejection, and the feeder must keep draining regardless.
            offered.set(offered.get() + 1);
        });
        assert_eq!(offered.get(), 4, "every queued batch must still be offered");
    }
}

#[cfg(test)]
mod skip_tests {
    use super::batches_after_skip;

    /// The batch-granular resume slice: 0 = the whole request, a mid value drops the
    /// played prefix, and an at/over-length skip (voice/rate change shifted batch
    /// counts between runs) CLAMPS to an empty remainder instead of panicking.
    #[test]
    fn batches_after_skip_slices_and_clamps() {
        let batches = ["a", "b", "c"];
        assert_eq!(batches_after_skip(&batches, 0), &["a", "b", "c"]);
        assert_eq!(batches_after_skip(&batches, 2), &["c"]);
        assert!(batches_after_skip(&batches, 3).is_empty());
        assert!(batches_after_skip(&batches, usize::MAX).is_empty());
        assert!(batches_after_skip::<&str>(&[], 1).is_empty());
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
