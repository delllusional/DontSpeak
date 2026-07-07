//! The `model_status` aggregator + the caps-event status channel.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ds_config::{Paths, VoiceConfig};

use crate::config_gate::{
    apple_native_shim_available, apple_native_tts_active, caps_loop_enabled,
    kokoro_onnx_files_present, kokoro_present_for, parakeet_available, parakeet_onnx_files_present,
    stt_uses_onnx_runtime,
};
use crate::downloads::{DownloadProg, TargetState};
use crate::engine::{PasteState, dictation_preview};
use crate::stats;
use crate::tts::TtsManager;
use ds_model::DownloadTarget;
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
    // The Kokoro row reflects the ACTIVE TTS backend (mirrors the Parakeet row below):
    //   * apple-native → gated on the shim (the loader) + the downloaded Core ML sets (the
    //                    engine's download manager fetches them — target `kokoro_coreml` —
    //                    and FluidAudio only LOADS, enforceOffline). Presence reads the SAME
    //                    revision-pinned completion markers the downloader writes, so a
    //                    partial fetch reads MISSING here exactly as it does to the fetcher.
    //   * onnx (cpu/coreml/cuda) → gated on the downloaded ONNX model + voices + dylib.
    // `uses_apple_native_model()` / `resolved_stt_provider()` resolve to `Ane` as a STATIC
    // preference on ANY macOS, but the ANE (FluidAudio Core ML) backend only actually serves when
    // its shim dylib is present; without it the warm child DOWNGRADES to the ONNX-CPU path (e.g.
    // Intel macOS — see the realized `tts_provider`/`stt_provider` below, and the identical
    // `ane_active` guard in `downloads::auto_download_missing`). So gate the ROW's apple-native-ness
    // on the SAME runtime truth (shim present), else the ONNX-CPU path's row reads "missing" (no
    // Core ML files) even though the model loaded and is actively running on CPU.
    let shim = apple_native_shim_available();
    let tts_uses_apple_native = apple_native_tts_active(&cfg);
    let kokoro_files = if tts_uses_apple_native {
        ds_model::coreml_repo::is_coreml_set_present(&ds_model::coreml_repo::KOKORO_COREML_SET)
    } else {
        kokoro_onnx_files_present()
    };
    let kokoro_present = kokoro_present_for(tts_uses_apple_native, shim, kokoro_files);
    // The STT engine is `parakeet`; the ACTIVE runtime is the resolved provider.
    //   * onnx         → gated on the downloaded ONNX model files (+ shared dylib).
    //   * apple-native → gated on the shim + ITS downloaded Core ML sets (target
    //                    `parakeet_coreml`), marker-checked like the Kokoro row above.
    let parakeet_onnx_files = parakeet_onnx_files_present();
    // Same shim-aware downgrade as Kokoro above: the STT provider resolves to `Ane` as a static
    // preference, but with no Core ML shim the warm child runs Parakeet on the ONNX-CPU path — so
    // the row must gate on the downloaded ONNX files, not the (absent) apple-native cache.
    let stt_uses_onnx = stt_uses_onnx_runtime(cfg.resolved_stt_provider(), shim);
    let parakeet_present = if stt_uses_onnx {
        parakeet_onnx_files
    } else {
        parakeet_available() // shim + the downloaded Core ML sets (see config_gate)
    };
    let parakeet_enabled = resolved_stt == Some(ds_config::SttEngine::BuiltIn);
    // "running" green dot: the selected engine is parakeet and its active runtime is ready.
    let parakeet_running = parakeet_enabled && parakeet_present;
    // System STT (Apple's on-device recognizer). No DontSpeak-managed download ring —
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
    let (dict_text, dict_awaiting, dict_target, dict_has_target, dict_refused) = paste
        .lock()
        .map(|p| {
            let (text, awaiting) = dictation_preview(&p.final_state, &p.partial, p.caps_held);
            (
                text,
                awaiting,
                p.target.clone(),
                p.has_paste_target,
                // LIVE refusal window (same clock the tick digest hashes, so this snapshot
                // and the push that woke the app agree): a Caps tap the engine refused
                // because the selected engine can't transcribe yet.
                crate::engine::refusal_live(p.refused_until, std::time::Instant::now()),
            )
        })
        .unwrap_or((String::new(), false, None, true, false));

    // Background-download snapshot → per-engine "state"/"progress"/"error" so the
    // app renders the lifecycle dot directly (engine owns the decision). Targets
    // download in PARALLEL, one progress entry each — every row reports its OWN
    // target's fraction, never a shared value mirrored onto whichever row happened
    // to be fetching.
    let dl = downloads.lock().unwrap().targets.clone();
    // A row lights "downloading" for exactly ITS OWN in-flight target. (Note: the
    // voices-only `KokoroVoices` fetch does NOT light the Kokoro row — it only gates the
    // ACTIVE VOICE of an already-runnable model, so a ring there would read as "TTS not
    // ready" while TTS actually works.)
    let downloading = |eng: DownloadTarget| matches!(dl.get(&eng), Some(TargetState::Active(_)));
    // Active-only: a Done entry's finished % must NOT feed these direct per-target
    // fractions (the row-level Done fallback lives in `row_download_frac`).
    let frac_for = |eng: DownloadTarget| match dl.get(&eng) {
        Some(TargetState::Active(p)) => p.frac(),
        _ => 0.0,
    };
    let dl_err_for = |eng: DownloadTarget| match dl.get(&eng) {
        Some(TargetState::Failed(e)) => Some(e.clone()),
        _ => None,
    };
    // Kokoro row reflects the Kokoro MODEL: running = warm child up; enabled =
    // Kokoro is the selected TTS engine AND TTS is on; failed = warm-load error
    // (present but won't start) or a failed Kokoro download. BOTH flavors of the
    // Kokoro fetch (ONNX `kokoro_model`, apple-native `kokoro_coreml`) run in the
    // download manager, so one per-target progress/error channel serves every platform.
    let kokoro_enabled = resolved_tts == Some(ds_config::TtsEngine::Kokoro);
    // A warm-load error means "present but won't start" — a real failure ONLY when the
    // model is present. On a clean install the warm child also errors ("kokoro model not
    // downloaded"), but that's the `missing` state (offer Download), not a failure — so
    // ignore the load error unless the model is present (else the row reads red "failed"
    // instead of the download affordance). A genuine download failure always surfaces.
    let kokoro_error = combined_error(
        kokoro_present,
        dl_err_for(DownloadTarget::KokoroModel)
            .or_else(|| dl_err_for(DownloadTarget::KokoroCoreml)),
        // A mid-session `load tts`/preload failure (e.g. a transient AV-scan file-not-found on
        // an already-downloaded model) ahead of the warm-CHILD start failure — both are
        // per-Kokoro, so either surfaces.
        tts.tts_load_error().or_else(|| tts.last_error()),
    );

    // System TTS (macOS `say`) — the speech-OUT analogue of the System STT row. No model
    // to download/remove; present + running when it's the selected engine and TTS is on,
    // so the adaptive TTS row can show "System" (green) instead of a greyed-out Kokoro.
    let tts_system_enabled = resolved_tts == Some(ds_config::TtsEngine::System);
    let tts_system_running = tts_system_enabled; // System selected ⇒ on (no separate flag)

    // Diarization model presence (FluidAudio's self-managed cache).
    let diar_present = diarization_present();
    // The SepFormer separator the speaker-LOCK pairs with diarization: without it the lock
    // fails open (transcribes unfiltered), so the row's green must require it too. Plain
    // existence — the pinned sha was verified at download time (`ensure`), and hashing
    // ~29 MB per status poll would be waste.
    let sepformer_present = ds_model::model_path(ds_model::SEPFORMER_FILE)
        .map(|p| p.is_file())
        .unwrap_or(false);

    // "downloading" comes from the DOWNLOAD MANAGER on every platform — EVERY model fetch
    // (ONNX models, the apple-native Core ML sets, the GPU runtime) runs there as its own
    // single-flight target, so the dot reads "downloading" for exactly the fetch window and
    // never a premature "starting"/green (presence is marker-gated, so a partial download
    // reads missing, and the state precedence puts downloading first). GREEN =
    // `tts_loaded`/`stt_loaded`, which the helper sets only AFTER the model is resident + warm.
    // The shared GPU runtime is a compute DEPENDENCY of the ONNX engines: on a first-boot NVIDIA
    // box it's fetched (single-flighted) AHEAD of the model download, and until it lands neither
    // ONNX engine can run — so the rows must read "downloading" (with the runtime's %), not a stale
    // "missing"/"ort cpu". Gated to the ONNX path: on the apple-native (ANE / macOS) path the
    // runtime isn't used and `Cuda` is never a download target, so this is a no-op there.
    // BUT once the row's own engine has already loaded, a Cuda fetch that outlives it (e.g. a
    // SECOND box's Parakeet/Kokoro warming up onto the same shared runtime, or the runtime
    // re-verifying after the row's model is already resident+warm) must NOT force the row back
    // to "downloading" — that was the bug (a fully-loaded, working Kokoro flashing back to a
    // ~25% ring). See `row_downloading` for the precise gate: Cuda alone can only force
    // "downloading" while the row's own engine has NOT yet loaded.
    //
    // `tts_loaded`/`stt_loaded` (GREEN, set only after the model is resident + warm) are
    // hoisted here — reused below at `running:` and again in the `stats.loaded` block — so
    // `row_downloading` can read "has this row's engine already loaded" without a second call.
    let tts_loaded = tts.is_tts_loaded();
    let stt_loaded = tts.is_stt_loaded();
    let cuda_downloading = downloading(DownloadTarget::Cuda);
    let kokoro_own_downloading =
        downloading(DownloadTarget::KokoroModel) || downloading(DownloadTarget::KokoroCoreml);
    let kokoro_downloading = row_downloading(
        kokoro_own_downloading,
        cuda_downloading,
        tts_loaded,
        tts_uses_apple_native,
    );
    let parakeet_own_downloading = (stt_uses_onnx && downloading(DownloadTarget::ParakeetModel))
        || downloading(DownloadTarget::ParakeetCoreml);
    let parakeet_downloading = row_downloading(
        parakeet_own_downloading,
        cuda_downloading,
        stt_loaded,
        !stt_uses_onnx,
    );
    // Each row's ring shows ITS OWN target's fraction (downloads run in parallel) —
    // see `row_download_frac` for the model-wins-over-CUDA priority.
    let kokoro_frac = row_download_frac(
        &dl,
        DownloadTarget::KokoroModel,
        DownloadTarget::KokoroCoreml,
    );
    let parakeet_frac = row_download_frac(
        &dl,
        DownloadTarget::ParakeetModel,
        DownloadTarget::ParakeetCoreml,
    );

    let status = ModelStatus {
        // Removable only on the ONNX path (apple-native has no DontSpeak-managed Kokoro
        // files — FluidAudio self-manages its cache, mirroring the Parakeet row) AND
        // while the WARM Kokoro child isn't holding the files (the System engine doesn't
        // warm Kokoro, so the files are free even with TTS on).
        kokoro: engine_obj(
            RowState {
                present: kokoro_present,
                downloading: kokoro_downloading,
                error: kokoro_error,
                running: tts_loaded,
                enabled: kokoro_enabled,
            },
            !tts_uses_apple_native && kokoro_present && !kokoro_warm,
            kokoro_frac,
        ),
        // Parakeet STT — one engine, runtime chosen by `stt_provider`. With the ONNX
        // runtime it has downloadable model files (removable only when the warm Kokoro
        // child isn't holding the shared dylib) and shows a download ring; with
        // apple-native FluidAudio self-manages its cache (never removable, no ring).
        parakeet: engine_obj(
            RowState {
                present: parakeet_present,
                downloading: parakeet_downloading,
                error: combined_error(
                    parakeet_present,
                    if stt_uses_onnx {
                        dl_err_for(DownloadTarget::ParakeetModel)
                    } else {
                        dl_err_for(DownloadTarget::ParakeetCoreml)
                    },
                    // A mid-session `load stt`/preload failure (e.g. a transient AV-scan
                    // file-not-found on an already-downloaded model) — only surfaced as
                    // "Failed" (not "Missing") while the model is actually present.
                    tts.stt_load_error(),
                ),
                running: stt_loaded && parakeet_enabled,
                enabled: parakeet_enabled,
            },
            stt_uses_onnx && parakeet_present && !kokoro_warm,
            parakeet_frac,
        ),
        // Speaker diarization / speaker-LOCK (FluidAudio Core ML — never removable). The
        // dot tracks the speaker-LOCK feature
        // the user actually turns on: GREEN (`running`) only when `stt_speaker_lock` is on
        // AND diarization is enabled AND the models are present (the lock can actually
        // isolate the enrolled voice) — INCLUDING the SepFormer separator, which the lock
        // needs to un-mix the enrolled voice (absent ⇒ the lock silently fails open, so
        // green would lie); GREY (`idle`) when the lock is off — even though diarization
        // may be enabled under the hood for the diarize/enroll tools. Missing → the
        // Download button; orange while the shim fetches its models OR the separator
        // downloads (auto-kicked when the lock turns on).
        diarization: engine_obj(
            RowState {
                present: diar_present,
                downloading: downloading(DownloadTarget::DiarizationCoreml)
                    || downloading(DownloadTarget::SepformerModel),
                error: dl_err_for(DownloadTarget::DiarizationCoreml)
                    .or_else(|| dl_err_for(DownloadTarget::SepformerModel)),
                running: cfg.stt_speaker_lock
                    && cfg.is_diarization_on()
                    && diar_present
                    && sepformer_present,
                enabled: cfg.stt_speaker_lock,
            },
            false,
            frac_for(DownloadTarget::DiarizationCoreml)
                .max(frac_for(DownloadTarget::SepformerModel)),
        ),
        // System STT (Apple's on-device recognizer) — the OS owns the model, so
        // there's nothing for DontSpeak to remove and no download RING (no progress): never
        // `removable`, never `downloading`. But it warms like Parakeet: the state machine
        // derives "warming" (orange) from present && !running && enabled — true while the
        // en-US model is still being prepared (present but not Ready) — then "running"
        // (green) once it's installed, "missing" when selected but unavailable (macOS < 26 /
        // unsupported locale) so the dot honestly shows it can't run, no silent fallback.
        system: engine_obj(
            RowState {
                present: system_present,
                downloading: false,
                error: None,
                running: system_running,
                enabled: system_enabled,
            },
            false,
            0.0,
        ),
        // claude_code STT — Claude Code does the (cloud) transcription; nothing to download
        // or remove. "present" = CC voice on + key synthesizable; the `error` carries the
        // "run /voice" / "rebind the key" hint so the UI can tell the user how to enable it.
        claude_code: engine_obj(
            RowState {
                present: claude_code_present,
                downloading: false,
                error: claude_code_error,
                running: claude_code_running,
                enabled: claude_code_enabled,
            },
            false,
            0.0,
        ),
        // System TTS (macOS `say`) — the speech-OUT analogue of the System STT row, so the
        // adaptive TTS row can show "System" (green when selected + TTS on) instead of a
        // greyed-out Kokoro. No model to download/remove.
        tts_system: engine_obj(
            RowState {
                present: tts_system_enabled,
                downloading: false,
                error: None,
                running: tts_system_running,
                enabled: tts_system_running,
            },
            false,
            0.0,
        ),
        // The ACTIVE STT engine token, so the app's single STT row can reflect whichever
        // engine is selected (parakeet vs system) without inferring it from the dots.
        stt_engine: resolved_stt
            .map(|e| e.as_str())
            .unwrap_or("off")
            .to_string(),
        // The ACTUAL STT runtime for the built_in (Parakeet) engine, from the warm child's
        // realized `STT_PROVIDER` line — "ane"/"cuda"/"cpu" — the SAME realized-EP channel and
        // shared `realized_ort_token` mapping as `tts_provider` below, so the two rows can't drift.
        // The child already reports the honest backend (CPU/ANE fallback included). Null for
        // system/claude_code.
        stt_provider: stt_provider_token(resolved_stt, &tts.stt_realized_provider()),
        // The ACTIVE TTS engine token ("built_in" = Kokoro, "system" = `say`), so the app's
        // TTS row adapts the same way the STT row does (built_in → Kokoro, system → System).
        tts_engine: resolved_tts
            .map(|e| e.as_str())
            .unwrap_or("off")
            .to_string(),
        // The ACTUAL TTS runtime the warm Kokoro child is on, as a config-style TOKEN
        // (`ane`/`coreml`/`cuda`/`cpu`) so it matches `stt_provider`'s vocabulary
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
            // Exactly derivable from the SAME two locals independently serialized above as
            // `running.parakeet`/`running.system` — extracted to `dictation_local_stt` so
            // that OR relationship is pinned by a test and a future edit to either row can't
            // silently desync them (see `tests::local_stt_matches_running_flags`).
            local_stt: dictation_local_stt(parakeet_running, system_running),
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
            // A dictation START was just refused (engine enabled but can't transcribe yet —
            // model missing/downloading/loading). Each overlay shows the panel washed in
            // the SAME warning glow as `has_paste_target == false` for the refusal window,
            // so a Caps tap on a fresh install is never a silent no-op.
            refused: dict_refused,
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
                tts: tts_loaded,
                stt: stt_loaded,
            },
            // Diarization stats for the Settings row's expansion: enabled, model presence,
            // the enrolled voiceprint names (so the row can show "who it recognizes"), and
            // the live thresholds. Lives UNDER `stats` (where the app's EngineStats.parse
            // reads it) — NOT at the root, where it would collide with the diarization
            // engine_obj dot below and clobber its `state` (so the dot never goes green).
            // On-demand, so there's no realtime-factor like STT/TTS.
            diarization: DiarStats {
                enabled: cfg.is_diarization_on(),
                present: diar_present,
                // The resolved diarizer runtime in the SAME token vocabulary as
                // tts_provider/stt_provider, so the row's "Runtime" line reuses runtimeLabel
                // (the single apple_native rung is Core ML / ANE → "ane"). On-demand, so no
                // realtime factor.
                runtime: match cfg.resolved_diarizer_provider() {
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

/// Relabel a warm-child REALIZED-provider wire token to the config [`Provider`](ds_config::Provider)
/// the status row shows, through the SHARED [`RealizedProvider`](ds_config::RealizedProvider) — the
/// ONE realized-EP vocabulary BOTH engines PRODUCE (as a typed enum) and this row CONSUMES, so a
/// token typo is a compile error and the two rows can't drift into different labels for the same
/// runtime (drift guard: [`tests::tts_and_stt_report_the_same_realized_runtime`]).
fn realized_ort_token(child_provider: &str) -> ds_config::Provider {
    ds_config::RealizedProvider::parse(child_provider).to_provider()
}

/// The STT runtime TOKEN the UI shows — the REALIZED EP the warm child reports on its `STT_PROVIDER`
/// line (what the Parakeet sessions ACTUALLY loaded on, CPU fallback included), mapped through the
/// SAME [`realized_ort_token`] as TTS. No longer a preference gated on `cuda_present`/`shim_ok`: the
/// child already reports the honest backend (it falls back to the ONNX CPU path when the ANE shim /
/// GPU runtime is absent), so this just relabels it. `None` for non-built_in engines
/// (claude_code/system/off have no local Parakeet ort runtime).
fn stt_provider_token(
    resolved_stt: Option<ds_config::SttEngine>,
    child_provider: &str,
) -> Option<String> {
    match resolved_stt {
        Some(ds_config::SttEngine::BuiltIn) => {
            Some(realized_ort_token(child_provider).as_str().to_string())
        }
        _ => None,
    }
}

/// The TTS runtime TOKEN the UI shows — the REALIZED EP the warm Kokoro child reports on its
/// `PROVIDER` line (`"CoreML-ANE"`/`"CoreML"`/`"CUDA"`/`"CPU"`), i.e. what ACTUALLY loaded (CPU
/// fallback included), mapped through the SAME [`realized_ort_token`] as STT. `None` for the System
/// (`say`) / Off engines (no Kokoro runtime).
fn tts_provider_token(
    resolved_tts: Option<ds_config::TtsEngine>,
    child_provider: &str,
) -> Option<String> {
    match resolved_tts {
        Some(ds_config::TtsEngine::Kokoro) => {
            Some(realized_ort_token(child_provider).as_str().to_string())
        }
        _ => None,
    }
}

/// Whether FluidAudio's speaker-diarization Core ML models are on disk in our `coreml_dir`.
/// Uses the SAME completion-marker check the downloader writes (`is_coreml_repo_present`), so the
/// status row and the downloader can never disagree about one location — a partial/aborted
/// fetch (subdir exists, no `.ds-ready` marker) reads MISSING here exactly as it does to the
/// downloader, instead of the old substring heuristic that called a half-download "present".
fn diarization_present() -> bool {
    ds_model::coreml_repo::is_coreml_repo_present(&ds_model::coreml_repo::DIARIZATION_COREML)
}

/// Whether a model row reads "downloading", given its OWN in-flight signal
/// (`own_downloading` — the row's ONNX model fetch or apple-native Core ML set) plus the
/// shared CUDA runtime's state. `cuda_downloading` alone can ONLY force the row into
/// "downloading" while the row's own engine has NOT yet loaded (`!engine_loaded`) — that's
/// the shared GPU runtime's first-boot behavior described above (fetched ahead of the model,
/// neither ONNX engine can run until it lands). Once the row's engine IS loaded (resident +
/// warm), a Cuda fetch that's still running for some OTHER reason (e.g. re-verifying, or
/// serving the other engine) must NOT flip an already-working row back to "downloading" —
/// that was the regression this guards (a fully-loaded, working Kokoro flashing from a full
/// ring back to ~25%). Never applies on the apple-native path (`uses_apple_native`): the
/// shared runtime doesn't exist there and `Cuda` is never a download target. Pure, so the
/// gate is unit-tested directly (see [`tests::row_downloading_gates_cuda_on_engine_loaded`]).
fn row_downloading(
    own_downloading: bool,
    cuda_downloading: bool,
    engine_loaded: bool,
    uses_apple_native: bool,
) -> bool {
    own_downloading || (!uses_apple_native && cuda_downloading && !engine_loaded)
}

/// Combine a model row's DOWNLOAD-manager error with its (mid-session re/load) failure,
/// gated on PRESENCE for the load half only — a genuinely absent/never-installed model must
/// read "missing" (offer Download), never a stale "failed", even if a load-error slot happens
/// to still hold a message from a previous install (`TtsManager`'s per-model `stt_load_error`/
/// `tts_load_error` are cleared on the model's own residency transitions, but this keeps the
/// row itself defensive against any ordering this misses). A download error, by contrast,
/// surfaces UNCONDITIONALLY — mirrors the pre-existing `kokoro_error` ordering, where a failed
/// fetch is shown even though the model isn't present (that IS the interesting state: "tried to
/// get it, couldn't"). Pure, mirroring `row_downloading`/`row_download_frac` — extracted so this
/// precedence is unit-tested directly rather than needing a full `EngineShared`/`TtsManager`
/// harness (see `local_stt_matches_running_flags`'s doc for why that's disproportionate here).
fn combined_error(
    present: bool,
    dl_err: Option<String>,
    load_err: Option<String>,
) -> Option<String> {
    dl_err.or(if present { load_err } else { None })
}

/// The fraction a MODEL row's download ring shows, picked from the row's OWN targets in
/// priority order: the ONNX model fetch's live progress, then its just-finished progress
/// (so a completed fetch's ring reads 100% rather than snapping back to 0 the instant it
/// retires into `Done`), then the apple-native Core ML set the same way, then the shared
/// CUDA runtime's LIVE progress (the compute dependency — shown only when it is the sole
/// fetch blocking the row; a concurrent model fetch wins). 0.0 when none of the above apply —
/// the caller's `engine_obj` zeroes progress anyway unless the row reads "downloading". Pure,
/// so the row↔target wiring is unit-tested.
fn row_download_frac(
    targets: &std::collections::HashMap<DownloadTarget, TargetState>,
    model: DownloadTarget,
    coreml: DownloadTarget,
) -> f64 {
    [model, coreml]
        .iter()
        .find_map(|t| match targets.get(t) {
            // A row's OWN target feeds its ring whether live or just finished;
            // Failed feeds nothing (the error channel covers it).
            Some(TargetState::Active(p) | TargetState::Done(p)) => Some(p.frac()),
            _ => None,
        })
        .or_else(|| match targets.get(&DownloadTarget::Cuda) {
            // The Cuda fallback is LIVE-only: a finished Cuda fetch must not keep
            // feeding rows' rings after it ends.
            Some(TargetState::Active(p)) => Some(p.frac()),
            _ => None,
        })
        .unwrap_or(0.0)
}

/// The five same-shaped flags every model row feeds into [`engine_obj`], bundled into one
/// struct instead of five positional args. Before this, `engine_obj` took `present`, `dling`,
/// `error`, `running`, `enabled` as separate positional `bool`/`Option<String>` params — with
/// the `tts_system` call site passing the SAME variable for both `running` and `enabled`, easy
/// to transpose by accident. `removable`/`progress` stay as separate args to `engine_obj`
/// since they aren't part of this row-identity cluster (e.g. `removable` also depends on
/// whether another engine is holding a shared dylib).
struct RowState {
    present: bool,
    downloading: bool,
    error: Option<String>,
    running: bool,
    enabled: bool,
}

/// Build one engine row with a lifecycle `state` (the app maps it 1:1 to a status dot):
/// downloading > failed > missing > running > warming > idle. Internal to
/// `dontspeakd::status` only — does not touch [`ds_status::EngineObj`]'s shape or the
/// serialized JSON.
fn engine_obj(row: RowState, removable: bool, progress: f64) -> EngineObj {
    let state = engine_state(
        row.present,
        row.downloading,
        row.error.is_some(),
        row.running,
        row.enabled,
    );
    EngineObj {
        present: row.present,
        removable,
        state: state.as_str().to_string(),
        progress: if row.downloading { progress } else { 0.0 },
        error: row.error,
    }
}

/// `Dictation.local_stt`: true whenever EITHER local-transcript STT engine (Parakeet or
/// System) is the one actually running — the confirm panel should appear whenever a local
/// engine is producing a transcript, not just for Parakeet. This is exactly the same OR of
/// the same two locals independently serialized a few lines away as `Running.parakeet` /
/// `Running.system`; extracted into its own PURE function (rather than inlined at both call
/// sites) so the relationship is pinned by a test
/// ([`tests::local_stt_matches_running_flags`]) and a future edit to either row can't
/// silently desync them.
fn dictation_local_stt(parakeet_running: bool, system_running: bool) -> bool {
    parakeet_running || system_running
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
    use super::{
        StatusGate, combined_error, dictation_local_stt, engine_state, realized_ort_token,
        row_download_frac, row_downloading, stt_provider_token, tts_provider_token,
    };
    use ds_config::{Provider, SttEngine, TtsEngine};
    use ds_status::EngineState;
    use std::thread;
    use std::time::{Duration, Instant};

    /// The gate's two real transitions, exercised directly (no test anywhere else
    /// constructs a `StatusGate` and calls `wait_changed`):
    /// (1) fast path — the state already advanced past `since`, returns immediately;
    /// (2) blocking path — nothing changes, so the call blocks for the full timeout and
    ///     returns the unchanged seq;
    /// (3) a `bump()` from another thread wakes the waiter promptly, well before its
    ///     (long) timeout — the whole point of the condvar over a poll loop.
    #[test]
    fn wait_changed_fast_path_blocking_wait_and_cross_thread_wakeup() {
        let gate = StatusGate::new();

        // (1) Fast path: bump happened before the caller even asked, so `since = 0`
        // (never observed) is already stale — returns immediately with the new seq.
        gate.bump();
        let seq_after_bump = gate.seq();
        assert_eq!(
            gate.wait_changed(0, Duration::from_millis(1)),
            seq_after_bump
        );

        // (2) Blocking path: `since` is already current and nothing bumps again, so the
        // call must ride out the full timeout and report the unchanged seq.
        let since = gate.seq();
        let start = Instant::now();
        let result = gate.wait_changed(since, Duration::from_millis(20));
        assert!(
            start.elapsed() >= Duration::from_millis(20),
            "should have blocked for the full timeout, took {:?}",
            start.elapsed()
        );
        assert_eq!(result, since, "no bump landed, so seq is unchanged");

        // (3) Cross-thread wakeup: a bump from another thread must wake the waiter well
        // before its 5s timeout, returning the freshly-bumped seq.
        let since = gate.seq();
        let gate2 = gate.clone();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            gate2.bump();
        });
        let wait_start = Instant::now();
        let woken = gate.wait_changed(since, Duration::from_secs(5));
        assert!(
            wait_start.elapsed() < Duration::from_secs(1),
            "bump should wake the waiter promptly, not time out; took {:?}",
            wait_start.elapsed()
        );
        assert_eq!(woken, since.wrapping_add(1));
        handle.join().unwrap();
    }

    /// Each model row picks ITS OWN target's fraction from the parallel-download map:
    /// the model fetch wins over the Core ML set, which wins over the shared CUDA
    /// runtime; a foreign target's progress never bleeds into the row; idle rows read 0.
    /// Also: a just-FINISHED target's `Done` progress wins over the (unrelated) live Cuda
    /// fetch, so a completed row's ring reads its final % instead of snapping to the
    /// runtime's — the fix for the ring falling back down after a finished download.
    #[test]
    fn row_download_frac_picks_the_rows_own_target() {
        use crate::downloads::{DownloadProgress, TargetState};
        use ds_model::DownloadTarget::*;
        use std::collections::HashMap;
        let p = |done, total| DownloadProgress { done, total };
        let empty: HashMap<_, _> = HashMap::new();

        // Kokoro model + Parakeet model + CUDA all in flight at once, distinct fractions.
        let active: HashMap<_, _> = [
            (KokoroModel, TargetState::Active(p(10, 100))), // 0.10
            (ParakeetModel, TargetState::Active(p(30, 100))), // 0.30
            (Cuda, TargetState::Active(p(90, 100))),        // 0.90
        ]
        .into_iter()
        .collect();
        // Each row shows its OWN model %, not the other row's and not CUDA's.
        assert_eq!(row_download_frac(&active, KokoroModel, KokoroCoreml), 0.10);
        assert_eq!(
            row_download_frac(&active, ParakeetModel, ParakeetCoreml),
            0.30
        );

        // Only CUDA in flight (models present): both rows show the runtime's %.
        let cuda_only: HashMap<_, _> = [(Cuda, TargetState::Active(p(50, 100)))]
            .into_iter()
            .collect();
        assert_eq!(
            row_download_frac(&cuda_only, KokoroModel, KokoroCoreml),
            0.5
        );
        assert_eq!(
            row_download_frac(&cuda_only, ParakeetModel, ParakeetCoreml),
            0.5
        );

        // Core ML flavor in flight → the row's second-priority target.
        let coreml: HashMap<_, _> = [(KokoroCoreml, TargetState::Active(p(25, 100)))]
            .into_iter()
            .collect();
        assert_eq!(row_download_frac(&coreml, KokoroModel, KokoroCoreml), 0.25);
        // ...and it does NOT bleed into the Parakeet row.
        assert_eq!(
            row_download_frac(&coreml, ParakeetModel, ParakeetCoreml),
            0.0
        );

        // Nothing in flight ⇒ 0.
        assert_eq!(row_download_frac(&empty, KokoroModel, KokoroCoreml), 0.0);

        // THE FIX: Kokoro's own fetch just finished (retired into `Done`) while Cuda is
        // STILL live for some unrelated reason — the row must show its own finished %,
        // never fall back to Cuda's, and must NOT bleed into the Parakeet row (which has
        // nothing of its own, so it falls through to Cuda).
        let kokoro_done_cuda_live: HashMap<_, _> = [
            (KokoroModel, TargetState::Done(p(100, 100))),
            (Cuda, TargetState::Active(p(50, 100))),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            row_download_frac(&kokoro_done_cuda_live, KokoroModel, KokoroCoreml),
            1.0,
            "a finished own-target download wins over a still-live Cuda fetch"
        );
        assert_eq!(
            row_download_frac(&kokoro_done_cuda_live, ParakeetModel, ParakeetCoreml),
            0.5,
            "Parakeet has nothing of its own, so it still falls back to Cuda"
        );

        // The Cuda fallback is LIVE-only: a FINISHED Cuda entry must not keep feeding
        // rows' rings (today's `active`-map-only read, pinned).
        let cuda_done: HashMap<_, _> = [(Cuda, TargetState::Done(p(100, 100)))]
            .into_iter()
            .collect();
        assert_eq!(
            row_download_frac(&cuda_done, KokoroModel, KokoroCoreml),
            0.0,
            "a Done Cuda entry must not feed the fallback"
        );
    }

    /// THE REGRESSION GUARD for the Kokoro-status bug: a fully-loaded, working row must
    /// never read "downloading" just because the shared Cuda runtime is (still, or again)
    /// in flight for some unrelated reason. Cuda alone can only force "downloading" while
    /// the row's own engine has NOT yet loaded — the exact first-boot intent from the
    /// comment above `row_downloading`'s call sites.
    #[test]
    fn row_downloading_gates_cuda_on_engine_loaded() {
        // THE BUG: Cuda still downloading, the row's OWN target is NOT downloading, but the
        // row's engine has ALREADY loaded (fully downloaded, verified, warm, working) ⇒ the
        // row must read NOT downloading — Cuda mustn't flash it back to a progress ring.
        assert!(
            !row_downloading(false, true, true, false),
            "an already-loaded engine must not be forced back into 'downloading' by Cuda"
        );

        // First-boot intent (preserved): engine not yet loaded + Cuda downloading ⇒ the row
        // DOES read downloading (the shared runtime gates both ONNX engines before it lands).
        assert!(
            row_downloading(false, true, false, false),
            "on first boot, Cuda-in-flight must still gate a not-yet-loaded ONNX row"
        );

        // The row's OWN target downloading always wins, regardless of Cuda or load state.
        assert!(row_downloading(true, false, true, false));
        assert!(row_downloading(true, true, true, false));

        // Nothing downloading at all ⇒ false.
        assert!(!row_downloading(false, false, false, false));
        assert!(!row_downloading(false, false, true, false));

        // The apple-native path never lets Cuda gate the row (the shared runtime doesn't
        // exist there and `Cuda` is never a download target on that path) — own-target
        // still wins, but Cuda-in-flight alone never does, loaded or not.
        assert!(!row_downloading(false, true, false, true));
        assert!(!row_downloading(false, true, true, true));
        assert!(row_downloading(true, true, false, true));
    }

    #[test]
    fn tts_and_stt_report_the_same_realized_runtime() {
        // DRIFT GUARD: both status rows relabel the child's realized-provider string through the ONE
        // shared `realized_ort_token`, so for the SAME reported runtime they MUST yield the SAME
        // token. TTS and STT can never drift into different labels for the same EP — the whole point
        // of routing both through one mapper (and one `ds_model::cuda_session_builder`).
        let k = Some(TtsEngine::Kokoro);
        let b = Some(SttEngine::BuiltIn);
        for realized in ["CUDA", "CPU", "CoreML-ANE", "CoreML", "System", "surprise"] {
            assert_eq!(
                tts_provider_token(k, realized),
                stt_provider_token(b, realized),
                "TTS and STT must map realized `{realized}` to the SAME token"
            );
        }
    }

    #[test]
    fn provider_tokens_reflect_the_realized_runtime() {
        // The token is the REALIZED EP the child reports, not a preference — CPU fallback included.
        let k = Some(TtsEngine::Kokoro);
        let b = Some(SttEngine::BuiltIn);
        assert_eq!(tts_provider_token(k, "CUDA").as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, "CUDA").as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, "CPU").as_deref(), Some("cpu"));
        assert_eq!(stt_provider_token(b, "CoreML-ANE").as_deref(), Some("ane"));
        // Anything unrecognized (or "System") is CPU, never a wrong GPU claim.
        assert_eq!(stt_provider_token(b, "System").as_deref(), Some("cpu"));
        // The shared mapper's own table.
        assert_eq!(realized_ort_token("CUDA"), Provider::OrtCuda);
        assert_eq!(realized_ort_token("CoreML"), Provider::OrtCoreMl);
        assert_eq!(realized_ort_token("nonsense"), Provider::OrtCpu);
        // No local runtime token for the delegate/OS engines or when the engine is off.
        assert_eq!(
            stt_provider_token(Some(SttEngine::ClaudeCode), "CUDA"),
            None
        );
        assert_eq!(stt_provider_token(None, "CUDA"), None);
        assert_eq!(tts_provider_token(Some(TtsEngine::System), "CUDA"), None);
        assert_eq!(tts_provider_token(None, "CUDA"), None);
    }

    #[test]
    fn engine_state_precedence_table() {
        // The model lifecycle the app maps to a dot. `dling` comes from the download
        // manager's active target, `running` from tts_loaded/stt_loaded (set only after the
        // model is resident + warm) — so on a clean install: downloading ⇒ orange
        // "Downloading…", then (briefly) warming ⇒ "Starting…", then running ⇒ green. Never
        // green mid-fetch.
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
        // The regression guard: an active download forces "downloading" even if the
        // present/running flags say otherwise (e.g. a non-empty partial dir on disk).
        assert_eq!(
            engine_state(true, true, false, false, true),
            EngineState::Downloading
        );
    }

    /// `combined_error`'s presence-gating: a model row's DOWNLOAD-manager error always
    /// surfaces, but a LOAD-error only counts while the model is actually present — a
    /// since-removed/never-installed model must read "missing", never a stale "failed".
    #[test]
    fn combined_error_gates_the_load_error_on_presence_but_not_the_download_error() {
        // present + load_err (no dl_err) → Some(load_err).
        assert_eq!(
            combined_error(true, None, Some("load boom".to_string())),
            Some("load boom".to_string())
        );
        // present + dl_err + no load_err → Some(dl_err).
        assert_eq!(
            combined_error(true, Some("dl boom".to_string()), None),
            Some("dl boom".to_string())
        );
        // present + BOTH → the download error wins (mirrors the pre-existing kokoro_error
        // ordering: dl_err_for(...) is checked before the load-error fallback).
        assert_eq!(
            combined_error(
                true,
                Some("dl boom".to_string()),
                Some("load boom".to_string())
            ),
            Some("dl boom".to_string())
        );
        // !present + load_err (no dl_err) → None: don't show a stale failure for a
        // since-removed/never-installed model.
        assert_eq!(
            combined_error(false, None, Some("load boom".to_string())),
            None
        );
        // !present + dl_err → Some(dl_err): a download failure surfaces even though the
        // model never landed (that IS the interesting state).
        assert_eq!(
            combined_error(false, Some("dl boom".to_string()), None),
            Some("dl boom".to_string())
        );
        // neither → None.
        assert_eq!(combined_error(true, None, None), None);
        assert_eq!(combined_error(false, None, None), None);
    }

    /// Pins `dictation_local_stt`'s own OR definition — NOT an end-to-end check of
    /// `model_status_json` itself (that would need a full `EngineShared`/`TtsManager` test
    /// harness, disproportionate for what's currently just two identically-named,
    /// single-assignment locals). The real invariant — `Dictation.local_stt` and
    /// `Running.parakeet`/`Running.system` a few lines above it in `model_status_json`
    /// both read the SAME `parakeet_running`/`system_running` bindings — currently holds
    /// by construction (grep this file for those names before touching either call site),
    /// not because this test would catch a divergence between them.
    #[test]
    fn local_stt_matches_running_flags() {
        for parakeet_running in [false, true] {
            for system_running in [false, true] {
                assert_eq!(
                    dictation_local_stt(parakeet_running, system_running),
                    parakeet_running || system_running,
                    "local_stt must equal running.parakeet || running.system \
                     (parakeet_running={parakeet_running}, system_running={system_running})"
                );
            }
        }
    }
}
