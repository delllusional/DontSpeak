//! The `model_status` aggregator + the caps-event status channel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ds_config::{Paths, VoiceConfig};

use crate::config_gate::{
    apple_native_shim_available, caps_loop_enabled, cuda_runtime_present, kokoro_present_for,
    parakeet_available,
};
use crate::downloads::DownloadProg;
use ds_model::DownloadTarget;
use crate::engine::{PasteState, dictation_preview};
use crate::stats;
use crate::tts::TtsManager;
use ds_status::{
    CapsEvent as CapsEventDto, DiarStats, Dictation, EngineObj, EngineState, Loaded, ModelStatus,
    Running, Stats,
};

/// Epoch milliseconds, for ordering caps events the app displays as a live log.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A single caps-trigger event surfaced to the app over `model_status` (the
/// engine → app status channel). `kind` is a stable machine token the app maps to
/// a label: "press" / "release" / "start" / "stop" / "reset".
#[derive(Clone)]
pub(crate) struct CapsEvent {
    pub ts_ms: u64,
    pub kind: &'static str,
}

/// Shared, bounded log of recent caps events (newest last). Cloned into both the
/// engine's poll loop (writer) and the RPC status handler (reader).
pub(crate) type CapsLog = Arc<Mutex<VecDeque<CapsEvent>>>;
/// Keep only the most recent N events — this is a live status panel, not history.
pub(crate) const CAPS_LOG_MAX: usize = 50;

/// A monotonically-incrementing status SEQUENCE + a condvar, so a client can BLOCK
/// until ANY `model_status`-relevant state actually changes instead of polling for it.
/// Every component that flips a status flag [`bump`](StatusGate::bump)s it right after
/// the flip: the engine on dictation-preview changes (live partial, awaiting-confirm,
/// paste target) and recording start/stop; the TTS queue on playback start/stop
/// (`tts_active`); the listener on hands-free recording (`stt_active`); the
/// [`TtsManager`] on global mute; and engine start/stop (engineRunning transitions).
/// The `WaitModelStatus` IPC handler [`wait_changed`](StatusGate::wait_changed)s on it.
/// This turns the engine→app status transport from a 120 ms poll into a ~0-jitter PUSH
/// (the app calls the blocking FFI on a dedicated thread; see `ds_model_status_wait`).
pub(crate) struct StatusGate {
    /// Current sequence number; bumped on every status-affecting change.
    seq: Mutex<u64>,
    cv: Condvar,
}

impl StatusGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            seq: Mutex::new(0),
            cv: Condvar::new(),
        })
    }

    /// Advance the sequence and wake every blocked `wait_changed`.
    pub(crate) fn bump(&self) {
        let mut s = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        *s = s.wrapping_add(1);
        self.cv.notify_all();
    }

    /// The current sequence (embedded in `model_status_json` so the app echoes it
    /// back as `since` on the next wait).
    pub(crate) fn seq(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until the sequence differs from `since` (a status change landed) or
    /// `timeout` elapses, then return the current sequence. Returns immediately if
    /// the state already advanced past `since` while the caller was away.
    pub(crate) fn wait_changed(&self, since: u64, timeout: Duration) -> u64 {
        let guard = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        // Fast path: the state already advanced while the caller was away.
        if *guard != since {
            return *guard;
        }
        // `wait_timeout_while` re-checks the predicate (here "still unchanged") across
        // spurious wakeups, only returning once the seq differs from `since` or the
        // single `timeout` deadline elapses — the idiomatic guard against a notify that
        // races a wakeup. Predicate + the `bump` notify share this one `seq` mutex, so
        // there is no lost-wakeup window.
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |s| *s == since)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

/// The shared Arc handles threaded through the RPC server and the status
/// aggregator. Bundled into one struct so [`crate::ipc::spawn_ipc_server`] and
/// [`model_status_json`] take a single `&EngineShared` instead of a long list of
/// `Arc`-cloned args. Built ONCE in `engine_run` (same Arcs, same clones).
#[derive(Clone)]
pub(crate) struct EngineShared {
    pub tts: Arc<TtsManager>,
    pub caps_active: Arc<AtomicBool>,
    pub stt_active: Arc<AtomicBool>,
    pub caps_log: CapsLog,
    pub paste: PasteState,
    pub downloads: DownloadProg,
    pub tts_stats: Arc<stats::TtsStats>,
    pub stt_stats: Arc<stats::SttStats>,
    pub lifetime: Arc<stats::LifetimeSeconds>,
    /// The push gate the `WaitModelStatus` handler blocks on (shared with every
    /// component that flips a status flag, each of which bumps it after the flip).
    pub gate: Arc<StatusGate>,
}

/// Build the model presence + removability report (the engine is the authority:
/// it knows what it has loaded). A model is `removable` only if present AND not
/// currently running in the engine — for Kokoro/onnx that means the warm TTS
/// child is NOT alive; Parakeet shares the onnx dylib, so it's removable
/// whenever present unless the warm Kokoro child is holding that dylib.
pub(crate) fn model_status_json(
    shared: &EngineShared,
    paths: &Paths,
    tts_active: bool,
) -> serde_json::Value {
    let EngineShared {
        tts,
        caps_active,
        stt_active,
        caps_log,
        paste,
        downloads,
        tts_stats,
        stt_stats,
        lifetime,
        gate,
    } = shared;
    let cfg = VoiceConfig::load(paths);
    // The engines the preference ladders RESOLVE to on this build — every "is engine X
    // active" check below reads these, not the raw ladder (so an unusable rung is skipped).
    let resolved_tts = cfg.resolved_tts();
    let resolved_stt = cfg.resolved_stt();
    // Is the Kokoro warm child up (for removability + the Kokoro-engine case).
    let kokoro_warm = tts.is_running();
    // TTS "running" for the UI dot = the engine is on AND ready: off → never; System
    // (`say`) is always ready; Kokoro needs its warm child up.
    let tts_running = match resolved_tts {
        Some(ds_config::TtsEngine::System) => true,
        Some(ds_config::TtsEngine::Kokoro) => kokoro_warm,
        _ => false, // off / no usable rung
    };

    // CHEAP presence: file existence only — NO sha256. model_status is polled to
    // drive the UI's status dots, so it must be fast; full sha verification over
    // the 325MB Kokoro onnx + the Parakeet ONNX files would delay the dots by
    // many seconds. Correctness-critical sha checks stay in the load path
    // (load_synth / ParakeetModel::load), not here.
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    // The Kokoro row reflects the ACTIVE TTS backend (mirrors the Parakeet row below):
    //   * apple-native → gated on FluidAudio capability (macOS + shim); FluidAudio
    //                    self-manages its Core ML model cache, so there's no DontSpeak
    //                    on-disk file gate and nothing for us to remove.
    //   * onnx (cpu/coreml/cuda) → gated on the downloaded ONNX model + voices + dylib.
    // Without this branch the row read "missing" on the apple-native path (no ONNX
    // files) even though TTS works.
    let tts_uses_apple_native = cfg.uses_apple_native_model();
    let kokoro_onnx_files = exists(ds_model::model_path(ds_model::KOKORO_ONNX_FILE))
        && exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE))
        && exists(ds_model::onnxruntime_dylib_path());
    let kokoro_present = kokoro_present_for(
        tts_uses_apple_native,
        apple_native_shim_available(),
        kokoro_onnx_files,
    );
    // The STT engine is `parakeet`; the ACTIVE runtime is the resolved provider.
    //   * onnx         → gated on the downloaded ONNX model files (+ shared dylib);
    //                    the only runtime with deletable, downloadable files.
    //   * apple-native → gated on FluidAudio capability (macOS + shim); FluidAudio
    //                    self-manages its model cache (no on-disk file gate here).
    let parakeet_onnx_files = exists(ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_DECODER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_JOINER_FILE))
        && exists(ds_model::model_path(ds_model::PARAKEET_TOKENS_FILE))
        && exists(ds_model::onnxruntime_dylib_path());
    let stt_uses_onnx = matches!(
        cfg.resolved_stt_provider(),
        ds_config::Provider::OrtCpu | ds_config::Provider::OrtCuda
    );
    let parakeet_present = if stt_uses_onnx {
        parakeet_onnx_files
    } else {
        parakeet_available()
    };
    let parakeet_enabled = resolved_stt == Some(ds_config::SttEngine::BuiltIn);
    // "running" green dot: the selected engine is parakeet and its active runtime is ready.
    let parakeet_running = parakeet_enabled && parakeet_present;
    // System STT (macOS 26 on-device SpeechAnalyzer). No DontSpeak-managed download ring —
    // the OS owns the en-US model — but it has a real not-ready window: the first time it's
    // selected the model downloads. So it gets the SAME present/warming/running split as
    // Parakeet: "present" = can run (model installed OR downloading), "warming" (orange) =
    // model still being prepared, "running" (green) = model installed + ready NOW.
    let system_enabled = resolved_stt == Some(ds_config::SttEngine::System);
    // Only probe (a shim dlopen + Speech query) when System is actually selected — the
    // row is hidden otherwise, so non-system users pay nothing on the model-status poll.
    let system_state = if system_enabled {
        ds_stt::system_state()
    } else {
        ds_stt::SystemState::Unavailable
    };
    // present = can run (model installed OR still downloading); running (green) = installed
    // + ready NOW. When present && !running && enabled, engine_obj derives "warming"
    // (orange) — the "preparing" dot, mirroring Parakeet while its model loads.
    let system_present = system_enabled && system_state != ds_stt::SystemState::Unavailable;
    let system_running = system_state == ds_stt::SystemState::Ready;

    // claude_code STT — delegate to Claude Code's own voice dictation. READ Claude Code's
    // config (settings.json voice + keybindings.json) ONLY when it's the selected engine
    // (the row is hidden otherwise, so non-claude_code users pay no file IO). "present" =
    // CC voice is enabled AND its bound key is one we can synthesize; otherwise we surface
    // a "how to enable" hint instead of silently doing nothing.
    let claude_code_enabled = resolved_stt == Some(ds_config::SttEngine::ClaudeCode);
    // `claude_code_key` = the human label of the keypress we SYNTHESIZE into Claude Code
    // (its bound `voice:pushToTalk`); the app shows it instead of local STT stats, since
    // claude_code does no local transcription — "we just press this key, Claude Code does
    // the rest". `None` when the engine isn't usable (the row shows the error hint instead).
    let (claude_code_present, claude_code_running, claude_code_error, claude_code_key) =
        if claude_code_enabled {
            let cc = ds_config::read_claude_code_voice(paths);
            let chord = ds_platform::KeyChord::parse(&cc.key);
            let present = cc.enabled && chord.is_supported();
            let error = if present {
                None
            } else if !cc.enabled {
                Some(ds_i18n::t("status.engine.reason.cc_voice_off"))
            } else {
                Some(ds_i18n::t_args_json(
                    "status.engine.reason.cc_key_unsupported",
                    &serde_json::json!({ "key": chord.label() }).to_string(),
                ))
            };
            let key = present.then(|| chord.label().to_string());
            (present, present, error, key)
        } else {
            (false, false, None, None)
        };

    // Recent caps-trigger events for the app's status panel (newest last).
    let caps_events: Vec<CapsEventDto> = caps_log
        .lock()
        .map(|q| {
            q.iter()
                .map(|e| CapsEventDto {
                    ts: e.ts_ms,
                    kind: e.kind.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    // Dictation-preview snapshot for the confirm panel (see `dictation_preview`): the
    // finalized transcript while awaiting confirmation, else the live partial — but never
    // the finalized text while a Caps press is in flight (a long-press cancel mustn't flash
    // the bubble before it dismisses).
    let (dict_text, dict_awaiting, dict_target, dict_has_target) = paste
        .lock()
        .map(|p| {
            let (text, awaiting) = dictation_preview(p.pending.as_deref(), &p.partial, p.caps_held);
            (text, awaiting, p.target.clone(), p.has_paste_target)
        })
        .unwrap_or((String::new(), false, None, true));

    // Background-download snapshot → per-engine "state"/"progress"/"error" so the
    // app renders the lifecycle dot directly (engine owns the decision). A model
    // setup also pulls the shared onnx dylib, so ANY active download counts onnx
    // as downloading too.
    let (dl_target, dl_done, dl_total, dl_err) = {
        let s = downloads.lock().unwrap();
        (s.active_target, s.done, s.total, s.last_error.clone())
    };
    let dl_frac = if dl_total > 0 {
        dl_done as f64 / dl_total as f64
    } else {
        0.0
    };
    // An active "all" fetch counts as downloading EVERY model it produces; otherwise the row
    // matches its own target. (Note: the voices-only `KokoroVoices` fetch does NOT light the
    // Kokoro row here — that ANE-path ring is driven by `tts.tts_downloading()` below, exactly
    // as before the rename.)
    let downloading =
        |eng: DownloadTarget| dl_target == Some(eng) || dl_target == Some(DownloadTarget::All);
    let dl_err_for = |eng: DownloadTarget| {
        dl_err
            .as_ref()
            .filter(|(t, _)| *t == eng || *t == DownloadTarget::All)
            .map(|(_, m)| m.clone())
    };
    // Build one engine object with a lifecycle `state` (the app maps it 1:1 to a
    // status dot): downloading > failed > missing > running > warming > idle.
    let engine_obj = |present: bool,
                      removable: bool,
                      dling: bool,
                      error: Option<String>,
                      running: bool,
                      enabled: bool,
                      progress: f64,
                      dl_files: (u64, u64)|
     -> EngineObj {
        let state = engine_state(present, dling, error.is_some(), running, enabled);
        let (dl_index, dl_count) = if dling { dl_files } else { (0, 0) };
        EngineObj {
            present,
            removable,
            state: state.as_str().to_string(),
            progress: if dling { progress } else { 0.0 },
            error,
            dl_index,
            dl_count,
        }
    };
    // Apple-native Kokoro/Parakeet download % comes from the warm child (it fetches the Core ML
    // models itself); the ONNX path uses the download-manager fraction. Picks whichever is live.
    let kokoro_progress = if tts.tts_downloading() {
        tts.tts_dl_progress()
    } else {
        dl_frac
    };
    let parakeet_progress = if tts.stt_downloading() {
        tts.stt_dl_progress()
    } else {
        dl_frac
    };

    // Kokoro row reflects the Kokoro MODEL: running = warm child up; enabled =
    // Kokoro is the selected TTS engine AND TTS is on; failed = warm-load error
    // (present but won't start) or a failed Kokoro download.
    let kokoro_enabled = resolved_tts == Some(ds_config::TtsEngine::Kokoro);
    // A warm-load error means "present but won't start" — a real failure ONLY when the
    // model is present. On a clean install the warm child also errors ("kokoro model not
    // downloaded"), but that's the `missing` state (offer Download), not a failure — so
    // ignore the load error unless the model is present (else the row reads red "failed"
    // instead of the download affordance). A genuine download failure always surfaces.
    let kokoro_error = dl_err_for(DownloadTarget::KokoroModel).or_else(|| {
        if kokoro_present {
            tts.last_error()
        } else {
            None
        }
    });

    // System TTS (macOS `say`) — the speech-OUT analogue of the System STT row. No model
    // to download/remove; present + running when it's the selected engine and TTS is on,
    // so the adaptive TTS row can show "System" (green) instead of a greyed-out Kokoro.
    let tts_system_enabled = resolved_tts == Some(ds_config::TtsEngine::System);
    let tts_system_running = tts_system_enabled; // System selected ⇒ on (no separate flag)

    // Diarization model presence (FluidAudio's self-managed cache).
    let diar_present = diarization_present();

    // "downloading" comes from the HELPER, not a disk heuristic: on the ANE path the warm
    // child emits `DOWNLOADING tts`/`stt` when the Core ML model is absent and clears it on
    // READY/STTLOADED, so the dot reads "downloading" for exactly the fetch window and never a
    // premature "starting"/green (a partial download dir can't be told from a complete one on
    // disk). The ONNX path keeps its download-manager ring. GREEN = `tts_loaded`/`stt_loaded`,
    // which the helper sets only AFTER the model is resident + warm.
    let kokoro_dling = downloading(DownloadTarget::KokoroModel) || tts.tts_downloading();
    let parakeet_dling =
        (stt_uses_onnx && downloading(DownloadTarget::ParakeetModel)) || tts.stt_downloading();

    let status = ModelStatus {
        // Removable only on the ONNX path (apple-native has no DontSpeak-managed Kokoro
        // files — FluidAudio self-manages its cache, mirroring the Parakeet row) AND
        // while the WARM Kokoro child isn't holding the files (the System engine doesn't
        // warm Kokoro, so the files are free even with TTS on).
        kokoro: engine_obj(
            kokoro_present,
            !tts_uses_apple_native && kokoro_present && !kokoro_warm,
            kokoro_dling,
            kokoro_error,
            tts.tts_loaded(),
            kokoro_enabled,
            kokoro_progress,
            tts.tts_dl_files(),
        ),
        // Parakeet STT — one engine, runtime chosen by `stt_provider`. With the ONNX
        // runtime it has downloadable model files (removable only when the warm Kokoro
        // child isn't holding the shared dylib) and shows a download ring; with
        // apple-native FluidAudio self-manages its cache (never removable, no ring).
        parakeet: engine_obj(
            parakeet_present,
            stt_uses_onnx && parakeet_present && !kokoro_warm,
            parakeet_dling,
            if stt_uses_onnx {
                dl_err_for(DownloadTarget::ParakeetModel)
            } else {
                None
            },
            tts.stt_loaded() && parakeet_enabled,
            parakeet_enabled,
            parakeet_progress,
            tts.stt_dl_files(),
        ),
        // Speaker diarization / speaker-LOCK (FluidAudio Core ML, self-managed cache like
        // apple-native Parakeet — never removable). The dot tracks the speaker-LOCK feature
        // the user actually turns on: GREEN (`running`) only when `stt_speaker_lock` is on
        // AND diarization is enabled AND the models are present (the lock can actually
        // isolate the enrolled voice); GREY (`idle`) when the lock is off — even though
        // diarization may be enabled under the hood for the diarize/enroll tools. Missing →
        // the Download button; orange while the shim fetches its models.
        diarization: engine_obj(
            diar_present,
            false,
            downloading(DownloadTarget::Diarization),
            dl_err_for(DownloadTarget::Diarization),
            cfg.stt_speaker_lock && cfg.diarization_on() && diar_present,
            cfg.stt_speaker_lock,
            dl_frac,
            (0, 0),
        ),
        // System STT (macOS 26 on-device SpeechAnalyzer) — the OS owns the model, so
        // there's nothing for DontSpeak to remove and no download RING (no progress): never
        // `removable`, never `downloading`. But it warms like Parakeet: the state machine
        // derives "warming" (orange) from present && !running && enabled — true while the
        // en-US model is still being prepared (present but not Ready) — then "running"
        // (green) once it's installed, "missing" when selected but unavailable (macOS < 26 /
        // unsupported locale) so the dot honestly shows it can't run, no silent fallback.
        system: engine_obj(
            system_present,
            false,
            false,
            None,
            system_running,
            system_enabled,
            0.0,
            (0, 0),
        ),
        // claude_code STT — Claude Code does the (cloud) transcription; nothing to download
        // or remove. "present" = CC voice on + key synthesizable; the `error` carries the
        // "run /voice" / "rebind the key" hint so the UI can tell the user how to enable it.
        claude_code: engine_obj(
            claude_code_present,
            false,
            false,
            claude_code_error,
            claude_code_running,
            claude_code_enabled,
            0.0,
            (0, 0),
        ),
        // System TTS (macOS `say`) — the speech-OUT analogue of the System STT row, so the
        // adaptive TTS row can show "System" (green when selected + TTS on) instead of a
        // greyed-out Kokoro. No model to download/remove.
        tts_system: engine_obj(
            tts_system_enabled,
            false,
            false,
            None,
            tts_system_running,
            tts_system_running,
            0.0,
            (0, 0),
        ),
        // The ACTIVE STT engine token, so the app's single STT row can reflect whichever
        // engine is selected (parakeet vs system) without inferring it from the dots.
        stt_engine: resolved_stt
            .map(|e| e.as_str())
            .unwrap_or(ds_config::SttEngine::Off.as_str())
            .to_string(),
        // The ACTUAL STT runtime for the built_in (Parakeet) engine — "ane" (FluidAudio
        // Core ML / ANE — the neural engine), "ort_cuda" (ort, NVIDIA GPU) or "ort_cpu" (ort,
        // CPU). Like the TTS `tts_provider`, this is HONEST about fallback: `ane` degrades to
        // ort_cpu when the FluidAudio shim is absent, and `ort_cuda` degrades to ort_cpu when the
        // GPU runtime isn't fetched — the SAME checks the loaders use (`for_provider` on
        // SMKOKORO_DYLIB_PATH, the GPU-runtime probe), so engine + helper agree. Null for
        // system/claude_code.
        stt_provider: stt_provider_token(
            resolved_stt,
            cfg.resolved_stt_provider(),
            apple_native_shim_available(),
            cuda_runtime_present(),
        ),
        // The ACTIVE TTS engine token ("built_in" = Kokoro, "system" = `say`), so the app's
        // TTS row adapts the same way the STT row does (built_in → Kokoro, system → System).
        tts_engine: resolved_tts
            .map(|e| e.as_str())
            .unwrap_or(ds_config::TtsEngine::Off.as_str())
            .to_string(),
        // The ACTUAL TTS runtime the warm Kokoro child is on, as a config-style TOKEN
        // (`ane`/`ort_coreml`/`ort_cuda`/`ort_cpu`) so it matches `stt_provider`'s vocabulary
        // AND round-trips with the `tts_provider` setting. Mapped from the live PROVIDER the
        // child reports ("CoreML-ANE"/"CoreML"/"CUDA"/"CPU"). Null for the system (`say`) engine.
        tts_provider: tts_provider_token(resolved_tts, tts.provider().as_str()),
        // The keypress we synthesize into Claude Code (its bound voice key), shown in the
        // claude_code row instead of local stats. Null unless claude_code is selected + usable.
        claude_code_key,
        // Back-compat: the flat running map the MCP `status`/`model_status` tools read.
        running: Running {
            caps: caps_active.load(Ordering::Relaxed),
            // The raw `caps_enabled` SETTING (before the Accessibility preflight that
            // `caps` also folds in), so the UI can tell "off" from "on but blocked by a
            // missing permission" and warn accordingly. Cheap: a tiny TOML read per poll.
            caps_wanted: caps_loop_enabled(&VoiceConfig::load(paths)),
            stt_active: stt_active.load(Ordering::Relaxed),
            // True while TTS audio is actually playing — drives the menu-bar
            // TTS state, mirroring `stt_active` for the capture state.
            tts_active,
            // Global MUTE (Caps-tap when dictation is off, or the tray checkbox): playback
            // still runs, only the audio is silenced. Drives the tray "Mute" toggle + the
            // faded menu-bar icon.
            muted: tts.is_muted(),
            // Kokoro-SPECIFIC (not "is any TTS running"): `tts_running` is true for System
            // `say` too, so gate on the Kokoro engine actually being the selected one.
            kokoro: tts_running && resolved_tts == Some(ds_config::TtsEngine::Kokoro),
            tts_system: tts_system_running,
            parakeet: parakeet_running,
            system: system_running,
            claude_code: claude_code_running,
        },
        // Dictation confirm-panel state: `recording` while capturing (live
        // partials in `text`), `awaiting_confirm` once the transcript is finalized
        // and waiting for the Caps confirm tap (`text` is then the final), `target`
        // = the app focused when recording started (the paste destination).
        // `local_stt` = this dictation is the local-transcript (Parakeet) path, so
        // the overlay should appear THE MOMENT recording starts (don't wait for the
        // first partial); ClaudeNative produces no partials, so its panel stays
        // suppressed (it submits straight to Claude).
        dictation: Dictation {
            recording: stt_active.load(Ordering::Relaxed),
            awaiting_confirm: dict_awaiting,
            text: dict_text.clone(),
            target: dict_target,
            // Both local STT engines deposit a confirm-panel transcript (Parakeet and
            // System); ClaudeNative submits straight to Claude and shows no panel.
            local_stt: parakeet_running || system_running,
            // LIVE: is an editable text field focused to receive the paste? Sampled each
            // tick while the panel is up. The app tints the dictation glow when false
            // ("no input to submit into"). Replaces the old `no_target_warn` red flash.
            has_paste_target: dict_has_target,
            // The "speak now" glow decision, computed HERE so every platform's overlay
            // pulses identically and can't drift: glow only while actively recording with
            // nothing transcribed yet and not already awaiting the confirm tap — i.e. the
            // empty pill prompting the user to talk. Once words arrive (or we're awaiting
            // confirmation, or capture stopped) it goes static. The no-target warning glow
            // is a SEPARATE cue driven by `has_paste_target`.
            prompt_glow: stt_active.load(Ordering::Relaxed)
                && dict_text.is_empty()
                && !dict_awaiting,
        },
        // Menu-bar icon preference (app-only; the engine just passes it through): a SET of
        // tokens, e.g. ["stt","tts"] (both), ["stt"], or [] (never color). Drives which states
        // color the tray.
        tray_indicator: cfg
            .tray_indicator
            .iter()
            .map(|k| k.as_str().to_string())
            .collect(),
        // Live engine stats for the app's stats view: TTS + STT realtime factors /
        // counts, lifetime totals, and which models are resident in the warm helper.
        stats: Stats {
            tts: tts_stats.snapshot(),
            stt: stt_stats.snapshot(),
            // Persisted lifetime seconds (spoken + heard) across all sessions.
            lifetime: lifetime.snapshot(),
            // Which models are CURRENTLY resident in the warm helper — the honest
            // signal for "did Parakeet unload" (the memory number is noisy: ort
            // retains freed arena while TTS keeps synthesizing).
            loaded: Loaded {
                tts: tts.tts_loaded(),
                stt: tts.stt_loaded(),
            },
            // Diarization stats for the Settings row's expansion: enabled, model presence,
            // the enrolled voiceprint names (so the row can show "who it recognizes"), and
            // the live thresholds. Lives UNDER `stats` (where the app's EngineStats.parse
            // reads it) — NOT at the root, where it would collide with the diarization
            // engine_obj dot below and clobber its `state` (so the dot never goes green).
            // On-demand, so there's no realtime-factor like STT/TTS.
            diarization: DiarStats {
                enabled: cfg.diarization_on(),
                present: diar_present,
                // The resolved diarizer runtime in the SAME token vocabulary as
                // tts_provider/stt_provider, so the row's "Runtime" line reuses runtimeLabel
                // (the single apple_native rung is Core ML / ANE → "ane"). On-demand, so no
                // realtime factor.
                runtime: match cfg.resolved_diarizer() {
                    ds_config::DiarizerProvider::AppleNative => "ane",
                }
                .to_string(),
                speakers: ds_config::SpeakerStore::load(&paths.speakers_json).names(),
                clustering_threshold: cfg.clustering_threshold as f64,
                speaker_threshold: cfg.speaker_threshold as f64,
            },
        },
        // Engine → app caps status channel: a bounded log of recent press/release/
        // tap/reset events the Settings window renders live.
        caps_events,
        // Build-id handshake: the app compares this against its own embedded id and
        // restarts the engine if they drift (see build.rs / bundle.sh lockstep).
        build_id: env!("DONTSPEAK_BUILD_ID").to_string(),
        // Push sequence: the app echoes this back as `since` on the next
        // `WaitModelStatus` so it blocks until the NEXT change (see `StatusGate`).
        seq: gate.seq(),
    };
    serde_json::to_value(status).unwrap()
}

/// The STT runtime TOKEN the UI shows — the ACTUAL runtime, NOT the naive resolved preference.
/// `ane` degrades to `ort_cpu` when the FluidAudio shim is absent, and `ort_cuda` degrades to
/// `ort_cpu` when the GPU runtime isn't fetched — the SAME gates the loaders use (`for_provider`,
/// the GPU-runtime probe), so the row matches what really loaded. `None` for non-built_in engines
/// (claude_code/system/off have no local Parakeet runtime). Pure (callers pass the live
/// `shim_ok`/`cuda_present`) so the "actual, not naive" invariant is unit-tested — see
/// [`provider_token_tests`]. (NOTE: the streaming runner currently builds CPU-only ort sessions —
/// int8 dynamic-quant isn't GPU-accelerated — so an `ort_cuda` token reflects the resolved
/// PREFERENCE; STT compute is on CPU regardless until/unless a GPU EP is wired for streaming.)
fn stt_provider_token(
    resolved_stt: Option<ds_config::SttEngine>,
    resolved_provider: ds_config::Provider,
    shim_ok: bool,
    cuda_present: bool,
) -> Option<String> {
    use ds_config::{Provider, SttEngine};
    match resolved_stt {
        Some(SttEngine::BuiltIn) => Some(
            match resolved_provider {
                Provider::Ane if !shim_ok => Provider::OrtCpu.as_str(),
                Provider::OrtCuda if !cuda_present => Provider::OrtCpu.as_str(),
                other => other.as_str(),
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// The TTS runtime TOKEN the UI shows — mapped from the live PROVIDER the warm Kokoro child
/// reports (`"CoreML-ANE"`/`"CoreML"`/`"CUDA"`/`"CPU"`), i.e. what ACTUALLY loaded (the child
/// builds its own ort session and records the realized EP, CPU fallback included), not a
/// preference. `None` for the System (`say`) / Off engines (no Kokoro runtime).
fn tts_provider_token(
    resolved_tts: Option<ds_config::TtsEngine>,
    child_provider: &str,
) -> Option<String> {
    use ds_config::{Provider, TtsEngine};
    match resolved_tts {
        Some(TtsEngine::Kokoro) => Some(
            match child_provider {
                "CoreML-ANE" => Provider::Ane.as_str(),
                "CoreML" => Provider::OrtCoreMl.as_str(),
                "CUDA" => Provider::OrtCuda.as_str(),
                _ => Provider::OrtCpu.as_str(),
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// Whether FluidAudio's speaker-diarization Core ML models are on disk in our `coreml_dir`.
/// Uses the SAME completion-marker check the downloader writes (`coreml_repo_present`), so the
/// status row and the downloader can never disagree about one location — a partial/aborted
/// fetch (subdir exists, no `.ds-ready` marker) reads MISSING here exactly as it does to the
/// downloader, instead of the old substring heuristic that called a half-download "present".
fn diarization_present() -> bool {
    ds_model::coreml_repo::coreml_repo_present(&ds_model::coreml_repo::DIARIZER_COREML)
}

/// PURE lifecycle-state (the app maps it 1:1 to a status dot). Precedence:
/// `downloading > failed > missing > running > warming > idle`. Extracted so the ordering —
/// in particular "a model still downloading is NEVER green/running" — is unit-tested. Returns
/// the canonical [`EngineState`]; the caller stores its `.as_str()` into the wire DTO.
pub(crate) fn engine_state(
    present: bool,
    dling: bool,
    has_error: bool,
    running: bool,
    enabled: bool,
) -> EngineState {
    if dling {
        EngineState::Downloading
    } else if has_error {
        EngineState::Failed
    } else if !present {
        EngineState::Missing
    } else if running {
        EngineState::Running
    } else if enabled {
        EngineState::Warming
    } else {
        EngineState::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::{engine_state, stt_provider_token, tts_provider_token};
    use ds_config::{Provider, SttEngine, TtsEngine};
    use ds_status::EngineState;

    #[test]
    fn stt_provider_is_actual_not_naive() {
        // GUARD against the "UI claims CUDA but runs CPU" trap: ort_cuda DEGRADES to ort_cpu when
        // the GPU runtime isn't fetched — NOT the naive resolved preference.
        let b = Some(SttEngine::BuiltIn);
        assert_eq!(
            stt_provider_token(b, Provider::OrtCuda, false, false).as_deref(),
            Some("ort_cpu")
        );
        assert_eq!(
            stt_provider_token(b, Provider::OrtCuda, false, true).as_deref(),
            Some("ort_cuda")
        );
        // ane degrades to ort_cpu without the FluidAudio shim; ort_cpu is always itself.
        assert_eq!(
            stt_provider_token(b, Provider::Ane, false, false).as_deref(),
            Some("ort_cpu")
        );
        assert_eq!(
            stt_provider_token(b, Provider::Ane, true, false).as_deref(),
            Some("ane")
        );
        assert_eq!(
            stt_provider_token(b, Provider::OrtCpu, true, true).as_deref(),
            Some("ort_cpu")
        );
        // No local runtime for the delegate/OS engines or when TTS/STT is off.
        assert_eq!(
            stt_provider_token(Some(SttEngine::ClaudeCode), Provider::OrtCuda, true, true),
            None
        );
        assert_eq!(
            stt_provider_token(None, Provider::OrtCuda, true, true),
            None
        );
    }

    #[test]
    fn tts_provider_reflects_the_childs_realized_runtime() {
        // The token is what the warm child ACTUALLY loaded, not a preference.
        let k = Some(TtsEngine::Kokoro);
        assert_eq!(tts_provider_token(k, "CUDA").as_deref(), Some("ort_cuda"));
        assert_eq!(tts_provider_token(k, "CPU").as_deref(), Some("ort_cpu"));
        assert_eq!(tts_provider_token(k, "CoreML-ANE").as_deref(), Some("ane"));
        assert_eq!(tts_provider_token(Some(TtsEngine::System), "CUDA"), None);
        assert_eq!(tts_provider_token(None, "CUDA"), None);
    }

    #[test]
    fn engine_state_precedence_table() {
        // The model lifecycle the app maps to a dot. `dling` comes from the helper's
        // DOWNLOADING signal, `running` from tts_loaded/stt_loaded (set only after the model
        // is resident + warm) — so on a clean install: downloading ⇒ orange "Downloading…",
        // then (briefly) warming ⇒ "Starting…", then running ⇒ green. Never green mid-fetch.
        assert_eq!(
            engine_state(true, true, true, true, true),
            EngineState::Downloading
        ); // dling wins
        assert_eq!(
            engine_state(false, false, true, true, true),
            EngineState::Failed
        ); // error over missing
        assert_eq!(
            engine_state(false, false, false, false, true),
            EngineState::Missing
        );
        assert_eq!(
            engine_state(true, false, false, true, true),
            EngineState::Running
        );
        // Downloaded, loading into memory (not yet `running`) ⇒ "warming" = "Starting…".
        assert_eq!(
            engine_state(true, false, false, false, true),
            EngineState::Warming
        );
        assert_eq!(
            engine_state(true, false, false, false, false),
            EngineState::Idle
        );
        // The regression guard: a helper DOWNLOADING signal forces "downloading" even if the
        // present/running flags say otherwise (e.g. a non-empty partial dir on disk).
        assert_eq!(
            engine_state(true, true, false, false, true),
            EngineState::Downloading
        );
    }

    #[test]
    fn tts_and_stt_states_are_independent() {
        // The point of parallel init: each engine's dot is computed from ITS OWN flags, so STT
        // can read "downloading" while TTS is already "running" — neither gates the other.
        let stt = engine_state(
            true, /* dling */ true, false, /* running */ false, true,
        );
        let tts = engine_state(
            true, /* dling */ false, false, /* running */ true, true,
        );
        assert_eq!((tts, stt), (EngineState::Running, EngineState::Downloading));
        // ...and the reverse pairing (TTS still fetching while STT is warm) holds too.
        let tts = engine_state(true, true, false, false, true);
        let stt = engine_state(true, false, false, true, true);
        assert_eq!((tts, stt), (EngineState::Downloading, EngineState::Running));
    }
}
