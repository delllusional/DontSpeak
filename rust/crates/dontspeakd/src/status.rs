//! `model_status` aggregator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use ds_config::{Paths, VoiceConfig};

use crate::config_gate::{
    caps_loop_enabled, mlx_shim_available, mlx_tts_active, parakeet_available,
    parakeet_onnx_files_present, stt_uses_onnx_runtime, tts_model_files_present,
};
use crate::downloads::{DownloadProg, TargetState};
use crate::engine::{PasteState, dictation_preview};
use crate::stats;
use crate::tts::TtsManager;
use ds_model::DownloadTarget;
use ds_status::{
    Activity, DiarizationStatus, Dictation, DictationState, EngineState, EngineStatus, ModelStatus,
    Stats, StatusSttEngine, StatusTrayKind, StatusTtsEngine, StatusTtsModel, SttStatus,
    TtsSnapshot, TtsStatus,
};

/// Status seq + condvar for `WaitModelStatus`. Bump after every status flip.
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

    /// Seq first, then read: mid-read transitions stay unacked (next wait returns).
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

/// Shared Arcs for IPC + status (built once in `engine_run`).
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
    /// WaitModelStatus gate (bumped on every status flip).
    pub gate: Arc<StatusGate>,
}

/// Model presence report. `read_tts` runs under gate.snapshot so mid-report
/// transitions stay unacked. Returns `(speaking, speaker, queued)`.
pub(crate) fn model_status_json(
    shared: &EngineShared,
    paths: &Paths,
    read_tts: impl FnOnce() -> (bool, Option<ds_config::ClientSource>, u64),
) -> serde_json::Value {
    let ((tts_active, tts_source, tts_queued), seq) = shared.gate.snapshot(read_tts);
    let EngineShared {
        tts,
        caps_active,
        stt_active,
        paste,
        downloads,
        tts_stats,
        stt_stats,
        lifetime,
        gate: _,
    } = shared;
    let cfg = VoiceConfig::load(paths);
    // Resolved ladders (skip unusable rungs) — all active checks use these.
    let resolved_tts = cfg.resolved_tts();
    let resolved_stt = cfg.resolved_stt();
    // CHEAP presence: file existence only — NO sha256. model_status is polled to
    // drive the UI's status dots, so it must be fast; full sha verification over
    // large TTS and Parakeet ONNX files would delay the dots by
    // many seconds. Correctness-critical sha checks stay in the load path
    // (load_synth / ParakeetModel::load), not here.
    // The TTS row reflects the active backend (mirrors the Parakeet row below).
    let shim = mlx_shim_available();
    let tts_uses_mlx = mlx_tts_active(&cfg);
    let tts_present = tts_model_files_present(&cfg);
    let parakeet_onnx_files = parakeet_onnx_files_present();
    // Same shim-aware ONNX downgrade as TTS.
    let stt_uses_onnx = stt_uses_onnx_runtime(cfg.resolved_stt_provider(), shim);
    let parakeet_present = if stt_uses_onnx {
        parakeet_onnx_files
    } else {
        parakeet_available()
    };
    let parakeet_enabled = resolved_stt == Some(ds_config::SttEngine::BuiltIn);
    let parakeet_running = parakeet_enabled && parakeet_present;
    // System STT: OS-owned model; same present/warming/running split as Parakeet.
    let system_enabled = resolved_stt == Some(ds_config::SttEngine::System);
    // Probe only when selected (row hidden otherwise).
    let system_state = if system_enabled {
        ds_stt::system_state()
    } else {
        ds_stt::SystemState::Unavailable
    };
    let system_present = system_enabled && system_state != ds_stt::SystemState::Unavailable;
    let system_running = system_state == ds_stt::SystemState::Ready;

    // claude_code: read CC config only when selected. present = voice on + synthesizable key.
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

    // Dictation-preview snapshot for the confirm panel (see `dictation_preview`): the
    // finalized transcript while awaiting confirmation, else the live partial — but never
    // the finalized text while a Caps press is in flight (a long-press cancel mustn't flash
    // the bubble before it dismisses).
    let (dict_text, dict_awaiting, dict_has_target, dict_refused, dict_frontend_owned) = paste
        .lock()
        .map(|p| {
            let (text, awaiting) = dictation_preview(&p.final_state, &p.partial, p.caps_held);
            (
                text,
                awaiting,
                p.can_paste,
                // Same refusal clock as tick digest.
                crate::engine::refusal_live(p.refused_until, std::time::Instant::now()),
                // Frontend-owned (Zed): force served state to hidden.
                p.frontend_owned,
            )
        })
        .unwrap_or((String::new(), false, true, false, false));

    // Per-target download snapshot (parallel; each row owns its fraction).
    let dl = downloads
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .targets
        .clone();
    let downloading = |eng: DownloadTarget| matches!(dl.get(&eng), Some(TargetState::Active(_)));
    // Active-only (Done % is row_download_frac).
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

    let diar_present = diarization_present();
    // SepFormer required for lock green (exists check only — sha at download).
    let sepformer_present = ds_model::model_path(ds_model::SEPFORMER_FILE)
        .map(|p| p.is_file())
        .unwrap_or(false);

    // Download manager owns "downloading". GREEN = loaded (resident+warm).
    // Cuda is ONNX compute dep: force downloading until row's engine loads; never
    // after (avoids loaded Kokoro flashing back to ~25% ring). See row_downloading.
    let tts_loaded = tts.is_tts_loaded();
    let stt_loaded = tts.is_stt_loaded();
    let cuda_downloading = downloading(DownloadTarget::Cuda);
    let tts_own_downloading = tts_targets.iter().any(|target| downloading(*target));
    let tts_downloading = row_downloading(
        tts_own_downloading,
        cuda_downloading && cfg.resolved_tts_provider() == ds_config::Provider::OrtCuda,
        tts_loaded,
        tts_uses_mlx,
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

    // One load each — avoid tear across recording/prompt_glow/state.
    let dict_recording = stt_active.load(Ordering::Relaxed);
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
        // Parakeet STT — one engine, runtime chosen by `stt.provider`.
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

    // Speaker lock needs both diarization and SepFormer before it can report running.
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

    let status = ModelStatus {
        seq,
        activity: Activity {
            caps: caps_loop_enabled(&cfg),
            caps_active: caps_active.load(Ordering::Relaxed),
            recording: dict_recording,
            speaking: tts_active,
            speaker: tts_source,
            muted: tts.is_muted(),
        },
        tts: TtsStatus {
            engine: status_tts_engine(resolved_tts),
            model: (resolved_tts == Some(ds_config::TtsEngine::BuiltIn))
                .then(|| status_tts_model(cfg.tts_model)),
            language: (resolved_tts == Some(ds_config::TtsEngine::BuiltIn))
                .then(|| "auto".to_string()),
            provider: tts_provider_token(resolved_tts, tts.provider().as_str()),
            status: tts_status,
        },
        stt: SttStatus {
            engine: status_stt_engine(resolved_stt),
            provider: stt_provider_token(resolved_stt, &tts.stt_realized_provider()),
            status: stt_status,
            voice_key: claude_code_key,
        },
        diarization: DiarizationStatus {
            status: diarization_status,
            enabled: cfg.is_diarization_on(),
            // Realized compute token hosts pass to `ds_runtime_label`.
            provider: ds_config::Provider::Mlx.as_str().to_string(),
            speakers: ds_config::SpeakerStore::load(&paths.speakers_json).names(),
            activity_threshold: cfg.activity_threshold as f64,
        },
        dictation: Dictation {
            state: served_dictation_state(
                dict_frontend_owned,
                dict_recording,
                dict_awaiting,
                dict_local,
                dict_refused,
            ),
            text: dict_text,
            can_paste: dict_has_target,
        },
        stats: Stats {
            // Depth comes from the queue under `gate.snapshot`, not the stats accumulator.
            tts: TtsSnapshot {
                queued: tts_queued,
                ..tts_stats.snapshot()
            },
            stt: stt_stats.snapshot(),
            lifetime: lifetime.snapshot(),
        },
        tray: cfg.tray.iter().copied().map(status_tray_kind).collect(),
    };
    serde_json::to_value(status).unwrap_or(serde_json::Value::Null)
}

/// Child realized-EP → config Provider (shared STT/TTS; drift guard in tests).
fn realized_provider_token(child_provider: &str) -> ds_config::Provider {
    ds_config::RealizedProvider::parse(child_provider).to_provider()
}

/// Realized STT EP token (built_in only); child reports honest backend.
fn stt_provider_token(
    resolved_stt: Option<ds_config::SttEngine>,
    child_provider: &str,
) -> Option<String> {
    match resolved_stt {
        Some(ds_config::SttEngine::BuiltIn) => {
            Some(realized_provider_token(child_provider).as_str().to_string())
        }
        _ => None,
    }
}

/// Realized TTS provider token (local engines only; child reports honest backend).
fn tts_provider_token(
    resolved_tts: Option<ds_config::TtsEngine>,
    child_provider: &str,
) -> Option<String> {
    match resolved_tts {
        Some(ds_config::TtsEngine::BuiltIn) => {
            Some(realized_provider_token(child_provider).as_str().to_string())
        }
        _ => None,
    }
}

fn tts_download_targets(cfg: &VoiceConfig) -> Vec<DownloadTarget> {
    if mlx_tts_active(cfg) {
        let mut targets = vec![DownloadTarget::mlx_for_tts(cfg.tts_model)];
        if cfg.tts_model == ds_config::TtsModel::Kokoro {
            targets.push(DownloadTarget::KokoroFrontend);
        }
        targets
    } else {
        vec![DownloadTarget::portable_for_tts(cfg.tts_model)]
    }
}

/// MLX diarization present — same completion markers as downloader.
fn diarization_present() -> bool {
    ds_model::mlx_repo::is_mlx_set_present(&ds_model::mlx_repo::DIARIZATION_MLX_SET)
}

/// Row "downloading": own fetch, or Cuda while `!engine_loaded` on ONNX path.
/// Cuda must not flip an already-loaded row. Pure (unit-tested).
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

/// Flags for [`engine_obj`] (named fields avoid running/enabled transpose).
struct RowState {
    present: bool,
    downloading: bool,
    error: Option<String>,
    running: bool,
    enabled: bool,
}

/// Build one engine row with a lifecycle `state` (the app maps it 1:1 to a status dot):
/// downloading > failed > missing > running > warming > idle.
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

/// Frontend-owned dictation forces Hidden so tray overlays stay down (Zed renders it).
pub(crate) fn served_dictation_state(
    frontend_owned: bool,
    recording: bool,
    awaiting: bool,
    local_stt: bool,
    refused: bool,
) -> DictationState {
    if frontend_owned {
        DictationState::Hidden
    } else {
        dictation_state(recording, awaiting, local_stt, refused)
    }
}


#[cfg(test)]
mod tests {
    use super::{
        EngineShared, StatusGate, combined_error, dictation_local_stt, dictation_state,
        engine_state, model_status_json, realized_provider_token, row_download_frac,
        row_downloading, served_dictation_state, status_tts_model, stt_provider_token,
        tts_provider_token,
    };
    use crate::downloads::{DownloadProgress, DownloadState, TargetState};
    use crate::engine::PasteBuf;
    use crate::stats::{LifetimeSeconds, SttStats, TtsStats};
    use crate::tts::TtsManager;
    use ds_config::{Paths, Provider, SttEngine, TtsEngine};
    use ds_model::DownloadTarget;
    use ds_status::{DictationState, EngineState, ModelStatus, StatusSttEngine, StatusTtsEngine};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Exercise the real status serializer with isolated files and an unstarted helper. This
    /// pins the cross-platform wire snapshot without probing the ambient model cache or spawning
    /// any process.
    #[test]
    fn model_status_json_combines_downloads_runtime_flags_preview_and_stats() {
        let _env_guard = crate::config_gate::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let previous_model_dir = std::env::var_os("DONTSPEAK_MODEL_DIR");
        let previous_ort = std::env::var_os("ORT_DYLIB_PATH");
        let previous_shim = std::env::var_os("DONTSPEAK_MLX_DYLIB_PATH");

        let temp = tempfile::tempdir().unwrap();
        let model_dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(temp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            &paths.config_toml,
            "stt_engine_ladder = [\"built_in\"]\ntts_engine_ladder = [\"built_in\"]\ncaps = false\ntray = []\n",
        )
        .unwrap();
        // SAFETY: process-wide test environment mutation is serialized by ENV_LOCK and restored
        // before assertions leave this test.
        unsafe {
            std::env::set_var("DONTSPEAK_MODEL_DIR", model_dir.path());
            std::env::remove_var("ORT_DYLIB_PATH");
            std::env::remove_var("DONTSPEAK_MLX_DYLIB_PATH");
        }

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
        };

        let value = model_status_json(&shared, &paths, || (true, None, 3));
        // caps events are logged (see `Engine::record_caps`) but never serialized here.
        assert!(value.get("caps_events").is_none());
        assert_eq!(value["stats"]["tts"]["queued"], 3);
        // SAFETY: restore the three values while ENV_LOCK is still held.
        unsafe {
            match previous_model_dir {
                Some(value) => std::env::set_var("DONTSPEAK_MODEL_DIR", value),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
            match previous_ort {
                Some(value) => std::env::set_var("ORT_DYLIB_PATH", value),
                None => std::env::remove_var("ORT_DYLIB_PATH"),
            }
            match previous_shim {
                Some(value) => std::env::set_var("DONTSPEAK_MLX_DYLIB_PATH", value),
                None => std::env::remove_var("DONTSPEAK_MLX_DYLIB_PATH"),
            }
        }

        let status: ModelStatus = serde_json::from_value(value).unwrap();
        let tts = status.tts.status.as_ref().unwrap();
        assert_eq!(tts.state, EngineState::Downloading);
        assert_eq!(tts.progress, 0.25);
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
        assert_eq!(status.dictation.state, DictationState::Hidden);
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
                tts_provider_token(k, realized),
                stt_provider_token(b, realized),
                "TTS and STT must map realized `{realized}` to the SAME token"
            );
        }
    }

    #[test]
    fn provider_tokens_reflect_the_realized_runtime() {
        // The token is the REALIZED EP the child reports, not a preference — CPU fallback included.
        let k = Some(TtsEngine::BuiltIn);
        let b = Some(SttEngine::BuiltIn);
        assert_eq!(tts_provider_token(k, "CUDA").as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, "CUDA").as_deref(), Some("cuda"));
        assert_eq!(stt_provider_token(b, "CPU").as_deref(), Some("cpu"));
        assert_eq!(stt_provider_token(b, "MLX").as_deref(), Some("mlx"));
        // Anything unrecognized (or "System") is CPU, never a wrong GPU claim.
        assert_eq!(stt_provider_token(b, "System").as_deref(), Some("cpu"));
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
    fn served_dictation_state_overrides_only_frontend_owned() {
        // Frontend-owned always Hidden regardless of underlying phase.
        assert_eq!(
            served_dictation_state(true, true, false, true, false),
            DictationState::Hidden
        );
        assert_eq!(
            served_dictation_state(true, false, true, true, false),
            DictationState::Hidden
        );
        // Not frontend-owned: same as dictation_state.
        assert_eq!(
            served_dictation_state(false, true, false, true, false),
            dictation_state(true, false, true, false)
        );
        assert_eq!(
            served_dictation_state(false, false, true, true, false),
            dictation_state(false, true, true, false)
        );
    }

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
}
