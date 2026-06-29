//! `--serve` warm-child loop: load the model ONCE, then read JSON requests on
//! stdin (one object per line) and synth+play / listen / (un)load each. Owns the
//! `State`/`Job` machine and the op dispatch (`listen`/`lstop`/`load`/`unload`/
//! `speak`/etc.).

use ds_aec::DuplexAudio;
use ds_tts::batch::{chunk_text, stream_batches};
use ds_tts::g2p;
use serde::Deserialize;

use crate::_exit;
use crate::listen::{ListenSig, concurrent_listen_loop, run_listen};
use crate::oneshot::{Backend, load_backend};

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
    eprintln!(
        "capture: rate={rate} accum={} pcm16k={} secs={seconds}",
        accum.len(),
        pcm.len()
    );
    Ok(pcm)
}

/// The Core ML / ANE backend is the only diarizer wired today. Returns `Err` (a
/// user-facing message) when the config selects a runtime that resolves to anything
/// else — e.g. `onnx`, or any provider off macOS — so `diarizer_provider` is honored
/// instead of silently falling through to Core ML. `Ok` ⇒ Core ML is the right backend.
#[cfg(target_os = "macos")]
fn ensure_coreml_diarizer(cfg: &ds_config::VoiceConfig) -> Result<(), String> {
    use ds_config::DiarizerProvider;
    match cfg.resolved_diarizer() {
        DiarizerProvider::AppleNative => Ok(()),
        other => Err(format!(
            "diarizer_provider={} is not available on this platform (only apple-native is wired)",
            other.as_str()
        )),
    }
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
        println!("DIARERR {}", msg.replace('\n', " "));
        println!("DDONE");
        let _ = std::io::stdout().flush();
    };

    // Read config fresh (mirrors capture_gain); gate + threshold come from it.
    let cfg = ds_config::Paths::resolve().map(|p| ds_config::VoiceConfig::load(&p));
    let Some(cfg) = cfg else {
        return emit_err("config unavailable");
    };
    if !cfg.diarization_on() {
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
            println!("DIAR {json}");
            println!("DDONE");
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
        println!("ENROLLERR {}", msg.replace('\n', " "));
        println!("EDONE");
        let _ = std::io::stdout().flush();
    };

    let cfg = ds_config::Paths::resolve().map(|p| ds_config::VoiceConfig::load(&p));
    let Some(cfg) = cfg else {
        return emit_err("config unavailable");
    };
    if !cfg.diarization_on() {
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
            println!("EMB {json}");
            println!("EDONE");
            let _ = std::io::stdout().flush();
        }
        Err(e) => emit_err(&e),
    }
}

// The warm synth/STT server is cross-platform: the body is rodio + ds_stt +
// ds_model (all portable) and `_exit` is an extern-C symbol present on every libc.
// Audio that can't open degrades via the `ERR audio` path below — there is no
// platform that needs a blanket "unsupported" stub.
/// Apple-native ONLY: download the Core ML repos for `kind` ("tts" / "stt") OURSELVES — so the
/// engine shows a real % and FluidAudio (which now runs offline, `enforceOffline`) only LOADS.
/// Emits `DOWNLOADING <kind>` immediately (dot flips), then `DOWNLOADING <kind> <fd> <ft> <idx>
/// <cnt>` per file as bytes arrive. No-op without the ANE shim (the ONNX path is fetched
/// engine-side) or when every repo is already present. Returns false on a download error (the
/// caller's load then surfaces the real failure).
fn ensure_coreml_models(kind: &str) -> bool {
    use std::io::Write;
    if std::env::var_os("SMKOKORO_DYLIB_PATH").is_none() {
        return true; // no ANE shim → ONNX path; nothing for us to fetch here
    }
    use ds_model::coreml_repo::{
        CoremlRepo, KOKORO_COREML, KOKORO_G2P_COREML, PARAKEET_COREML, coreml_repo_present,
        ensure_coreml_repos,
    };
    let repos: &[&CoremlRepo] = match kind {
        "tts" => &[&KOKORO_COREML, &KOKORO_G2P_COREML],
        "stt" => &[&PARAKEET_COREML],
        _ => return true,
    };
    if repos.iter().all(|r| coreml_repo_present(r)) {
        return true;
    }
    // Flip the dot to "downloading" before the first byte count lands.
    println!("DOWNLOADING {kind}");
    let _ = std::io::stdout().flush();
    eprintln!("dontspeak/helper: fetching {kind} Core ML model(s) before warm");
    // Per-file: `<file_done> <file_total> <file_index> <file_count>` → the dot shows
    // "<index>/<count> · <this file's %>".
    let prog = |fd: u64, ft: u64, idx: u64, cnt: u64| {
        println!("DOWNLOADING {kind} {fd} {ft} {idx} {cnt}");
        let _ = std::io::stdout().flush();
    };
    match ensure_coreml_repos(repos, &prog) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("dontspeak/helper: {kind} Core ML download failed: {e}");
            false
        }
    }
}

/// Leading silence prepended to EACH utterance's rodio sink, so the output-stream RESUME
/// latency (rodio pauses the CoreAudio output when idle) is absorbed by the silence instead of
/// clipping the speech onset — the "first speak, purple icon, no sound" fix. Pure + unit-tested
/// so it can't silently regress to 0 samples (which would re-break the onset).
const LEAD_SILENCE_MS: u32 = 80;

/// `LEAD_SILENCE_MS` of mono silence at `srate_hz`. See [`LEAD_SILENCE_MS`].
fn leading_silence_pcm(srate_hz: u32) -> Vec<f32> {
    vec![0.0f32; srate_hz as usize * LEAD_SILENCE_MS as usize / 1000]
}

pub(crate) fn serve() -> ! {
    use std::io::{BufRead, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Condvar, Mutex};

    // ── STT (Parakeet) preloads in PARALLEL with the TTS load below ──────────────────────
    // Construct the transcriber first (cheap — no model load yet) and, when STT is wanted
    // (DONTSPEAK_STT_PRELOAD, set by the engine only for the built-in engine), preload it on its
    // OWN thread. So STT and TTS download/warm INDEPENDENTLY — each reports its own lifecycle
    // and neither blocks the other. The ONNX bootstrap's `ORT_DYLIB_PATH` write is serialized
    // by a Once in the model layer, so the two parallel loads don't race the env.
    // DONTSPEAK_STT_PROVIDER picks the local backend: "ane" → FluidAudio Core ML / ANE,
    // "ort_cpu" → portable ONNX Parakeet. Shared (Arc<Mutex>) so the preload thread, the
    // full-duplex concurrent-listen thread, and the request loop all reach it.
    let parakeet_dir = ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE)
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_default();
    let stt_provider = std::env::var("DONTSPEAK_STT_PROVIDER").unwrap_or_default();
    let transcriber = Arc::new(Mutex::new(ds_stt::LocalTranscriber::for_provider(
        &stt_provider,
        parakeet_dir,
    )));
    // Set the MOMENT the STT load is CLAIMED — by the parallel preload below OR a later
    // `load stt` request — so the two can't BOTH download the model concurrently (a double
    // fetch downloaded the same files twice and made the % bounce up and down).
    let stt_claimed = Arc::new(AtomicBool::new(false));
    if std::env::var_os("DONTSPEAK_STT_PRELOAD").is_some() {
        // Claim BEFORE spawning so a `load stt` that races in skips its own download.
        stt_claimed.store(true, Ordering::Relaxed);
        let transcriber = transcriber.clone();
        std::thread::spawn(move || {
            // Fetch the Parakeet Core ML model OURSELVES first (apple-native), reporting % via
            // DOWNLOADING; then preload() loads it offline. ONNX path is a no-op here.
            ensure_coreml_models("stt");
            // Download done → loading + warming. "Starting…" until STTLOADED (preload runs a
            // warmup inference, so STTLOADED honestly means resident + warm).
            println!("WARMING stt");
            let _ = std::io::stdout().flush();
            match transcriber.lock().unwrap().preload() {
                Ok(()) => {
                    println!("STTLOADED");
                    let _ = std::io::stdout().flush();
                }
                Err(e) => eprintln!("dontspeak/helper: preload stt failed: {e}"),
            }
        });
    }

    // Fetch the Kokoro Core ML model + shared G2P OURSELVES (apple-native) before the warm
    // load below, reporting % via DOWNLOADING; load_backend then loads them offline. The dot
    // reads "downloading" until READY. ONNX path is a no-op here (fetched engine-side).
    ensure_coreml_models("tts");
    // Download done (if any) → now LOADING + warming the synth. Tell the engine so the dot
    // reads "Starting…" through the load + warmup (which can be slow on the first ANE compile),
    // instead of a stuck "Downloading" or a premature green, until READY below.
    println!("WARMING tts");
    let _ = std::io::stdout().flush();
    // Load once. READY/ERR let the UI know the model is warm.
    // Held as Option so a `unload tts` can free the Kokoro model while the helper
    // stays warm for STT; the next speak lazily reloads it (below).
    let mut synth = match load_backend() {
        Ok(s) => {
            // PROVIDER (before READY) lets the engine report the active execution provider.
            // READY is emitted LATER — only after the audio OUTPUT is opened + primed below —
            // so green honestly means "warm AND able to make sound", not just "model loaded".
            println!("PROVIDER {}", s.provider());
            let _ = std::io::stdout().flush();
            Some(s)
        }
        Err(e) => {
            println!("ERR {e}");
            let _ = std::io::stdout().flush();
            unsafe { _exit(1) };
        }
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
        listen: bool,
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
            listen: false,
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
                eprintln!(
                    "dontspeak/helper: full-duplex AEC active ({} Hz capture)",
                    d.capture_rate()
                );
                Some(d)
            }
            Err(e) => {
                eprintln!("dontspeak/helper: full-duplex unavailable ({e}); half-duplex");
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
    // One persistent audio device (the cpal stream is !Send → it must stay on THIS
    // playback thread). `log_on_drop(false)` + `_exit` on quit avoid the macOS-26
    // CoreAudio teardown abort. Per-request `Player`s are created on its mixer.
    // Skipped only when the duplex backend owns render (macOS VPIO); a capture-only
    // duplex keeps rodio for output.
    let device = if render_via_duplex {
        None
    } else {
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(mut d) => {
                d.log_on_drop(false);
                Some(d)
            }
            Err(e) => {
                println!("ERR audio: {e}");
                let _ = std::io::stdout().flush();
                unsafe { _exit(1) };
            }
        }
    };
    // An OWNED, `Send` clone of the device mixer (it's an `Arc` handle) for the reader thread
    // to play one-shot EARCONS on — mixed alongside any in-flight TTS by the OS. `None` when
    // the duplex backend owns render (macOS VPIO, no rodio mixer); the cue then falls back to
    // `afplay` on macOS (see the `cue` op below).
    let cue_mixer = device.as_ref().map(|d| d.mixer().clone());
    // The model is loaded + warm and the output device is open → signal READY (green). The
    // audio-stream RESUME latency (rodio pauses the CoreAudio output when idle) is handled
    // per-utterance below — a brief leading silence absorbs the resume so the speech onset
    // isn't clipped (the "purple icon, no sound" first speak).
    println!("READY");
    let _ = std::io::stdout().flush();
    // A `Send` handle so the stdin reader can barge the VPIO render from its thread
    // (the unit itself is !Send and lives here on the playback thread).
    let duplex_barge: Option<std::sync::Arc<AtomicBool>> = duplex.as_ref().map(|d| d.barge_flag());
    // Full-duplex COEXIST: spawn the concurrent listen thread (drains the
    // echo-cancelled mic + transcribes while this thread renders TTS). `listen_sig`
    // is the reader→thread control (Some only in full-duplex).
    let listen_sig: Option<Arc<(Mutex<ListenSig>, Condvar)>> = duplex.as_ref().map(|dx| {
        let sig = Arc::new((Mutex::new(ListenSig::default()), Condvar::new()));
        let capture = dx.capture_handle();
        let tr = transcriber.clone();
        let sig2 = sig.clone();
        std::thread::Builder::new()
            .name("ds-listen".into())
            .spawn(move || concurrent_listen_loop(capture, tr, sig2))
            .ok();
        sig
    });
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
    let listen_cancel = Arc::new(AtomicBool::new(false));
    // MUTE: silence output WITHOUT stopping — the queue/playback still drains (timing
    // preserved), only the audio is zeroed. Toggled by the `mute` op (Caps-tap when dictation
    // is off, or the tray checkbox). Read by the playback `append` below (zeroes the PCM) and
    // applied instantly to the sounding rodio player on toggle.
    let muted = Arc::new(AtomicBool::new(false));

    // Reader thread: parse JSON requests. speak/preview enqueue (newest wins) and
    // cancel any current playback; stop only cancels (no enqueue, no DONE).
    {
        let shared = shared.clone();
        let cur_player = cur_player.clone();
        let cancel = cancel.clone();
        let capture_cancel = capture_cancel.clone();
        let listen_cancel = listen_cancel.clone();
        let duplex_barge = duplex_barge.clone();
        let listen_sig = listen_sig.clone();
        let muted = muted.clone();
        let cue_mixer = cue_mixer.clone();
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
                    "af_sarah".to_string()
                } else {
                    req.voice
                };
                let cancel_current = || {
                    // Signal the playback loop to stop the in-flight request, then
                    // stop the player sounding right now (non-blocking flag). In
                    // full-duplex mode there is no rodio player — drain the VPIO
                    // render ring via its barge flag instead.
                    cancel.store(true, Ordering::SeqCst);
                    if let Some(p) = cur_player.lock().unwrap().as_ref() {
                        p.stop();
                    }
                    if let Some(f) = &duplex_barge {
                        f.store(true, Ordering::SeqCst);
                    }
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
                    // Clone the Arc out so the ramp does NOT hold the `cur_player` lock
                    // (the playback loop touches it too).
                    let player = cur_player.lock().unwrap().as_ref().cloned();
                    if let Some(p) = player {
                        const STEPS: u32 = 12;
                        let start = p.volume();
                        let step = std::time::Duration::from_millis(60) / STEPS;
                        for i in 1..=STEPS {
                            p.set_volume((start * (1.0 - i as f32 / STEPS as f32)).max(0.0));
                            std::thread::sleep(step);
                        }
                        p.stop();
                    }
                    if let Some(f) = &duplex_barge {
                        f.store(true, Ordering::SeqCst);
                    }
                };
                match req.op.as_str() {
                    "stop" => {
                        cancel_current(); // silent: no enqueue, no DONE
                        listen_cancel.store(true, Ordering::SeqCst); // also ends a half-duplex listen
                    }
                    "mute" => {
                        // Toggle global mute (text "on"/"off"). Does NOT cancel — playback keeps
                        // draining; the audio is just silenced. Apply instantly to the sounding
                        // rodio player so already-queued audio goes quiet immediately; new audio
                        // is zeroed by `append` below.
                        let on = req.text.trim() == "on";
                        muted.store(on, Ordering::SeqCst);
                        if let Some(p) = cur_player.lock().unwrap().as_ref() {
                            p.set_volume(if on { 0.0 } else { 1.0 });
                        }
                    }
                    "stopfade" => cancel_current_fade(), // graceful per-window barge (fade then stop)
                    "cue" => {
                        // One-shot EARCON (turn-done ding / needs-input cue): decode + play the
                        // resolved sound file on the SAME rodio mixer that renders TTS, so the OS
                        // mixes it over any in-flight speech. Does NOT cancel and does NOT emit a
                        // DONE — it rides alongside the queue. Fire-and-forget on its own thread;
                        // fail-quiet. In macOS VPIO full-duplex there is no rodio mixer (the duplex
                        // owns render), so fall back to `afplay`. Skipped while muted (the engine
                        // also gates on mute, but a toggle can race the send).
                        let path = req.text.clone();
                        if muted.load(Ordering::SeqCst) {
                            // muted → no cue
                        } else if let Some(mixer) = cue_mixer.clone() {
                            std::thread::spawn(move || {
                                let Ok(file) = std::fs::File::open(&path) else {
                                    return;
                                };
                                let Ok(decoder) =
                                    rodio::Decoder::new(std::io::BufReader::new(file))
                                else {
                                    return;
                                };
                                let player = rodio::Player::connect_new(&mixer);
                                player.append(decoder);
                                player.sleep_until_end();
                            });
                        } else {
                            #[cfg(target_os = "macos")]
                            {
                                let _ = std::process::Command::new("afplay").arg(&path).spawn();
                            }
                        }
                    }
                    "speak" => {
                        let text = req.text;
                        {
                            let (m, cv) = &*shared;
                            m.lock().unwrap().req = Some(PlayReq {
                                voice,
                                rate: req.rate,
                                text,
                            });
                            cv.notify_one();
                        }
                        cancel_current(); // newest request wins
                    }
                    "listen" => {
                        if let Some(sig) = &listen_sig {
                            // Full-duplex COEXIST: wake the concurrent listen thread;
                            // do NOT cancel an in-flight speak.
                            let (m, cv) = &**sig;
                            let mut s = m.lock().unwrap();
                            s.start = true;
                            s.stop = false;
                            cv.notify_one();
                        } else {
                            // Half-duplex: serve-loop listen, mutually exclusive w/ speak.
                            let (m, cv) = &*shared;
                            m.lock().unwrap().listen = true;
                            cv.notify_one();
                            cancel_current();
                        }
                    }
                    "lstop" => {
                        // End the listen WITHOUT touching the speak (coexist). In
                        // half-duplex it's the serve-loop listen, ended via listen_cancel
                        // (NOT the TTS `cancel`, so a queued speak isn't disturbed).
                        if let Some(sig) = &listen_sig {
                            let (m, cv) = &**sig;
                            m.lock().unwrap().stop = true;
                            cv.notify_one();
                        } else {
                            listen_cancel.store(true, Ordering::SeqCst);
                        }
                    }
                    "diarize" => {
                        // One-shot record-then-diarize. Runs on the serve loop's single
                        // capture thread, so it's mutually exclusive with speak/listen —
                        // cancel any in-flight playback, then queue the job.
                        let secs = req.seconds.unwrap_or(10).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap().diarize = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    "enroll" => {
                        // One-shot record-then-extract-voiceprint (same capture thread).
                        let secs = req.seconds.unwrap_or(15).clamp(1, 60);
                        let (m, cv) = &*shared;
                        m.lock().unwrap().enroll = Some(secs);
                        cv.notify_one();
                        cancel_current();
                    }
                    "unload" => {
                        // Free a cached model the engine no longer needs while the
                        // OTHER engine keeps the helper warm. Idle-only (the playback
                        // loop runs it between jobs); no cancel.
                        let (m, cv) = &*shared;
                        let mut s = m.lock().unwrap();
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
                        let mut s = m.lock().unwrap();
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
            // End an in-flight diarize/enroll capture AND a half-duplex listen too (both
            // ignore the TTS `cancel`).
            capture_cancel.store(true, Ordering::SeqCst);
            listen_cancel.store(true, Ordering::SeqCst);
            if let Some(p) = cur_player.lock().unwrap().as_ref() {
                p.stop();
            }
            if let Some(f) = &duplex_barge {
                f.store(true, Ordering::SeqCst);
            }
            if let Some(sig) = &listen_sig {
                let (m, cv) = &**sig;
                let mut ls = m.lock().unwrap();
                ls.quit = true;
                ls.stop = true;
                cv.notify_one();
            }
            let (m, cv) = &*shared;
            let mut s = m.lock().unwrap();
            s.req = None; // do NOT drain a pending request on quit
            s.listen = false;
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
            Listen,
            Diarize(u64),
            Enroll(u64),
            UnloadTts,
            UnloadStt,
            LoadTts,
            LoadStt,
        }
        let job = {
            let (m, cv) = &*shared;
            let mut s = m.lock().unwrap();
            while s.req.is_none()
                && !s.listen
                && s.diarize.is_none()
                && s.enroll.is_none()
                && !s.quit
                && !s.unload_tts
                && !s.unload_stt
                && !s.load_tts
                && !s.load_stt
            {
                s = cv.wait(s).unwrap();
            }
            // Drain a pending job even if `quit` also arrived; exit only when idle+quit.
            if let Some(r) = s.req.take() {
                Job::Speak(r)
            } else if s.listen {
                s.listen = false;
                Job::Listen
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
                unsafe { _exit(0) };
            }
        };
        // Fresh job: clear any cancel left by the request that triggered it (both the TTS
        // barge `cancel` and the dictation `listen_cancel`, so a stale stop can't end the
        // listen we're about to start).
        cancel.store(false, Ordering::SeqCst);
        listen_cancel.store(false, Ordering::SeqCst);

        // STT job: capture + stream partials + final, then back to waiting.
        let PlayReq { voice, rate, text } = match job {
            Job::Speak(r) => r,
            Job::Listen => {
                // Half-duplex only (full-duplex routes to the concurrent thread, so
                // `duplex` is always None here — its old VPIO capture path is gone).
                // Uses `listen_cancel` so a TTS speak barge can't truncate the dictation;
                // only stop/lstop (the seconds-timer / Caps release) and EOF end it.
                run_listen(&mut transcriber.lock().unwrap(), &listen_cancel);
                continue;
            }
            Job::Diarize(secs) => {
                // One-shot: record `secs` of mic, then diarize. macOS-only (FluidAudio
                // Core ML); off macOS the cross-platform ONNX backend isn't wired yet.
                // Uses `capture_cancel` so a TTS barge can't abort the recording.
                #[cfg(target_os = "macos")]
                run_diarize(secs, &capture_cancel);
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = secs;
                    use std::io::Write as _;
                    println!("DIARERR diarization is only available on macOS");
                    println!("DDONE");
                    let _ = std::io::stdout().flush();
                }
                continue;
            }
            Job::Enroll(secs) => {
                // One-shot: record `secs` of mic, then extract a voiceprint. macOS-only.
                // Uses `capture_cancel` so a TTS barge can't abort the recording.
                #[cfg(target_os = "macos")]
                run_enroll(secs, &capture_cancel);
                #[cfg(not(target_os = "macos"))]
                {
                    let _ = secs;
                    use std::io::Write as _;
                    println!("ENROLLERR enrollment is only available on macOS");
                    println!("EDONE");
                    let _ = std::io::stdout().flush();
                }
                continue;
            }
            Job::UnloadTts => {
                // Drop the cached Kokoro model; the next speak lazily reloads it.
                let freed = synth.take().is_some();
                eprintln!("dontspeak/helper: unloaded tts (kokoro), freed={freed}");
                continue;
            }
            Job::UnloadStt => {
                let freed = transcriber.lock().unwrap().unload();
                eprintln!("dontspeak/helper: unloaded stt (parakeet), freed={freed}");
                continue;
            }
            Job::LoadTts => {
                if synth.is_none() {
                    match load_backend() {
                        Ok(s) => synth = Some(s),
                        Err(e) => eprintln!("dontspeak/helper: preload tts failed: {e}"),
                    }
                }
                continue;
            }
            Job::LoadStt => {
                // STT is normally preloaded in PARALLEL at startup (the thread in `serve()`), so
                // this engine-sent `load stt` is usually redundant. SINGLE-FLIGHT it: if the
                // load is already claimed (by that preload, or a prior `load stt`), skip — else
                // claim it and load HERE (STT became wanted after startup, or preload was off).
                if stt_claimed.swap(true, Ordering::Relaxed) {
                    continue;
                }
                ensure_coreml_models("stt");
                match transcriber.lock().unwrap().preload() {
                    Ok(()) => {
                        println!("STTLOADED");
                        let _ = std::io::stdout().flush();
                    }
                    Err(e) => eprintln!("dontspeak/helper: preload stt failed: {e}"),
                }
                continue;
            }
        };

        // Lazily (re)load the Kokoro synth if a prior `unload tts` freed it.
        if synth.is_none() {
            match load_backend() {
                Ok(s) => synth = Some(s),
                Err(e) => {
                    eprintln!("dontspeak/helper: synth reload failed: {e}");
                    println!("DONE");
                    let _ = std::io::stdout().flush();
                    continue;
                }
            }
        }
        let synth = synth.as_mut().expect("synth loaded above");

        // Output sink: a fresh per-request rodio `Player` on the persistent mixer,
        // shared via `cur_player` for barge — OR the duplex render queue when the
        // backend owns render (macOS VPIO), barged via `duplex_barge`. `device` is
        // Some exactly when rodio renders (half-duplex OR capture-only duplex), so we
        // create the player whenever a device exists.
        let player = match &device {
            Some(dev) => {
                let p = Arc::new(rodio::Player::connect_new(dev.mixer()));
                // Start at the CURRENT mute volume: if this reply began while muted, it plays
                // silently (volume 0) but the REAL samples are buffered, so unmuting mid-reply
                // (the `mute` op sets this player's volume to 1.0) restores the sound — mute
                // never destroys the audio, it only attenuates it.
                p.set_volume(if muted.load(Ordering::SeqCst) {
                    0.0
                } else {
                    1.0
                });
                *cur_player.lock().unwrap() = Some(p.clone());
                Some(p)
            }
            None => None,
        };
        let channels = std::num::NonZero::new(1u16).expect("1 channel");
        let srate = std::num::NonZero::new(24_000u32).expect("24 kHz");

        // Prepend a brief LEADING SILENCE to the rodio sink: when the output stream was idle
        // (rodio pauses it to save power), restarting it on the first buffer drops the leading
        // audio — the "purple icon, no sound" first utterance. ~80 ms of silence first absorbs
        // that resume so the speech onset is intact. Cheap, and inaudible. (VPIO duplex render
        // owns its own always-live unit, so it needs none.)
        if let Some(p) = &player {
            p.append(rodio::buffer::SamplesBuffer::new(
                channels,
                srate,
                leading_silence_pcm(srate.get()),
            ));
        }

        // GAPLESS streaming into ONE continuous sink. Both engines pack the reply through the
        // SAME text splitter — `batch::chunk_text` — so NEITHER can be handed a line whose
        // phonemes overflow its model and get silently dropped (the Core ML chain rejects an
        // over-long utterance with `phonemeSequenceTooLong`). Per text chunk: the ONNX path
        // ramps phoneme batches within the chunk (a small first batch for fast first-audio,
        // growing to the 510-phoneme cap) and the Core ML path synthesizes the chunk whole
        // (FluidAudio phonemizes internally). Each piece is synthesized FULLY before it's
        // appended; chunks/batches append to the one sink in order, so playback stays gapless.
        // Cancel is checked between pieces for prompt barge-in.
        //
        // Per-utterance timing for the app's engine stats: total synth time, total audio
        // produced (→ realtime factor), and time-to-first-audio. Emitted as a `STATS …` line.
        let t_req = std::time::Instant::now();
        let mut synth_nanos: u128 = 0;
        let mut total_samples: usize = 0;
        let mut first_ms = 0.0_f64;
        let mut produced = false;
        // ONE append path, shared by both engines: VPIO render ring (duplex owns render) or
        // the rodio sink (half-duplex / capture-only duplex). MUTE is REVERSIBLE on the rodio
        // path — push the REAL samples and attenuate via the player VOLUME (set at creation +
        // by the `mute` op), so unmuting mid-reply restores the buffered audio. The VPIO render
        // ring has no volume control, so there we zero the PCM when muted (best-effort; the
        // ring is only a few live frames, so unmute resumes within ~a frame).
        let append = |pcm: Vec<f32>| {
            if render_via_duplex {
                if let Some(dx) = &duplex {
                    let pcm = if muted.load(Ordering::SeqCst) {
                        vec![0.0; pcm.len()]
                    } else {
                        pcm
                    };
                    dx.render_push(&pcm);
                }
            } else if let Some(p) = &player {
                p.append(rodio::buffer::SamplesBuffer::new(channels, srate, pcm));
            }
        };
        match synth {
            // ONNX Kokoro: per SHARED text chunk, ramped phoneme-batch streaming.
            Backend::Ort(synth) => {
                // Probe espeak availability ONCE per utterance (skipped entirely for English),
                // not once per chunk — each probe spawns `espeak-ng --version`.
                let espeak_ok = g2p::needs_espeak(&voice) && g2p::espeak_available();
                'ort: for chunk in chunk_text(&text) {
                    let phonemes = g2p::phonemize_for_with(&chunk, &voice, espeak_ok);
                    for batch in stream_batches(&phonemes) {
                        if cancel.load(Ordering::SeqCst) {
                            break 'ort;
                        }
                        let t0 = std::time::Instant::now();
                        let pcm = match synth.synthesize(&batch, &voice, rate) {
                            Ok(p) if !p.is_empty() => p,
                            _ => continue,
                        };
                        synth_nanos += t0.elapsed().as_nanos();
                        if !produced {
                            first_ms = t_req.elapsed().as_secs_f64() * 1000.0;
                            produced = true;
                        }
                        total_samples += pcm.len();
                        if cancel.load(Ordering::SeqCst) {
                            break 'ort;
                        }
                        append(pcm);
                    }
                }
            }
            // Apple-native: per SHARED text chunk, synthesize the whole chunk (FluidAudio
            // phonemizes internally; the chunk bound keeps it under the model's phoneme cap).
            #[cfg(target_os = "macos")]
            Backend::Coreml(c) => {
                'cm: for chunk in chunk_text(&text) {
                    if cancel.load(Ordering::SeqCst) {
                        break 'cm;
                    }
                    let t0 = std::time::Instant::now();
                    match c.synthesize_text(&chunk, &voice, rate) {
                        Ok(pcm) if !pcm.is_empty() => {
                            synth_nanos += t0.elapsed().as_nanos();
                            if !produced {
                                first_ms = t_req.elapsed().as_secs_f64() * 1000.0;
                                produced = true;
                            }
                            total_samples += pcm.len();
                            if cancel.load(Ordering::SeqCst) {
                                break 'cm;
                            }
                            append(pcm);
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("dontspeak/helper: coreml synth failed: {e}"),
                    }
                }
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
        *cur_player.lock().unwrap() = None;
        // Stats BEFORE DONE (skip cancelled/empty utterances — they'd skew the RTF).
        if produced && !cancel.load(Ordering::SeqCst) {
            let synth_ms = synth_nanos as f64 / 1e6;
            let audio_ms = total_samples as f64 / 24_000.0 * 1000.0;
            println!("STATS synth_ms={synth_ms:.1} audio_ms={audio_ms:.1} first_ms={first_ms:.1}");
        }
        // Exactly one DONE per speak/preview request (even if cancelled).
        println!("DONE");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod audio_tests {
    use super::{LEAD_SILENCE_MS, leading_silence_pcm};

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
}
