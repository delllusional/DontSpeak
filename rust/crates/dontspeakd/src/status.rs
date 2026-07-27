//! `model_status` aggregator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ds_config::{Paths, VoiceConfig};

use crate::config_gate::{
    NativeShims, caps_loop_enabled, native_tts_active, parakeet_available,
    parakeet_onnx_files_present, stt_uses_onnx_runtime, tts_model_files_present,
};
use crate::dictation_presenter::DictationPresenterRegistry;
use crate::downloads::{DownloadProg, TargetState};
use crate::engine::{PasteState, dictation_preview};
use crate::stats;
use crate::tts::TtsManager;
use ds_model::DownloadTarget;
use ds_status::{
    Activity, DiarizationStatus, Dictation, DictationState, DownloadStatus, EngineState,
    EngineStatus, ModelStatus, Stats, StatusSttEngine, StatusTrayKind, StatusTtsEngine,
    StatusTtsModel, SttStatus, TtsSnapshot, TtsStatus,
};

/// Seq + condvar for `WaitModelStatus` (bump on every status flip).
pub(crate) struct StatusGate {
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

    /// Bump seq; wake waiters.
    pub(crate) fn bump(&self) {
        let mut s = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        *s = s.wrapping_add(1);
        self.cv.notify_all();
    }

    /// Hold seq lock after a flag write (queue race tests).
    #[cfg(test)]
    pub(crate) fn hold_transition_for_test(&self) -> std::sync::MutexGuard<'_, u64> {
        self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Current seq (app echoes as `since` on next wait).
    pub(crate) fn seq(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Seq first, then read (mid-read transitions stay unacked).
    pub(crate) fn snapshot<T>(&self, read: impl FnOnce() -> T) -> (T, u64) {
        let seq = self.seq();
        (read(), seq)
    }

    /// Block until seq ≠ `since` or timeout. Immediate if already advanced.
    pub(crate) fn wait_changed(&self, since: u64, timeout: Duration) -> u64 {
        let guard = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        if *guard != since {
            return *guard;
        }
        // Predicate + bump share `seq` mutex — no lost-wakeup window.
        let (guard, _) = self
            .cv
            .wait_timeout_while(guard, timeout, |s| *s == since)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

/// Shared Arcs for IPC + status (`engine_run`).
#[derive(Clone)]
pub(crate) struct EngineShared {
    pub tts: Arc<TtsManager>,
    pub caps_active: Arc<AtomicBool>,
    pub stt_active: Arc<AtomicBool>,
    pub paste: PasteState,
    pub downloads: DownloadProg,
    pub tts_stats: Arc<stats::TtsStats>,
    pub stt_stats: Arc<stats::SttStats>,
    pub lifetime: Arc<stats::LifetimeSeconds>,
    pub gate: Arc<StatusGate>,
    pub dictation_presenters: Arc<DictationPresenterRegistry>,
}

struct DictationPresentation {
    text: String,
    awaiting: bool,
    can_paste: bool,
    refused: bool,
    recording: bool,
    session_generation: u64,
}

fn dictation_presentation(
    stt_active: &AtomicBool,
    paste: &PasteState,
    now: Instant,
) -> DictationPresentation {
    // Producers publish the paste buffer and generation before setting recording.
    // Read the flag first so a snapshot cannot pair the prior generation with a
    // newly-published recording edge.
    let recording = stt_active.load(Ordering::SeqCst);
    paste
        .lock()
        .map(|p| {
            let (text, awaiting) = dictation_preview(&p.final_state, &p.partial, p.caps_held);
            DictationPresentation {
                text,
                awaiting,
                can_paste: p.can_paste,
                refused: crate::engine::refusal_live(p.refused_until, now),
                recording,
                session_generation: p.presentation_session_id,
            }
        })
        .unwrap_or(DictationPresentation {
            text: String::new(),
            awaiting: false,
            can_paste: true,
            refused: false,
            recording,
            session_generation: 0,
        })
}

pub(crate) fn current_dictation_session_id(
    presenters: &DictationPresenterRegistry,
    stt_active: &AtomicBool,
    paste: &PasteState,
) -> Option<String> {
    let presentation = dictation_presentation(stt_active, paste, Instant::now());
    (presentation.recording || presentation.awaiting || presentation.refused)
        .then(|| presenters.session_id(presentation.session_generation))
}

/// Model presence report. `read_tts` under `gate.snapshot` (mid-report transitions unacked).
pub(crate) fn model_status_json(
    shared: &EngineShared,
    paths: &Paths,
    read_tts: impl FnOnce() -> crate::ttsq::TtsStatusSample,
) -> serde_json::Value {
    let (tts_sample, seq) = shared.gate.snapshot(read_tts);
    let EngineShared {
        tts,
        caps_active,
        stt_active,
        paste,
        downloads,
        tts_stats,
        stt_stats,
        lifetime,
        gate,
        dictation_presenters,
    } = shared;
    let cfg = VoiceConfig::load(paths);
    let resolved_tts = cfg.resolved_tts();
    let resolved_stt = cfg.resolved_stt();
    // Pin markers keep polled model presence cheap after one verified state.
    let shims = NativeShims::probe().unwrap_or_default();
    let tts_uses_native = native_tts_active(&cfg);
    let tts_present = tts_model_files_present(&cfg);
    let parakeet_onnx_files = parakeet_onnx_files_present();
    let stt_uses_onnx = stt_uses_onnx_runtime(cfg.resolved_stt_provider(), shims);
    let parakeet_present = if stt_uses_onnx {
        parakeet_onnx_files
    } else {
        parakeet_available()
    };
    let parakeet_enabled = resolved_stt == Some(ds_config::SttEngine::BuiltIn);
    let parakeet_running = parakeet_enabled && parakeet_present;
    let system_enabled = resolved_stt == Some(ds_config::SttEngine::System);
    // Probe only when selected.
    let system_state = if system_enabled {
        ds_stt::system_state()
    } else {
        ds_stt::SystemState::Unavailable
    };
    let system_present = system_enabled && system_state != ds_stt::SystemState::Unavailable;
    let system_running = system_state == ds_stt::SystemState::Ready;

    // claude_code: present = voice on + synthesizable key.
    let claude_code_enabled = resolved_stt == Some(ds_config::SttEngine::ClaudeCode);
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

    // Confirm panel: finalized while awaiting, else partial; never finalized under Caps hold.
    let dictation_presentation = dictation_presentation(stt_active, paste, Instant::now());

    let (dl, download_transfer_start) = {
        let downloads = downloads.lock().unwrap_or_else(|e| e.into_inner());
        (downloads.targets.clone(), downloads.transfer_start.clone())
    };
    let download_statuses = active_download_statuses(&dl, &download_transfer_start, Instant::now());
    let downloading = |eng: DownloadTarget| matches!(dl.get(&eng), Some(TargetState::Active(_)));
    // Active-only (Done % via row_download_frac).
    let frac_for = |eng: DownloadTarget| match dl.get(&eng) {
        Some(TargetState::Active(p)) => p.frac(),
        _ => 0.0,
    };
    let dl_err_for = |eng: DownloadTarget| match dl.get(&eng) {
        Some(TargetState::Failed(e)) => Some(e.clone()),
        _ => None,
    };
    let tts_enabled = resolved_tts == Some(ds_config::TtsEngine::BuiltIn);
    let tts_targets = tts_download_targets(&cfg);
    let tts_download_error = tts_targets.iter().find_map(|target| dl_err_for(*target));
    let tts_error = combined_error(
        tts_present,
        tts_download_error,
        tts.tts_load_error().or_else(|| tts.last_error()),
    );

    let tts_system_enabled = resolved_tts == Some(ds_config::TtsEngine::System);
    let tts_system_running = tts_system_enabled;

    let diar_present = ds_model::ModelRoots::ambient()
        .is_some_and(|roots| diarization_present(&roots, cfg.resolved_diarizer()));
    // Exists only — sha at download.
    let sepformer_present = ds_model::model_path(ds_model::SEPFORMER_FILE)
        .map(|p| p.is_file())
        .unwrap_or(false);

    // GREEN = loaded. Cuda keeps ONNX rows downloading until load (no flash-back ring).
    let tts_loaded = tts.is_tts_loaded();
    let stt_loaded = tts.is_stt_loaded();
    let cuda_downloading = downloading(DownloadTarget::Cuda);
    let tts_own_downloading = tts_targets.iter().any(|target| downloading(*target));
    let tts_downloading = row_downloading(
        tts_own_downloading,
        cuda_downloading && cfg.resolved_tts_provider() == ds_config::Provider::OrtCuda,
        tts_loaded,
        tts_uses_native,
    );
    let parakeet_own_downloading = (stt_uses_onnx && downloading(DownloadTarget::ParakeetModel))
        || downloading(DownloadTarget::ParakeetMlx);
    let parakeet_downloading = row_downloading(
        parakeet_own_downloading,
        cuda_downloading,
        stt_loaded,
        !stt_uses_onnx,
    );
    let tts_frac = row_download_frac(&dl, &tts_targets);
    let parakeet_frac = row_download_frac(
        &dl,
        &[DownloadTarget::ParakeetModel, DownloadTarget::ParakeetMlx],
    );

    let dict_recording = dictation_presentation.recording;
    let dict_local = dictation_local_stt(parakeet_running, system_running);

    let tts_status = match resolved_tts {
        Some(ds_config::TtsEngine::BuiltIn) => Some(engine_status(
            RowState {
                present: tts_present,
                downloading: tts_downloading,
                error: tts_error,
                running: tts_loaded,
                enabled: tts_enabled,
            },
            tts_frac,
        )),
        Some(ds_config::TtsEngine::System) => Some(engine_status(
            RowState {
                present: tts_system_enabled,
                downloading: false,
                error: None,
                running: tts_system_running,
                enabled: tts_system_running,
            },
            0.0,
        )),
        None => None,
    };

    let stt_status = match resolved_stt {
        Some(ds_config::SttEngine::BuiltIn) => Some(engine_status(
            RowState {
                present: parakeet_present,
                downloading: parakeet_downloading,
                error: combined_error(
                    parakeet_present,
                    if stt_uses_onnx {
                        dl_err_for(DownloadTarget::ParakeetModel)
                    } else {
                        dl_err_for(DownloadTarget::ParakeetMlx)
                    },
                    tts.stt_load_error(),
                ),
                running: stt_loaded && parakeet_enabled,
                enabled: parakeet_enabled,
            },
            parakeet_frac,
        )),
        Some(ds_config::SttEngine::System) => Some(engine_status(
            RowState {
                present: system_present,
                downloading: false,
                error: None,
                running: system_running,
                enabled: system_enabled,
            },
            0.0,
        )),
        Some(ds_config::SttEngine::ClaudeCode) => Some(engine_status(
            RowState {
                present: claude_code_present,
                downloading: false,
                error: claude_code_error,
                running: claude_code_running,
                enabled: claude_code_enabled,
            },
            0.0,
        )),
        None => None,
    };

    // Running needs diarization + SepFormer.
    let diarization_status = engine_status(
        RowState {
            present: diar_present,
            downloading: downloading(DownloadTarget::DiarizationMlx)
                || downloading(DownloadTarget::SepformerModel),
            error: dl_err_for(DownloadTarget::DiarizationMlx)
                .or_else(|| dl_err_for(DownloadTarget::SepformerModel)),
            running: cfg.speaker_lock
                && cfg.is_diarization_on()
                && diar_present
                && sepformer_present,
            enabled: cfg.speaker_lock,
        },
        frac_for(DownloadTarget::DiarizationMlx).max(frac_for(DownloadTarget::SepformerModel)),
    );
    let diar_shim = match cfg.resolved_diarizer() {
        ds_config::DiarizerProvider::Mlx => shims.mlx,
        ds_config::DiarizerProvider::Fluid => shims.fluid,
    };
    let diarization_provider = diarization_provider_token(
        cfg.resolved_diarizer(),
        cfg.is_diarization_on(),
        diar_present && diar_shim,
    );

    let dictation_state = dictation_state(
        dict_recording,
        dictation_presentation.awaiting,
        dict_local,
        dictation_presentation.refused,
    );
    let dictation_session_id = dictation_session_id(
        dictation_presenters,
        dictation_state,
        dictation_presentation.session_generation,
    );
    let (external_ui_active, presenter_changed) =
        dictation_presenters.external_ui_active(dictation_session_id.as_deref(), Instant::now());
    if presenter_changed {
        gate.bump();
    }

    let status = ModelStatus {
        seq,
        activity: Activity {
            caps: caps_loop_enabled(&cfg),
            caps_active: caps_active.load(Ordering::Relaxed),
            recording: dict_recording,
            speaking: tts_sample.speaking,
            speaker: tts_sample.speaker,
            utterance_id: tts_sample.utterance.as_ref().map(|utterance| utterance.id),
            playback_state: tts_sample.playback_state,
            playback_hold_reason: tts_sample.playback_hold_reason,
            voice: tts_sample
                .utterance
                .as_ref()
                .and_then(|utterance| utterance.voice.clone()),
            language: tts_sample
                .utterance
                .as_ref()
                .and_then(|utterance| utterance.language.clone()),
            warning: tts_sample
                .utterance
                .as_ref()
                .and_then(|utterance| utterance.warning),
            muted: tts.is_muted(),
        },
        voice_sessions: tts_sample.voice_sessions,
        tts: TtsStatus {
            engine: status_tts_engine(resolved_tts),
            model: (resolved_tts == Some(ds_config::TtsEngine::BuiltIn))
                .then(|| status_tts_model(cfg.tts_model)),
            language: match resolved_tts {
                // Built-in scope: null while auto-detecting; the joined preferred codes otherwise.
                Some(ds_config::TtsEngine::BuiltIn) if cfg.preferred_languages.is_empty() => None,
                Some(ds_config::TtsEngine::BuiltIn) => Some(cfg.preferred_languages.join(",")),
                _ => None,
            },
            provider: tts_provider_token(resolved_tts, tts.provider().as_deref()),
            status: tts_status,
            recent_utterances: tts_sample.recent_utterances,
        },
        stt: SttStatus {
            engine: status_stt_engine(resolved_stt),
            provider: stt_provider_token(resolved_stt, tts.stt_realized_provider().as_deref()),
            status: stt_status,
            voice_key: claude_code_key,
        },
        diarization: DiarizationStatus {
            status: diarization_status,
            enabled: cfg.is_diarization_on(),
            provider: diarization_provider,
            speakers: ds_config::SpeakerStore::load(&paths.speakers_json).names(),
            activity_threshold: cfg.activity_threshold as f64,
        },
        dictation: Dictation {
            state: dictation_state,
            text: dictation_presentation.text,
            can_paste: dictation_presentation.can_paste,
            session_id: dictation_session_id,
            external_ui_active,
        },
        stats: Stats {
            // Queue depth under gate.snapshot, not the stats accumulator.
            tts: TtsSnapshot {
                queued: tts_sample.queued,
                ..tts_stats.snapshot()
            },
            stt: stt_stats.snapshot(),
            lifetime: lifetime.snapshot(),
        },
        tray: cfg.tray.iter().copied().map(status_tray_kind).collect(),
        downloads: download_statuses,
        agents: cfg.agents,
    };
    serde_json::to_value(status).unwrap_or(serde_json::Value::Null)
}

fn active_download_statuses(
    targets: &std::collections::HashMap<DownloadTarget, TargetState>,
    transfer_start: &std::collections::HashMap<DownloadTarget, (Instant, u64)>,
    now: Instant,
) -> Vec<DownloadStatus> {
    let mut statuses: Vec<_> = targets
        .iter()
        .filter_map(|(target, state)| {
            let TargetState::Active(progress) = state else {
                return None;
            };
            let (start_bytes, elapsed_seconds) = transfer_start
                .get(target)
                .map(|(started, start_done)| {
                    (
                        *start_done,
                        now.saturating_duration_since(*started).as_secs(),
                    )
                })
                .unwrap_or_default();
            Some(DownloadStatus {
                target: target.as_str().to_string(),
                done_bytes: progress.done,
                total_bytes: progress.total,
                start_bytes,
                elapsed_seconds,
            })
        })
        .collect();
    statuses.sort_by(|left, right| left.target.cmp(&right.target));
    statuses
}

/// Child realized-EP → config Provider (shared STT/TTS).
fn realized_provider_token(child_provider: &str) -> ds_config::Provider {
    ds_config::RealizedProvider::parse(child_provider).to_provider()
}

/// Built_in only; `None` until realized. Unknown realized token fails closed to `"cpu"`.
fn stt_provider_token(
    resolved_stt: Option<ds_config::SttEngine>,
    child_provider: Option<&str>,
) -> Option<String> {
    match (resolved_stt, child_provider) {
        (Some(ds_config::SttEngine::BuiltIn), Some(p)) => {
            Some(realized_provider_token(p).as_str().to_string())
        }
        _ => None,
    }
}

/// Built_in only; `None` until realized. Unknown realized token fails closed to `"cpu"`.
fn tts_provider_token(
    resolved_tts: Option<ds_config::TtsEngine>,
    child_provider: Option<&str>,
) -> Option<String> {
    match (resolved_tts, child_provider) {
        (Some(ds_config::TtsEngine::BuiltIn), Some(p)) => {
            Some(realized_provider_token(p).as_str().to_string())
        }
        _ => None,
    }
}

/// `None` until ladder + shim/assets + `ensure_backend` can run. Lock/SepFormer gate
/// `running`, not this token (`diarize`/`enroll` work without either).
fn diarization_provider_token(
    diarizer: ds_config::DiarizerProvider,
    enabled: bool,
    backend_present: bool,
) -> Option<String> {
    (enabled && backend_present && ds_stt::diarize::ensure_backend(diarizer).is_ok())
        .then(|| diarizer.as_str().to_string())
}

fn tts_download_targets(cfg: &VoiceConfig) -> Vec<DownloadTarget> {
    if native_tts_active(cfg) {
        // Fluid → Core ML (Kokoro only); other native → MLX.
        let native_target = DownloadTarget::fluid_for_tts(cfg.tts_model)
            .filter(|_| cfg.resolved_tts_provider() == ds_config::Provider::Fluid)
            .unwrap_or_else(|| DownloadTarget::mlx_for_tts(cfg.tts_model));
        let mut targets = vec![native_target];
        if cfg.tts_model == ds_config::TtsModel::Kokoro {
            targets.push(DownloadTarget::KokoroFrontend);
        }
        targets
    } else {
        vec![DownloadTarget::portable_for_tts(cfg.tts_model)]
    }
}

/// Provider → diarization set (single place so presence + status token agree).
fn diarization_set_for(
    provider: ds_config::DiarizerProvider,
) -> &'static [&'static ds_model::HfRepo] {
    match provider {
        ds_config::DiarizerProvider::Mlx => &ds_model::mlx_repo::DIARIZATION_MLX_SET[..],
        ds_config::DiarizerProvider::Fluid => &ds_model::coreml_repo::DIARIZATION_COREML_SET[..],
    }
}

/// Resolved rung's set only (#200 wrong-rung files → absent). `roots` value: no `$HOME` (#212).
fn diarization_present(
    roots: &ds_model::ModelRoots,
    provider: ds_config::DiarizerProvider,
) -> bool {
    ds_model::hf_repo::is_hf_set_present(roots, diarization_set_for(provider))
}

/// Own fetch, or Cuda while `!engine_loaded` on ONNX (never after load). Pure.
fn row_downloading(
    own_downloading: bool,
    cuda_downloading: bool,
    engine_loaded: bool,
    uses_mlx: bool,
) -> bool {
    own_downloading || (!uses_mlx && cuda_downloading && !engine_loaded)
}

/// dl_err always; load_err only while present (absent → missing, not stale failed).
fn combined_error(
    present: bool,
    dl_err: Option<String>,
    load_err: Option<String>,
) -> Option<String> {
    dl_err.or(if present { load_err } else { None })
}

/// Row ring fraction: own Active|Done, else live Cuda only.
fn row_download_frac(
    targets: &std::collections::HashMap<DownloadTarget, TargetState>,
    own_targets: &[DownloadTarget],
) -> f64 {
    own_targets
        .iter()
        .find_map(|t| match targets.get(t) {
            Some(TargetState::Active(p) | TargetState::Done(p)) => Some(p.frac()),
            _ => None,
        })
        .or_else(|| match targets.get(&DownloadTarget::Cuda) {
            Some(TargetState::Active(p)) => Some(p.frac()),
            _ => None,
        })
        .unwrap_or(0.0)
}

/// Named fields for [`engine_status`] (avoid running/enabled transpose).
struct RowState {
    present: bool,
    downloading: bool,
    error: Option<String>,
    running: bool,
    enabled: bool,
}

/// One engine row; priority downloading > failed > missing > running > warming > idle.
fn engine_status(row: RowState, progress: f64) -> EngineStatus {
    let state = engine_state(
        row.present,
        row.downloading,
        row.error.is_some(),
        row.running,
        row.enabled,
    );
    EngineStatus {
        state,
        progress: if row.downloading && progress.is_finite() {
            progress.clamp(0.0, 1.0)
        } else {
            0.0
        },
        error: row.error,
    }
}

fn status_stt_engine(resolved: Option<ds_config::SttEngine>) -> StatusSttEngine {
    match resolved {
        Some(ds_config::SttEngine::BuiltIn) => StatusSttEngine::BuiltIn,
        Some(ds_config::SttEngine::System) => StatusSttEngine::System,
        Some(ds_config::SttEngine::ClaudeCode) => StatusSttEngine::ClaudeCode,
        None => StatusSttEngine::Off,
    }
}

fn status_tts_engine(resolved: Option<ds_config::TtsEngine>) -> StatusTtsEngine {
    match resolved {
        Some(ds_config::TtsEngine::BuiltIn) => StatusTtsEngine::BuiltIn,
        Some(ds_config::TtsEngine::System) => StatusTtsEngine::System,
        None => StatusTtsEngine::Off,
    }
}

fn status_tts_model(model: ds_config::TtsModel) -> StatusTtsModel {
    match model {
        ds_config::TtsModel::Kokoro => StatusTtsModel::Kokoro,
        ds_config::TtsModel::Chatterbox => StatusTtsModel::Chatterbox,
        ds_config::TtsModel::Qwen => StatusTtsModel::Qwen,
        ds_config::TtsModel::OmniVoice => StatusTtsModel::OmniVoice,
    }
}

fn status_tray_kind(k: ds_config::TrayKind) -> StatusTrayKind {
    match k {
        ds_config::TrayKind::Stt => StatusTrayKind::Stt,
        ds_config::TrayKind::Tts => StatusTrayKind::Tts,
        ds_config::TrayKind::SttAnimated => StatusTrayKind::SttAnimated,
        ds_config::TrayKind::TtsAnimated => StatusTrayKind::TtsAnimated,
    }
}

/// `local_stt` ≡ running.parakeet ‖ running.system (pinned by test).
fn dictation_local_stt(parakeet_running: bool, system_running: bool) -> bool {
    parakeet_running || system_running
}

/// Lifecycle: downloading > failed > missing > running > warming > idle.
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

/// Confirm-panel state: awaiting > (recording && local_stt) > refused > hidden.
/// Awaiting wins stop-tap window; ClaudeNative recording stays hidden.
pub(crate) fn dictation_state(
    recording: bool,
    awaiting: bool,
    local_stt: bool,
    refused: bool,
) -> DictationState {
    if awaiting {
        DictationState::AwaitingConfirm
    } else if recording && local_stt {
        DictationState::Recording
    } else if refused {
        DictationState::Refused
    } else {
        DictationState::Hidden
    }
}

fn dictation_session_id(
    presenters: &DictationPresenterRegistry,
    state: DictationState,
    presentation_session_id: u64,
) -> Option<String> {
    (state != DictationState::Hidden).then(|| presenters.session_id(presentation_session_id))
}

#[cfg(test)]
mod tests {
    use super::{
        EngineShared, StatusGate, active_download_statuses, combined_error,
        current_dictation_session_id, diarization_present, diarization_provider_token,
        diarization_set_for, dictation_local_stt, dictation_session_id, dictation_state,
        engine_state, model_status_json, realized_provider_token, row_download_frac,
        row_downloading, status_tts_model, stt_provider_token, tts_provider_token,
    };
    use crate::downloads::{DownloadProgress, DownloadState, TargetState};
    use crate::engine::{FinalState, PasteBuf};
    use crate::stats::{LifetimeSeconds, SttStats, TtsStats};
    use crate::tts::TtsManager;
    use crate::ttsq::TtsStatusSample;
    use ds_config::{Paths, Provider, SttEngine, TtsEngine};
    use ds_model::DownloadTarget;
    use ds_status::{
        DictationState, EngineState, ModelStatus, StatusSttEngine, StatusTtsEngine,
        UtteranceStatus, UtteranceWarning,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Exercise the real status serializer with isolated files and an unstarted helper. This
    /// pins the cross-platform wire snapshot without probing the ambient model cache or spawning
    /// any process — the empty model dir arrives from the child environment
    /// ([`crate::test_env`]).
    #[test]
    fn model_status_json_combines_downloads_runtime_flags_preview_and_stats() {
        const TEST: &str =
            "status::tests::model_status_json_combines_downloads_runtime_flags_preview_and_stats";
        let Some(_child) = crate::test_env::child_run() else {
            let model_dir = tempfile::tempdir().unwrap();
            crate::test_env::run_child(
                TEST,
                crate::test_env::ChildEnv {
                    phase: "empty-model-dir",
                    model_dir: model_dir.path(),
                    ort_dylib: None,
                },
            );
            return;
        };

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            &paths.config_toml,
            "stt_engine_ladder = [\"built_in\"]\ntts_engine_ladder = [\"built_in\"]\ncaps = false\ntray = []\n",
        )
        .unwrap();

        let tts_stats = Arc::new(TtsStats::new());
        tts_stats.record(20.0, 100.0, 5.0);
        let stt_stats = Arc::new(SttStats::new());
        stt_stats.record(40.0, 200.0);
        let lifetime = Arc::new(LifetimeSeconds::load(paths.stats_toml.clone()));
        let tts = Arc::new(TtsManager::new(
            temp.path().join("unused-helper"),
            paths.log_file.clone(),
            tts_stats.clone(),
            stt_stats.clone(),
            lifetime.clone(),
        ));
        let paste = Arc::new(Mutex::new(PasteBuf::default()));
        {
            let mut p = paste.lock().unwrap();
            p.partial = "live words".to_string();
            p.can_paste = false;
            p.final_state = FinalState::Armed;
        }
        let downloads = Arc::new(Mutex::new(DownloadState::default()));
        {
            let mut state = downloads.lock().unwrap();
            state.targets.insert(
                DownloadTarget::KokoroModel,
                TargetState::Active(DownloadProgress {
                    done: 25,
                    total: 100,
                }),
            );
            state.targets.insert(
                DownloadTarget::ParakeetModel,
                TargetState::Failed("download failed".to_string()),
            );
        }
        let gate = StatusGate::new();
        gate.bump();
        let dictation_presenters =
            Arc::new(crate::dictation_presenter::DictationPresenterRegistry::default());
        let dictation_session = dictation_presenters.session_id(0);
        let lease = dictation_presenters
            .acquire(
                dictation_session.clone(),
                Some(&dictation_session),
                3_500,
                Instant::now(),
            )
            .result
            .unwrap();
        dictation_presenters
            .ready(
                &lease.id,
                &dictation_session,
                Some(&dictation_session),
                Instant::now(),
            )
            .result
            .unwrap();
        let shared = EngineShared {
            tts,
            caps_active: Arc::new(AtomicBool::new(true)),
            stt_active: Arc::new(AtomicBool::new(true)),
            paste,
            downloads,
            tts_stats,
            stt_stats,
            lifetime,
            gate,
            dictation_presenters,
        };

        let utterance = UtteranceStatus {
            id: 7,
            voice: Some("af_sarah".to_string()),
            language: Some("it".to_string()),
            warning: Some(UtteranceWarning::VoiceLanguageMismatch),
            outcome: None,
        };
        let value = model_status_json(&shared, &paths, || TtsStatusSample {
            speaking: true,
            speaker: None,
            queued: 3,
            playback_state: Some(ds_status::PlaybackState::Playing),
            playback_hold_reason: None,
            utterance: Some(utterance.clone()),
            voice_sessions: vec![],
            recent_utterances: vec![UtteranceStatus {
                outcome: Some(ds_status::UtteranceOutcome::Spoken),
                ..utterance
            }],
        });
        // caps events are logged (see `Engine::record_caps`) but never serialized here.
        assert!(value.get("caps_events").is_none());
        assert_eq!(value["stats"]["tts"]["queued"], 3);
        assert_eq!(value["activity"]["utterance_id"], 7);
        assert_eq!(value["activity"]["playback_state"], "playing");
        assert!(value["activity"]["playback_hold_reason"].is_null());
        assert_eq!(value["activity"]["voice"], "af_sarah");
        assert_eq!(value["activity"]["language"], "it");
        assert_eq!(value["activity"]["warning"], "voice_language_mismatch");
        assert_eq!(value["downloads"][0]["target"], "kokoro_model");
        assert_eq!(value["downloads"][0]["done_bytes"], 25);
        assert_eq!(value["downloads"][0]["total_bytes"], 100);

        let status: ModelStatus = serde_json::from_value(value).unwrap();
        let tts = status.tts.status.as_ref().unwrap();
        assert_eq!(tts.state, EngineState::Downloading);
        assert_eq!(tts.progress, 0.25);
        let last = &status.tts.recent_utterances[0];
        assert_eq!(last.id, 7);
        assert_eq!(last.language.as_deref(), Some("it"));
        assert_eq!(last.outcome, Some(ds_status::UtteranceOutcome::Spoken));
        let stt = status.stt.status.as_ref().unwrap();
        assert_eq!(stt.state, EngineState::Failed);
        assert_eq!(stt.error.as_deref(), Some("download failed"));
        assert_eq!(status.stt.engine, StatusSttEngine::BuiltIn);
        assert_eq!(status.tts.engine, StatusTtsEngine::BuiltIn);
        assert!(status.activity.caps_active);
        assert!(!status.activity.caps);
        assert!(status.activity.recording);
        assert!(status.activity.speaking);
        assert_eq!(status.stats.tts.queued, 3);
        assert_eq!(status.dictation.text, "live words");
        assert!(!status.dictation.can_paste);
        assert_eq!(status.dictation.state, DictationState::AwaitingConfirm);
        assert_eq!(
            status.dictation.session_id.as_deref(),
            Some(dictation_session.as_str())
        );
        assert!(status.dictation.external_ui_active);
        assert_eq!(status.stats.tts.utterances, 1);
        assert_eq!(status.stats.stt.transcriptions, 1);
        assert_eq!(status.seq, 1);
        assert!(status.tray.is_empty());
    }

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

    #[test]
    fn snapshot_does_not_acknowledge_a_transition_during_its_reads() {
        let gate = StatusGate::new();
        let (speaking, snapshot_seq) = gate.snapshot(|| {
            // Reproduce the queue boundary: the waiter woke for `speaking=false`, then
            // playback restarted before the status builder finished. The snapshot may
            // include either state, but its epoch must predate this transition.
            gate.bump();
            true
        });

        assert!(speaking);
        assert_ne!(snapshot_seq, gate.seq());
        assert_eq!(
            gate.wait_changed(snapshot_seq, Duration::from_secs(5)),
            gate.seq(),
            "the next wait must return immediately for the unacknowledged transition"
        );
    }

    #[test]
    fn new_recording_generation_cannot_reuse_the_previous_ready_presenter() {
        let presenters = crate::dictation_presenter::DictationPresenterRegistry::default();
        let old_session = presenters.session_id(7);
        let lease = presenters
            .acquire(
                old_session.clone(),
                Some(&old_session),
                3_500,
                Instant::now(),
            )
            .result
            .unwrap();
        presenters
            .ready(&lease.id, &old_session, Some(&old_session), Instant::now())
            .result
            .unwrap();

        let paste = Arc::new(Mutex::new(PasteBuf {
            presentation_session_id: 8,
            ..PasteBuf::default()
        }));
        let stt_active = AtomicBool::new(true);
        let current = current_dictation_session_id(&presenters, &stt_active, &paste).unwrap();

        assert_eq!(current, presenters.session_id(8));
        assert_eq!(
            presenters.external_ui_active(Some(&current), Instant::now()),
            (false, true),
            "the previous turn's ready lease must not suppress native UI for the new turn"
        );
    }

    #[test]
    fn active_downloads_report_sorted_raw_transfer_counters() {
        use std::collections::HashMap;

        let now = Instant::now();
        let mut targets = HashMap::new();
        targets.insert(
            DownloadTarget::QwenModel,
            TargetState::Active(DownloadProgress {
                done: 50,
                total: 200,
            }),
        );
        targets.insert(
            DownloadTarget::KokoroModel,
            TargetState::Active(DownloadProgress {
                done: 100,
                total: 100,
            }),
        );
        targets.insert(
            DownloadTarget::ParakeetModel,
            TargetState::Failed("offline".to_string()),
        );
        let transfer_start = HashMap::from([
            (
                DownloadTarget::QwenModel,
                (now - Duration::from_secs(2), 10),
            ),
            (
                DownloadTarget::KokoroModel,
                (now - Duration::from_secs(1), 0),
            ),
        ]);

        let statuses = active_download_statuses(&targets, &transfer_start, now);
        assert_eq!(statuses.len(), 2, "terminal targets are omitted");
        assert_eq!(statuses[0].target, "kokoro_model");
        assert_eq!(statuses[0].start_bytes, 0);
        assert_eq!(statuses[0].elapsed_seconds, 1);
        assert_eq!(statuses[1].target, "qwen_model");
        assert_eq!(statuses[1].start_bytes, 10);
        assert_eq!(statuses[1].elapsed_seconds, 2);
    }

    /// Each model row picks ITS OWN target's fraction from the parallel-download map:
    /// the model fetch wins over the MLX set, which wins over the shared CUDA
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
        assert_eq!(row_download_frac(&active, &[KokoroModel, KokoroMlx]), 0.10);
        assert_eq!(
            row_download_frac(&active, &[ParakeetModel, ParakeetMlx]),
            0.30
        );

        // Only CUDA in flight (models present): both rows show the runtime's %.
        let cuda_only: HashMap<_, _> = [(Cuda, TargetState::Active(p(50, 100)))]
            .into_iter()
            .collect();
        assert_eq!(
            row_download_frac(&cuda_only, &[KokoroModel, KokoroMlx]),
            0.5
        );
        assert_eq!(
            row_download_frac(&cuda_only, &[ParakeetModel, ParakeetMlx]),
            0.5
        );

        // MLX flavor in flight → the row's second-priority target.
        let mlx: HashMap<_, _> = [(KokoroMlx, TargetState::Active(p(25, 100)))]
            .into_iter()
            .collect();
        assert_eq!(row_download_frac(&mlx, &[KokoroModel, KokoroMlx]), 0.25);
        // ...and it does NOT bleed into the Parakeet row.
        assert_eq!(row_download_frac(&mlx, &[ParakeetModel, ParakeetMlx]), 0.0);

        // The MLX adjunct target now carries required frontend graphs, so it is one
        // of Kokoro's own targets and feeds that row's ring.
        let frontend: HashMap<_, _> = [(KokoroFrontend, TargetState::Active(p(3, 4)))]
            .into_iter()
            .collect();
        assert_eq!(
            row_download_frac(&frontend, &[KokoroModel, KokoroMlx, KokoroFrontend]),
            0.75
        );

        // Nothing in flight ⇒ 0.
        assert_eq!(row_download_frac(&empty, &[KokoroModel, KokoroMlx]), 0.0);

        // Done own-target % beats live Cuda fallback.
        let kokoro_done_cuda_live: HashMap<_, _> = [
            (KokoroModel, TargetState::Done(p(100, 100))),
            (Cuda, TargetState::Active(p(50, 100))),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            row_download_frac(&kokoro_done_cuda_live, &[KokoroModel, KokoroMlx]),
            1.0,
            "a finished own-target download wins over a still-live Cuda fetch"
        );
        assert_eq!(
            row_download_frac(&kokoro_done_cuda_live, &[ParakeetModel, ParakeetMlx]),
            0.5,
            "Parakeet has nothing of its own, so it still falls back to Cuda"
        );

        // Done Cuda must not keep feeding row rings.
        let cuda_done: HashMap<_, _> = [(Cuda, TargetState::Done(p(100, 100)))]
            .into_iter()
            .collect();
        assert_eq!(
            row_download_frac(&cuda_done, &[KokoroModel, KokoroMlx]),
            0.0,
            "a Done Cuda entry must not feed the fallback"
        );
    }

    /// Cuda alone forces downloading only while the row engine is not yet loaded.
    #[test]
    fn row_downloading_gates_cuda_on_engine_loaded() {
        assert!(
            !row_downloading(false, true, true, false),
            "an already-loaded engine must not be forced back into 'downloading' by Cuda"
        );

        // First boot: Cuda-in-flight still gates not-yet-loaded ONNX row.
        assert!(
            row_downloading(false, true, false, false),
            "on first boot, Cuda-in-flight must still gate a not-yet-loaded ONNX row"
        );

        assert!(row_downloading(true, false, true, false));
        assert!(row_downloading(true, true, true, false));

        assert!(!row_downloading(false, false, false, false));
        assert!(!row_downloading(false, false, true, false));

        // MLX path: Cuda never gates the row.
        assert!(!row_downloading(false, true, false, true));
        assert!(!row_downloading(false, true, true, true));
        assert!(row_downloading(true, true, false, true));
    }

    #[test]
    fn tts_and_stt_report_the_same_realized_runtime() {
        // TTS and STT map realized EP through one `realized_provider_token`.
        let k = Some(TtsEngine::BuiltIn);
        let b = Some(SttEngine::BuiltIn);
        for realized in ["CUDA", "CPU", "MLX", "System", "CoreML", "surprise"] {
            assert_eq!(
                tts_provider_token(k, Some(realized)),
                stt_provider_token(b, Some(realized)),
                "TTS and STT must map realized `{realized}` to the SAME token"
            );
        }
    }

    #[test]
    fn provider_tokens_reflect_the_realized_runtime() {
        // The token is the REALIZED EP the child reports, not a preference — CPU fallback included.
        let k = Some(TtsEngine::BuiltIn);
        let b = Some(SttEngine::BuiltIn);
        assert_eq!(tts_provider_token(k, Some("CUDA")).as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, Some("CUDA")).as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, Some("CPU")).as_deref(), Some("cpu"));
        assert_eq!(stt_provider_token(b, Some("MLX")).as_deref(), Some("mlx"));
        // Anything unrecognized (or "System") is CPU, never a wrong GPU claim.
        assert_eq!(
            stt_provider_token(b, Some("System")).as_deref(),
            Some("cpu")
        );
        assert_eq!(
            stt_provider_token(b, None),
            None,
            "no child has realized a backend (e.g. the Parakeet download) ⇒ null, not a \
             fabricated \"cpu\""
        );
        assert_eq!(
            tts_provider_token(k, None),
            None,
            "a built-in TTS with no realized backend yet (download/warming/no-preload) ⇒ \
             null, not a fabricated \"cpu\""
        );
        // ds-status is deliberately independent of ds-config, so the two model-token
        // vocabularies are unguarded duplicates anywhere but here, where both are in
        // scope: a rename on one side alone would split the config and status wires.
        for model in ds_config::TtsModel::ALL {
            assert_eq!(status_tts_model(*model).as_str(), model.as_str());
        }
        // The shared mapper's own table.
        assert_eq!(realized_provider_token("CUDA"), Provider::OrtCuda);
        assert_eq!(realized_provider_token("MLX"), Provider::Mlx);
        assert_eq!(realized_provider_token("CoreML"), Provider::OrtCoreMl);
        assert_eq!(realized_provider_token("nonsense"), Provider::OrtCpu);
        // No local runtime token for the delegate/OS engines or when the engine is off.
        assert_eq!(
            stt_provider_token(Some(SttEngine::ClaudeCode), Some("CUDA")),
            None
        );
        assert_eq!(stt_provider_token(None, Some("CUDA")), None);
        assert_eq!(
            tts_provider_token(Some(TtsEngine::System), Some("CUDA")),
            None
        );
        assert_eq!(tts_provider_token(None, Some("CUDA")), None);
    }

    #[test]
    fn diarization_provider_names_only_a_usable_backend() {
        // #200: the token was a constant, so the row claimed "mlx" with state "missing".
        // R8: extended to every rung — config off or shim/assets absent ⇒ nothing loadable ⇒
        // nothing to name, for MLX and FluidAudio alike.
        for provider in ds_config::DiarizerProvider::ALL.iter().copied() {
            for (enabled, present) in [(false, false), (false, true), (true, false)] {
                assert_eq!(
                    diarization_provider_token(provider, enabled, present),
                    None,
                    "{provider:?}: no loadable backend with enabled={enabled} present={present}"
                );
            }
            // Wiring is Apple-Silicon-macOS-only (`ds_stt::diarize::ensure_backend`, which
            // defers to `is_diarizer_usable`), so elsewhere even a downloaded, enabled diarizer
            // names nothing.
            let usable = diarization_provider_token(provider, true, true);
            if provider.is_diarizer_usable() {
                assert_eq!(usable.as_deref(), Some(provider.as_str()));
            } else {
                assert_eq!(usable, None, "{provider:?}");
            }
        }
    }

    #[test]
    fn diarization_presence_follows_the_selected_rung() {
        use ds_config::DiarizerProvider;
        // R8: the presence probe must follow the RESOLVED provider's set, so a
        // `diarizer=["fluid"]` config with only the MLX set on disk can never read the Core ML
        // row as present (and thus never claim `provider:"fluid"` on a row that cannot load).
        // Pin the set routing directly; the sha256 probe over real model files can't be
        // materialized hermetically.
        let names = |set: &[&ds_model::HfRepo]| set.iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(
            names(diarization_set_for(DiarizerProvider::Mlx)),
            names(&ds_model::mlx_repo::DIARIZATION_MLX_SET)
        );
        assert_eq!(
            names(diarization_set_for(DiarizerProvider::Fluid)),
            names(&ds_model::coreml_repo::DIARIZATION_COREML_SET)
        );
        // The two rungs must select DIFFERENT sets — the swap R8 guards against.
        assert_ne!(
            names(diarization_set_for(DiarizerProvider::Mlx)),
            names(diarization_set_for(DiarizerProvider::Fluid))
        );
        // Absent on disk, presence is false for every rung, resolved through `roots` (no
        // `$HOME` read).
        let tmp = tempfile::tempdir().unwrap();
        let roots = ds_model::ModelRoots::under(tmp.path());
        for provider in DiarizerProvider::ALL.iter().copied() {
            assert!(!diarization_present(&roots, provider), "{provider:?}");
        }
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

    /// Either local STT backend makes dictation local; delegated STT does not.
    #[test]
    fn local_stt_accepts_either_local_backend() {
        for parakeet_running in [false, true] {
            for system_running in [false, true] {
                assert_eq!(
                    dictation_local_stt(parakeet_running, system_running),
                    parakeet_running || system_running,
                    "local_stt must equal parakeet_running || system_running \
                     (parakeet_running={parakeet_running}, system_running={system_running})"
                );
            }
        }
    }

    /// The canonical `dictation.state` token must preserve the producer's show gate:
    /// `state != Hidden ⇔ awaiting || (recording && local_stt) || refused` —
    /// pinned across the FULL 16-row truth table so a precedence edit can't silently
    /// change any host's panel visibility.
    #[test]
    fn dictation_state_matches_the_show_gate_for_all_inputs() {
        for recording in [false, true] {
            for awaiting in [false, true] {
                for local_stt in [false, true] {
                    for refused in [false, true] {
                        let state = dictation_state(recording, awaiting, local_stt, refused);
                        assert_eq!(
                            state != DictationState::Hidden,
                            awaiting || (recording && local_stt) || refused,
                            "show-gate equivalence (recording={recording}, \
                             awaiting={awaiting}, local_stt={local_stt}, refused={refused})"
                        );
                    }
                }
            }
        }
    }

    /// Pins the precedence ladder itself: `awaiting_confirm > (recording && local_stt) >
    /// refused > hidden` — including that a non-local recording (ClaudeNative) stays
    /// `hidden` (its panel is deliberately suppressed).
    #[test]
    fn dictation_state_precedence_table() {
        // awaiting wins over everything (the finalized transcript must show).
        assert_eq!(
            dictation_state(true, true, true, true),
            DictationState::AwaitingConfirm
        );
        // recording (local) wins over refused (refused_until is cleared on a real start).
        assert_eq!(
            dictation_state(true, false, true, true),
            DictationState::Recording
        );
        // refused shows even while a non-local recording is active.
        assert_eq!(
            dictation_state(true, false, false, true),
            DictationState::Refused
        );
        // A ClaudeNative recording (no local transcript) keeps the panel hidden.
        assert_eq!(
            dictation_state(true, false, false, false),
            DictationState::Hidden
        );
    }

    #[test]
    fn dictation_session_projection_is_stable_visible_absent_hidden_and_new_per_turn() {
        let presenters = crate::dictation_presenter::DictationPresenterRegistry::default();
        let recording = dictation_session_id(&presenters, DictationState::Recording, 7).unwrap();
        let awaiting =
            dictation_session_id(&presenters, DictationState::AwaitingConfirm, 7).unwrap();
        assert_eq!(recording, awaiting);
        assert_eq!(
            dictation_session_id(&presenters, DictationState::Hidden, 7),
            None
        );

        let refused = dictation_session_id(&presenters, DictationState::Refused, 8).unwrap();
        let next_recording =
            dictation_session_id(&presenters, DictationState::Recording, 9).unwrap();
        assert_ne!(recording, refused);
        assert_ne!(refused, next_recording);
    }
}
