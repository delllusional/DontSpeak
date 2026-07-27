//! Background download state + auto-fetch / provider-apply.
//!
//! Targets fetch in parallel (own thread each). Single-flight per target: re-request
//! attaches via `model_status`. Shared files deduped by ds-model per-path flight lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{Paths, Provider, VoiceConfig};
use ds_model::DownloadTarget;

use crate::config_gate::{NativeShims, native_tts_active, stt_uses_onnx_runtime};
use crate::tts::TtsManager;

/// Byte progress of one in-flight download target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct DownloadProgress {
    pub done: u64,
    pub total: u64,
}

impl DownloadProgress {
    /// 0.0–1.0 fraction for the status ring; 0.0 until the total is known.
    pub fn frac(self) -> f64 {
        if self.total > 0 {
            self.done as f64 / self.total as f64
        } else {
            0.0
        }
    }
}

/// Per-target lifecycle. Absent = never started this session (absence is idle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetState {
    Active(DownloadProgress),
    /// Kept until a new download for this target starts.
    Failed(String),
    /// Keep final % so the row ring doesn't fall through to another live fetch (e.g. Cuda).
    Done(DownloadProgress),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetryGate {
    Permanent,
    Transient { failures: u32, not_before: Instant },
}

const AUTO_RETRY_BASE: Duration = Duration::from_secs(20);
const AUTO_RETRY_CAP: Duration = Duration::from_secs(15 * 60);

/// Download progress for `model_status` (orange ring / red failed dot).
#[derive(Default)]
pub(crate) struct DownloadState {
    /// See [`TargetState`].
    pub targets: HashMap<DownloadTarget, TargetState>,
    retry_gates: HashMap<DownloadTarget, RetryGate>,
    /// First timed sample (excludes already-present bytes from rate estimates).
    pub transfer_start: HashMap<DownloadTarget, (Instant, u64)>,
    /// Warm-child reload hook ([`wire`]); restart on success iff [`download_needs_child_reload`].
    pub warm: Option<Arc<TtsManager>>,
    pub paths: Option<Paths>,
    /// Boot-loop `reload_requested`: rebuild Stt/Tts after download (placeholder → real model).
    pub reload: Option<Arc<AtomicBool>>,
    /// `engine_stop` running flag; completion skips side effects when stopped. `None` = always run.
    pub shutdown: Option<Arc<AtomicBool>>,
}

impl DownloadState {
    /// Any Active target (Done/Failed persist — not `targets.is_empty()`).
    pub fn any_active(&self) -> bool {
        self.targets
            .values()
            .any(|t| matches!(t, TargetState::Active(_)))
    }
}

pub(crate) type DownloadProg = Arc<Mutex<DownloadState>>;

/// Engine-lifetime flags for [`wire`] (named to avoid same-typed Arc transpose).
pub(crate) struct DownloadFlags {
    pub reload: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
}

/// Wire warm + shutdown once at boot (one mutex take). See [`DownloadState`].
pub(crate) fn wire(dl: &DownloadProg, warm: Arc<TtsManager>, paths: Paths, flags: DownloadFlags) {
    let mut s = dl.lock().unwrap_or_else(|e| e.into_inner());
    s.warm = Some(warm);
    s.paths = Some(paths);
    s.reload = Some(flags.reload);
    s.shutdown = Some(flags.running);
}

/// Whether the warm child hosts this completed download.
pub(crate) fn download_needs_child_reload(target: DownloadTarget, cfg: &VoiceConfig) -> bool {
    let builtin_tts = cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn);
    // Same native-axis match as [`compute_needs`] (missing shim → ONNX).
    let provider = cfg.resolved_tts_provider();
    let native = native_tts_active(cfg);
    let mlx_tts = native && provider == ds_config::Provider::Mlx;
    let fluid_tts = native && provider == ds_config::Provider::Fluid;
    let tts_target_matches = target.tts_model() == Some(cfg.tts_model)
        && target.is_mlx_tts() == mlx_tts
        && target.is_fluid_tts() == fluid_tts;
    (builtin_tts && tts_target_matches)
        // Kokoro frontend assets load in the warm helper (ONNX + MLX).
        || (builtin_tts
            && cfg.tts_model == ds_config::TtsModel::Kokoro
            && target == DownloadTarget::KokoroFrontend)
        || (cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
            && matches!(
                target,
                DownloadTarget::ParakeetModel
                    | DownloadTarget::ParakeetMlx
                    | DownloadTarget::ParakeetFluid
            ))
        || (target == DownloadTarget::Cuda && ds_model::cuda_runtime_wanted(cfg))
}

/// Mark a due target Active. Active/permanent/backoff-gated targets attach or stay idle.
fn begin_download_at(s: &mut DownloadState, which: DownloadTarget, now: Instant) -> bool {
    if matches!(s.targets.get(&which), Some(TargetState::Active(_))) {
        return false; // already downloading — attach, don't retrigger
    }
    match s.retry_gates.get(&which) {
        Some(RetryGate::Permanent) => return false,
        Some(RetryGate::Transient { not_before, .. }) if now < *not_before => return false,
        _ => {}
    }
    // Active overwrites prior Done/Failed (no stale ring / error).
    s.targets
        .insert(which, TargetState::Active(DownloadProgress::default()));
    s.transfer_start.remove(&which);
    true
}

fn begin_download(s: &mut DownloadState, which: DownloadTarget) -> bool {
    begin_download_at(s, which, Instant::now())
}

fn retry_delay(failures: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(31))
        .unwrap_or(u32::MAX);
    AUTO_RETRY_BASE
        .saturating_mul(multiplier)
        .min(AUTO_RETRY_CAP)
}

/// Err → Failed always; Ok only Active → Done (keeps final %).
fn finish_download_at(
    s: &mut DownloadState,
    which: DownloadTarget,
    result: &std::io::Result<()>,
    now: Instant,
) {
    s.transfer_start.remove(&which);
    match result {
        // Always record Err (active or not) so a red-dot path stays visible.
        Err(e) => {
            s.targets.insert(which, TargetState::Failed(e.to_string()));
            let gate = if ds_model::is_permanent_error(e) {
                RetryGate::Permanent
            } else {
                let failures = match s.retry_gates.get(&which) {
                    Some(RetryGate::Transient { failures, .. }) => failures.saturating_add(1),
                    _ => 1,
                };
                RetryGate::Transient {
                    failures,
                    not_before: now.checked_add(retry_delay(failures)).unwrap_or(now),
                }
            };
            s.retry_gates.insert(which, gate);
        }
        // Ok on non-Active leaves state untouched.
        Ok(()) => {
            s.retry_gates.remove(&which);
            if let Some(TargetState::Active(p)) = s.targets.get(&which) {
                let p = *p;
                s.targets.insert(which, TargetState::Done(p));
            }
        }
    }
}

fn finish_download(s: &mut DownloadState, which: DownloadTarget, result: &std::io::Result<()>) {
    finish_download_at(s, which, result, Instant::now());
}

/// Lifecycle log line (`started`/`finished`/`failed` [+ detail]). Pure for unit tests.
fn download_event_msg(which: DownloadTarget, phase: &str, detail: Option<&str>) -> String {
    match detail {
        Some(d) => format!("model download ({}) {phase}: {d}", which.as_str()),
        None => format!("model download ({}) {phase}", which.as_str()),
    }
}

/// Background download (returns immediately; see crate doc).
pub(crate) fn start_download(dl: &DownloadProg, which: DownloadTarget) {
    if !begin_download(&mut dl.lock().unwrap_or_else(|e| e.into_inner()), which) {
        return; // attach, don't retrigger
    }
    log::info!(target: "engine", "{}", download_event_msg(which, "started", None));
    let dl = dl.clone();
    std::thread::spawn(move || {
        let (warm, paths, reload, shutdown) = {
            let s = dl.lock().unwrap_or_else(|e| e.into_inner());
            (
                s.warm.clone(),
                s.paths.clone(),
                s.reload.clone(),
                s.shutdown.clone(),
            )
        };
        let prog = |done: u64, total: u64| {
            // Late callbacks after finish no longer match Active.
            let mut state = dl.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(state.targets.get(&which), Some(TargetState::Active(_))) {
                state
                    .transfer_start
                    .entry(which)
                    .or_insert_with(|| (Instant::now(), done));
                if let Some(TargetState::Active(progress)) = state.targets.get_mut(&which) {
                    *progress = DownloadProgress { done, total };
                }
            }
        };
        // Match compute_needs or boot requeues incomplete forever.
        let cuda_assets = |model: ds_config::TtsModel| {
            paths.as_ref().is_some_and(|paths| {
                ds_model::tts_wants_cuda_assets(
                    model,
                    VoiceConfig::load(paths).tts_provider_token(),
                )
            })
        };
        // Resolve roots once; keep `ModelRoots::ambient` at the engine boundary.
        #[cfg(target_os = "macos")]
        let hf_repos = |set: &[&'static ds_model::HfRepo]| -> std::io::Result<()> {
            let roots = ds_model::ModelRoots::ambient()
                .ok_or_else(|| std::io::Error::other("cannot resolve the model directory"))?;
            ds_model::hf_repo::ensure_hf_repos(&roots, set, &prog)
        };
        // Host gate once (uniform red-dot path).
        let result: std::io::Result<()> = if !which.is_supported_on_this_host() {
            Err(std::io::Error::other(format!(
                "'{}' is not available on this platform",
                which.as_str()
            )))
        } else {
            match which {
                // Full portable Kokoro (weights + frontend + ORT).
                DownloadTarget::KokoroModel => {
                    ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ())
                }
                // Shared frontend only (MLX, or ONNX when weights already present).
                DownloadTarget::KokoroFrontend => {
                    ds_model::run_setup_kokoro_frontend_with_progress(&prog).map(|_| ())
                }
                DownloadTarget::ParakeetModel => {
                    ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ())
                }
                DownloadTarget::ChatterboxModel => ds_model::run_setup_tts_model_with_progress(
                    ds_config::TtsModel::Chatterbox,
                    cuda_assets(ds_config::TtsModel::Chatterbox),
                    &prog,
                )
                .map(|_| ()),
                DownloadTarget::QwenModel => ds_model::run_setup_tts_model_with_progress(
                    ds_config::TtsModel::Qwen,
                    cuda_assets(ds_config::TtsModel::Qwen),
                    &prog,
                )
                .map(|_| ()),
                DownloadTarget::OmniVoiceModel => ds_model::run_setup_tts_model_with_progress(
                    ds_config::TtsModel::OmniVoice,
                    cuda_assets(ds_config::TtsModel::OmniVoice),
                    &prog,
                )
                .map(|_| ()),
                #[cfg(all(
                    any(target_os = "windows", target_os = "linux"),
                    target_arch = "x86_64"
                ))]
                DownloadTarget::Cuda => {
                    ds_model::ensure_cuda_runtime_with_progress(&prog).map(|_| ())
                }
                #[cfg(target_os = "macos")]
                DownloadTarget::DiarizationMlx => {
                    hf_repos(&ds_model::mlx_repo::DIARIZATION_MLX_SET)
                }
                #[cfg(target_os = "macos")]
                target @ (DownloadTarget::KokoroMlx
                | DownloadTarget::ChatterboxMlx
                | DownloadTarget::QwenMlx
                | DownloadTarget::OmniVoiceMlx) => hf_repos(ds_model::mlx_repo::tts_mlx_set(
                    target.tts_model().expect("MLX TTS target has a model"),
                )),
                #[cfg(target_os = "macos")]
                DownloadTarget::ParakeetMlx => hf_repos(&ds_model::mlx_repo::PARAKEET_MLX_SET),
                // Fluid Kokoro: shared voices npz (owned by KokoroModel) + Core ML set.
                #[cfg(target_os = "macos")]
                DownloadTarget::KokoroFluid => {
                    ds_model::ensure_with_progress(&ds_model::kokoro_voices_spec(), &prog)
                        .and_then(|_| hf_repos(&ds_model::coreml_repo::KOKORO_COREML_SET))
                }
                #[cfg(target_os = "macos")]
                DownloadTarget::ParakeetFluid => {
                    hf_repos(&ds_model::coreml_repo::PARAKEET_COREML_SET)
                }
                #[cfg(target_os = "macos")]
                DownloadTarget::DiarizationFluid => {
                    hf_repos(&ds_model::coreml_repo::DIARIZATION_COREML_SET)
                }
                // Per-dictation re-resolve; no warm-child restart.
                #[cfg(target_os = "macos")]
                DownloadTarget::SepformerModel => {
                    ds_model::run_setup_sepformer_with_progress(&prog).map(|_| ())
                }
                _ => Err(std::io::Error::other(format!(
                    "'{}' is not an engine download target",
                    which.as_str()
                ))),
            }
        };
        match &result {
            Ok(()) => {
                log::info!(target: "engine", "{}", download_event_msg(which, "finished", None))
            }
            Err(e) => log::warn!(
                target: "engine",
                "{}",
                download_event_msg(which, "failed", Some(&e.to_string()))
            ),
        }
        finish_download(
            &mut dl.lock().unwrap_or_else(|e| e.into_inner()),
            which,
            &result,
        );
        // Detached thread can outlive stop — re-check; `None` = still running.
        let still_running = shutdown
            .as_ref()
            .map(|s| s.load(Ordering::Relaxed))
            .unwrap_or(true);
        if result.is_ok()
            && still_running
            && let (Some(tts), Some(paths)) = (warm, paths)
        {
            let cfg = VoiceConfig::load(&paths);
            // Pre-download pref had model absent (tts_preload=false); refresh before restart.
            tts.set_tts_wanted(crate::config_gate::helper_preloads_tts(&cfg));
            if download_needs_child_reload(which, &cfg) && tts.reload_models() {
                log::info!(
                    target: "engine",
                    "warm child restarted to load freshly-downloaded '{}'",
                    which.as_str()
                );
            }
        }
        // Separate from warm-child reload (engine Stt/Tts objects).
        if result.is_ok()
            && still_running
            && let Some(flag) = reload
        {
            flag.store(true, Ordering::Relaxed);
        }
    });
}

/// Host-gated diarization fetch (unsupported hosts must not loop). Pure.
fn diarization_needed(host_supported: bool, diarization_on: bool, set_present: bool) -> bool {
    host_supported && diarization_on && !set_present
}

/// Kokoro frontend missing while selected. Pure.
/// MLX always requests it; ONNX skips when full `KokoroModel` is already queued.
fn needs_kokoro_frontend(
    tts_is_kokoro: bool,
    frontend_assets_present: bool,
    downloading_full_kokoro_onnx: bool,
) -> bool {
    tts_is_kokoro && !frontend_assets_present && !downloading_full_kokoro_onnx
}

/// Enabled-but-missing flags (named fields — no positional transpose).
#[derive(Default)]
struct DownloadNeeds {
    tts_model: Option<DownloadTarget>,
    kokoro_frontend: bool,
    parakeet_model: bool,
    parakeet_mlx: bool,
    parakeet_fluid: bool,
    diarization_mlx: bool,
    diarization_fluid: bool,
    sepformer_model: bool,
}

/// Targets for `need`, start order TTS → frontend → STT (all kicked in parallel). Pure.
fn needed_downloads(need: &DownloadNeeds) -> Vec<DownloadTarget> {
    let mut targets = Vec::new();
    if let Some(target) = need.tts_model {
        targets.push(target);
    }
    if need.kokoro_frontend {
        targets.push(DownloadTarget::KokoroFrontend);
    }
    if need.parakeet_model {
        targets.push(DownloadTarget::ParakeetModel);
    }
    if need.parakeet_mlx {
        targets.push(DownloadTarget::ParakeetMlx);
    }
    if need.parakeet_fluid {
        targets.push(DownloadTarget::ParakeetFluid);
    }
    if need.diarization_mlx {
        targets.push(DownloadTarget::DiarizationMlx);
    }
    if need.diarization_fluid {
        targets.push(DownloadTarget::DiarizationFluid);
    }
    if need.sepformer_model {
        targets.push(DownloadTarget::SepformerModel);
    }
    targets
}

/// CUDA first when wanted, then missing models. Pure (pinned by test).
fn fetch_plan(prefetch_cuda: bool, need: &DownloadNeeds) -> Vec<DownloadTarget> {
    let mut plan = Vec::new();
    if prefetch_cuda {
        plan.push(DownloadTarget::Cuda);
    }
    plan.extend(needed_downloads(need));
    plan
}

/// Auto-fetch missing models (per-target permanent latch / transient backoff).
pub(crate) fn auto_download_missing(downloads: &DownloadProg, cfg: &VoiceConfig) {
    for which in fetch_plan(false, &compute_needs(cfg)) {
        start_download(downloads, which);
    }
}

/// Apply TTS provider, then [`fetch_plan`] (only CUDA-folding caller).
pub(crate) fn apply_provider_and_autofetch(
    tts: &Arc<TtsManager>,
    downloads: &DownloadProg,
    cfg: &VoiceConfig,
) {
    let prefetch_cuda = apply_tts_provider(tts, cfg, cfg.resolved_tts_provider());
    for which in fetch_plan(prefetch_cuda, &compute_needs(cfg)) {
        start_download(downloads, which);
    }
}

/// Live probe of missing sets. Impure; [`fetch_plan`] is pure.
fn compute_needs(cfg: &VoiceConfig) -> DownloadNeeds {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    // One roots resolve; `None` queues a fetch that fails on the same root.
    let roots = ds_model::ModelRoots::ambient();
    let set_present = |set: &[&'static ds_model::HfRepo]| {
        roots
            .as_ref()
            .is_some_and(|roots| ds_model::hf_repo::is_hf_set_present(roots, set))
    };
    let builtin_tts = cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn);
    let native_active = builtin_tts && native_tts_active(cfg);
    let fluid_active = native_active && cfg.resolved_tts_provider() == ds_config::Provider::Fluid;
    let mlx_active = native_active && !fluid_active;
    let kokoro_selected = builtin_tts && cfg.tts_model == ds_config::TtsModel::Kokoro;
    // Full frontend stack, not merely encoder.onnx (eSpeak gap on ONNX).
    let frontend_present = ds_model::is_kokoro_frontend_present();
    let tts_model = if !builtin_tts {
        None
    } else if fluid_active {
        // Core ML set + shared voices via `roots` (#212); half-fetched re-queues.
        let present = set_present(&ds_model::coreml_repo::KOKORO_COREML_SET)
            && roots
                .as_ref()
                .is_some_and(|r| r.model.join(ds_model::KOKORO_VOICES_FILE).is_file());
        (!present)
            .then(|| DownloadTarget::fluid_for_tts(cfg.tts_model))
            .flatten()
    } else if mlx_active {
        (!set_present(ds_model::mlx_repo::tts_mlx_set(cfg.tts_model)))
            .then(|| DownloadTarget::mlx_for_tts(cfg.tts_model))
    } else {
        let target = DownloadTarget::portable_for_tts(cfg.tts_model);
        // Match start_download's cuda-assets predicate.
        let cuda_assets = ds_model::tts_wants_cuda_assets(cfg.tts_model, cfg.tts_provider_token());
        (!(ds_model::tts_model_files_present(cfg.tts_model, cuda_assets)
            && exists(ds_model::onnxruntime_dylib_path())))
        .then_some(target)
    };
    let downloading_full_kokoro_onnx = tts_model == Some(DownloadTarget::KokoroModel);
    let kokoro_frontend = needs_kokoro_frontend(
        kokoro_selected,
        frontend_present,
        downloading_full_kokoro_onnx,
    );
    let stt_is_builtin = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn);
    let stt_onnx_runtime = stt_uses_onnx_runtime(
        cfg.resolved_stt_provider(),
        NativeShims::probe().unwrap_or_default(),
    );
    let stt_native = stt_is_builtin && !stt_onnx_runtime;
    let stt_fluid = stt_native && cfg.resolved_stt_provider() == ds_config::Provider::Fluid;
    let parakeet_model = stt_is_builtin && stt_onnx_runtime && !ds_model::is_parakeet_present();
    let parakeet_mlx =
        stt_native && !stt_fluid && !set_present(&ds_model::mlx_repo::PARAKEET_MLX_SET);
    let parakeet_fluid = stt_fluid && !set_present(&ds_model::coreml_repo::PARAKEET_COREML_SET);
    // Only the resolved diarizer rung's set (host gate stops off-Apple loops).
    let diar_provider = cfg.resolved_diarizer();
    let diarization_on = cfg.is_diarization_on();
    let diarization_mlx = diar_provider == ds_config::DiarizerProvider::Mlx
        && diarization_needed(
            DownloadTarget::DiarizationMlx.is_supported_on_this_host(),
            diarization_on,
            set_present(&ds_model::mlx_repo::DIARIZATION_MLX_SET),
        );
    let diarization_fluid = diar_provider == ds_config::DiarizerProvider::Fluid
        && diarization_needed(
            DownloadTarget::DiarizationFluid.is_supported_on_this_host(),
            diarization_on,
            set_present(&ds_model::coreml_repo::DIARIZATION_COREML_SET),
        );
    // Speaker-lock without SepFormer fails open.
    let sepformer_model = DownloadTarget::SepformerModel.is_supported_on_this_host()
        && cfg.speaker_lock
        && cfg.is_diarization_on()
        && !ds_model::is_sepformer_present();
    DownloadNeeds {
        tts_model,
        kokoro_frontend,
        parakeet_model,
        parakeet_mlx,
        parakeet_fluid,
        diarization_mlx,
        diarization_fluid,
        sepformer_model,
    }
}

/// Prefetch CUDA runtime only when wanted + driver + missing (`auto` never pulls). Pure.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn should_prefetch_cuda(wanted: bool, driver_present: bool, runtime_present: bool) -> bool {
    wanted && driver_present && !runtime_present
}

/// Apply provider; whether CUDA should enter [`fetch_plan`] (false off x86_64 Win/Linux).
fn apply_tts_provider(tts: &Arc<TtsManager>, cfg: &VoiceConfig, which: Provider) -> bool {
    // Token string only at the child edge; gating uses typed Provider.
    tts.set_provider(which.as_str());
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    {
        should_prefetch_cuda(
            ds_model::cuda_runtime_wanted(cfg),
            ds_model::is_cuda_driver_present(),
            ds_model::is_cuda_runtime_present(),
        )
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    {
        let _ = cfg;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTO_RETRY_BASE, AUTO_RETRY_CAP, DownloadNeeds, DownloadProgress, DownloadState, RetryGate,
        TargetState, begin_download, begin_download_at, compute_needs, diarization_needed,
        download_event_msg, download_needs_child_reload, fetch_plan, finish_download,
        finish_download_at, needed_downloads, needs_kokoro_frontend,
    };
    use ds_model::DownloadTarget;
    use std::time::{Duration, Instant};

    /// Pins CUDA-first boot order (old single-flight could drop Cuda when a model raced).
    #[test]
    fn fetch_plan_puts_cuda_first_then_all_models() {
        let needs = DownloadNeeds {
            tts_model: Some(DownloadTarget::KokoroModel),
            parakeet_model: true,
            ..Default::default()
        };
        assert_eq!(
            fetch_plan(true, &needs),
            vec![
                DownloadTarget::Cuda,
                DownloadTarget::KokoroModel,
                DownloadTarget::ParakeetModel,
            ]
        );
        assert_eq!(
            fetch_plan(false, &needs),
            vec![DownloadTarget::KokoroModel, DownloadTarget::ParakeetModel]
        );
        // Nothing missing + no CUDA wanted ⇒ empty pass.
        assert_eq!(fetch_plan(false, &DownloadNeeds::default()), vec![]);
        // CUDA alone (models already present) still fetches.
        assert_eq!(
            fetch_plan(true, &DownloadNeeds::default()),
            vec![DownloadTarget::Cuda]
        );
    }

    /// Runs in a child process holding the fixture environment — see [`crate::test_env`].
    #[test]
    fn corrupt_parakeet_files_remain_in_the_download_plan() {
        const TEST: &str = "downloads::tests::corrupt_parakeet_files_remain_in_the_download_plan";
        let Some(_child) = crate::test_env::child_run() else {
            let model_dir = tempfile::tempdir().unwrap();
            for file in [
                ds_model::PARAKEET_ENCODER_FILE,
                ds_model::PARAKEET_DECODER_FILE,
                ds_model::PARAKEET_JOINER_FILE,
                ds_model::PARAKEET_TOKENS_FILE,
            ] {
                std::fs::write(model_dir.path().join(file), b"corrupt but present").unwrap();
            }
            let runtime = model_dir.path().join("onnxruntime.dll");
            std::fs::write(&runtime, b"present runtime").unwrap();
            crate::test_env::run_child(
                TEST,
                crate::test_env::ChildEnv {
                    phase: "corrupt-files-present",
                    model_dir: model_dir.path(),
                    ort_dylib: Some(&runtime),
                },
            );
            return;
        };

        let config = ds_config::VoiceConfig {
            tts_engine: Some(Vec::new()),
            stt_engine: Some(vec![ds_config::SttEngine::BuiltIn]),
            provider: vec![ds_config::Provider::OrtCpu],
            ..ds_config::VoiceConfig::default()
        };

        assert!(
            compute_needs(&config).parakeet_model,
            "checksum-invalid files must be replaced even when every path exists"
        );
    }

    /// Runs in a child process holding the fixture environment — see [`crate::test_env`].
    #[test]
    fn checksum_invalid_tts_files_remain_in_the_download_plan() {
        const TEST: &str =
            "downloads::tests::checksum_invalid_tts_files_remain_in_the_download_plan";
        let Some(_child) = crate::test_env::child_run() else {
            let model_dir = tempfile::tempdir().unwrap();
            let model = ds_config::TtsModel::Chatterbox;
            let set = ds_model::tts_ort_asset_set(model);
            let dir = model_dir
                .path()
                .join(set.dir_name.expect("chatterbox subdirectory"));
            std::fs::create_dir_all(&dir).unwrap();
            for file in set.files_for(false) {
                std::fs::write(dir.join(file.file_name), b"stale but present").unwrap();
            }
            let runtime = model_dir.path().join("onnxruntime.dll");
            std::fs::write(&runtime, b"present runtime").unwrap();
            crate::test_env::run_child(
                TEST,
                crate::test_env::ChildEnv {
                    phase: "checksum-invalid-files-present",
                    model_dir: model_dir.path(),
                    ort_dylib: Some(&runtime),
                },
            );
            return;
        };

        let config = ds_config::VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: ds_config::TtsModel::Chatterbox,
            provider: vec![ds_config::Provider::OrtCpu],
            stt_engine: Some(Vec::new()),
            ..ds_config::VoiceConfig::default()
        };

        assert_eq!(
            compute_needs(&config).tts_model,
            Some(DownloadTarget::ChatterboxModel),
            "a complete-looking set with stale bytes must still reach the downloader"
        );
    }

    #[test]
    fn child_reload_is_model_and_provider_aware() {
        use ds_config::{Provider, TtsEngine, TtsModel, VoiceConfig};

        let config = |model| VoiceConfig {
            tts_engine: Some(vec![TtsEngine::BuiltIn]),
            tts_model: model,
            provider: vec![Provider::OrtCpu],
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        };
        let kokoro = config(TtsModel::Kokoro);
        assert!(download_needs_child_reload(
            DownloadTarget::KokoroModel,
            &kokoro
        ));
        assert!(!download_needs_child_reload(
            DownloadTarget::ChatterboxModel,
            &kokoro
        ));
        assert!(!download_needs_child_reload(DownloadTarget::Cuda, &kokoro));

        let chatterbox = config(TtsModel::Chatterbox);
        assert!(download_needs_child_reload(
            DownloadTarget::ChatterboxModel,
            &chatterbox
        ));
        assert!(!download_needs_child_reload(
            DownloadTarget::KokoroModel,
            &chatterbox
        ));
        assert!(!download_needs_child_reload(
            DownloadTarget::Cuda,
            &chatterbox
        ));
    }

    /// Diarization fetch need only when the host supports the target — both rungs share the
    /// predicate, so pin each target's host gate is Apple Silicon.
    #[test]
    fn diarization_need_is_gated_on_host_support() {
        for target in [
            DownloadTarget::DiarizationMlx,
            DownloadTarget::DiarizationFluid,
        ] {
            let host = target.is_supported_on_this_host();
            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            assert!(!host, "{target:?} diarization is Apple Silicon only");
            // Diarizer on + set absent: the need still follows host support.
            assert_eq!(diarization_needed(host, true, false), host);
        }
        assert!(!diarization_needed(false, true, false));
        assert!(!diarization_needed(true, false, false));
        assert!(!diarization_needed(true, true, true));
        assert!(diarization_needed(true, true, false));
    }

    #[test]
    fn kokoro_frontend_fetch_covers_mlx_and_onnx_gaps() {
        // Missing frontend, not already installing full ONNX Kokoro ⇒ fetch.
        assert!(
            needs_kokoro_frontend(true, false, false),
            "Kokoro selected + frontend missing ⇒ must fetch espeak/G2P/JA"
        );
        // Already present ⇒ idle.
        assert!(
            !needs_kokoro_frontend(true, true, false),
            "frontend assets already present ⇒ nothing to fetch"
        );
        // Full ONNX KokoroModel install includes the frontend — avoid double-fetch.
        assert!(
            !needs_kokoro_frontend(true, false, true),
            "KokoroModel download already installs the frontend"
        );
        // Weights present but eSpeak missing (ONNX gap we hit in production): still fetch.
        assert!(
            needs_kokoro_frontend(true, false, false),
            "ONNX weights without eSpeak still need KokoroFrontend"
        );
        assert!(
            !needs_kokoro_frontend(false, false, false),
            "non-Kokoro TTS needs no Kokoro frontend assets"
        );
    }

    #[test]
    fn needed_downloads_returns_all_targets_tts_first() {
        let need = |n: DownloadNeeds| needed_downloads(&n);
        assert_eq!(need(DownloadNeeds::default()), vec![]);
        // Parallel kick; Kokoro first only in start order.
        assert_eq!(
            need(DownloadNeeds {
                tts_model: Some(DownloadTarget::KokoroModel),
                parakeet_model: true,
                ..Default::default()
            }),
            vec![DownloadTarget::KokoroModel, DownloadTarget::ParakeetModel]
        );
        assert_eq!(
            need(DownloadNeeds {
                tts_model: Some(DownloadTarget::KokoroMlx),
                kokoro_frontend: true,
                parakeet_mlx: true,
                diarization_mlx: true,
                ..Default::default()
            }),
            vec![
                DownloadTarget::KokoroMlx,
                DownloadTarget::KokoroFrontend,
                DownloadTarget::ParakeetMlx,
                DownloadTarget::DiarizationMlx,
            ]
        );
        assert_eq!(
            need(DownloadNeeds {
                kokoro_frontend: true,
                parakeet_model: true,
                ..Default::default()
            }),
            vec![
                DownloadTarget::KokoroFrontend,
                DownloadTarget::ParakeetModel,
            ]
        );
        // Any selected TTS model slots before the STT targets.
        assert_eq!(
            need(DownloadNeeds {
                tts_model: Some(DownloadTarget::ChatterboxModel),
                parakeet_model: true,
                ..Default::default()
            }),
            vec![
                DownloadTarget::ChatterboxModel,
                DownloadTarget::ParakeetModel,
            ]
        );
        // A native flavor is carried by the same one-of field, so the Core ML variant needs
        // no plan shape of its own — only a `compute_needs` branch that selects it.
        assert_eq!(
            need(DownloadNeeds {
                tts_model: DownloadTarget::fluid_for_tts(ds_config::TtsModel::Kokoro),
                kokoro_frontend: true,
                ..Default::default()
            }),
            vec![DownloadTarget::KokoroFluid, DownloadTarget::KokoroFrontend]
        );
        // FluidAudio STT: its own STT target slots after any TTS, exactly where `parakeet_mlx`
        // would, and is mutually exclusive with it (one native rung per resolved provider).
        assert_eq!(
            need(DownloadNeeds {
                tts_model: DownloadTarget::fluid_for_tts(ds_config::TtsModel::Kokoro),
                parakeet_fluid: true,
                ..Default::default()
            }),
            vec![DownloadTarget::KokoroFluid, DownloadTarget::ParakeetFluid]
        );
        // The Fluid diarization set slots exactly where the MLX one does, and each is selected
        // by the resolved provider (never both), so the plan carries at most one.
        assert_eq!(
            need(DownloadNeeds {
                diarization_fluid: true,
                ..Default::default()
            }),
            vec![DownloadTarget::DiarizationFluid]
        );
    }

    /// The Fluid TTS variant must NOT restart the warm child yet: nothing resolves to that
    /// backend, so the child is hosting the ONNX or MLX model either way.
    #[test]
    fn a_fluid_tts_fetch_does_not_reload_a_child_that_cannot_host_it() {
        use ds_config::{Provider, TtsEngine, TtsModel, VoiceConfig};

        let kokoro = VoiceConfig {
            tts_engine: Some(vec![TtsEngine::BuiltIn]),
            tts_model: TtsModel::Kokoro,
            provider: vec![Provider::OrtCpu],
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        };
        assert_eq!(
            DownloadTarget::KokoroFluid.tts_model(),
            Some(TtsModel::Kokoro),
            "the target still names its model for the inventory row"
        );
        assert!(!download_needs_child_reload(
            DownloadTarget::KokoroFluid,
            &kokoro
        ));
        assert!(download_needs_child_reload(
            DownloadTarget::KokoroModel,
            &kokoro
        ));
    }

    /// Parallel targets: independent progress/errors; re-begin clears Failed.
    #[test]
    fn parallel_targets_track_independent_progress_and_errors() {
        let mut s = DownloadState::default();
        let (kok, par) = (DownloadTarget::KokoroModel, DownloadTarget::ParakeetModel);

        assert!(begin_download(&mut s, kok), "fresh target begins");
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress::default()),
            "a fresh begin starts Active at zero progress"
        );
        assert!(
            begin_download(&mut s, par),
            "second target begins IN PARALLEL"
        );
        s.targets.insert(
            kok,
            TargetState::Active(DownloadProgress {
                done: 10,
                total: 100,
            }),
        );
        assert!(!begin_download(&mut s, kok), "in-flight target attaches");
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress {
                done: 10,
                total: 100
            }),
            "attach must not reset the running fetch's progress"
        );

        s.targets.insert(
            par,
            TargetState::Active(DownloadProgress { done: 5, total: 50 }),
        );
        assert!(
            matches!(s.targets[&kok], TargetState::Active(p) if p.frac() == 0.1),
            "kokoro tracks its own fraction"
        );
        assert!(
            matches!(s.targets[&par], TargetState::Active(p) if p.frac() == 0.1),
            "parakeet tracks its own fraction"
        );

        let failure_time = Instant::now();
        finish_download_at(
            &mut s,
            par,
            &Err(std::io::Error::other("boom")),
            failure_time,
        );
        assert_eq!(s.targets[&par], TargetState::Failed("boom".into()));
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress {
                done: 10,
                total: 100
            }),
            "other target unaffected"
        );

        assert!(!begin_download_at(
            &mut s,
            par,
            failure_time + AUTO_RETRY_BASE - Duration::from_millis(1),
        ));
        assert!(begin_download_at(
            &mut s,
            par,
            failure_time + AUTO_RETRY_BASE,
        ));
        assert_eq!(
            s.targets[&par],
            TargetState::Active(DownloadProgress::default()),
            "a fresh begin over Failed drops the error"
        );

        finish_download(&mut s, kok, &Ok(()));
        assert_eq!(
            s.targets[&kok],
            TargetState::Done(DownloadProgress {
                done: 10,
                total: 100
            }),
            "a successful finish must retire the Active progress into Done"
        );
    }

    #[test]
    fn permanent_failures_latch_without_blocking_other_targets() {
        let now = Instant::now();
        let mut state = DownloadState::default();
        let kokoro = DownloadTarget::KokoroModel;
        let parakeet = DownloadTarget::ParakeetModel;
        assert!(begin_download_at(&mut state, kokoro, now));
        let failure = Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "checksum mismatch",
        ));
        finish_download_at(&mut state, kokoro, &failure, now);

        assert_eq!(state.retry_gates[&kokoro], RetryGate::Permanent);
        assert!(!begin_download_at(
            &mut state,
            kokoro,
            now + Duration::from_secs(24 * 60 * 60),
        ));
        assert!(begin_download_at(&mut state, parakeet, now));

        let missing = Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "repository missing",
        ));
        finish_download_at(&mut state, parakeet, &missing, now);
        assert_eq!(state.retry_gates[&parakeet], RetryGate::Permanent);
    }

    #[test]
    fn transient_failures_back_off_exponentially_and_success_clears_the_gate() {
        let now = Instant::now();
        let mut state = DownloadState::default();
        let target = DownloadTarget::ParakeetModel;
        let timeout = Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "offline"));

        assert!(begin_download_at(&mut state, target, now));
        finish_download_at(&mut state, target, &timeout, now);
        assert_eq!(
            state.retry_gates[&target],
            RetryGate::Transient {
                failures: 1,
                not_before: now + AUTO_RETRY_BASE,
            }
        );
        assert!(!begin_download_at(
            &mut state,
            target,
            now + AUTO_RETRY_BASE - Duration::from_millis(1),
        ));

        let second = now + AUTO_RETRY_BASE;
        assert!(begin_download_at(&mut state, target, second));
        finish_download_at(&mut state, target, &timeout, second);
        assert_eq!(
            state.retry_gates[&target],
            RetryGate::Transient {
                failures: 2,
                not_before: second + AUTO_RETRY_BASE * 2,
            }
        );
        assert_eq!(super::retry_delay(100), AUTO_RETRY_CAP);

        let due = second + AUTO_RETRY_BASE * 2;
        assert!(begin_download_at(&mut state, target, due));
        finish_download_at(&mut state, target, &Ok(()), due);
        assert!(!state.retry_gates.contains_key(&target));
        assert!(matches!(state.targets[&target], TargetState::Done(_)));
    }

    /// Stale Done must not linger as the ring % for a re-download.
    #[test]
    fn fresh_begin_replaces_prior_done() {
        let mut s = DownloadState::default();
        let kok = DownloadTarget::KokoroModel;

        assert!(begin_download(&mut s, kok));
        finish_download(&mut s, kok, &Ok(()));
        assert!(
            matches!(s.targets[&kok], TargetState::Done(_)),
            "setup: a successful finish must land in Done"
        );

        assert!(begin_download(&mut s, kok), "fresh start begins again");
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress::default()),
            "begin_download must replace a stale Done entry with a fresh Active one"
        );
    }

    /// Err always records Failed; Ok on absent leaves map unchanged.
    #[test]
    fn finish_on_non_active_target_records_error_but_not_done() {
        let mut s = DownloadState::default();
        let kok = DownloadTarget::KokoroModel;

        finish_download(&mut s, kok, &Ok(()));
        assert!(
            !s.targets.contains_key(&kok),
            "Ok on a never-begun target must not conjure a Done entry"
        );

        finish_download(&mut s, kok, &Err(std::io::Error::other("boom")));
        assert_eq!(
            s.targets[&kok],
            TargetState::Failed("boom".into()),
            "Err is always recorded, active or not"
        );
    }

    /// Done/Failed must not count as in-flight (else status gate bumps forever).
    #[test]
    fn any_active_ignores_done_and_failed_entries() {
        let mut s = DownloadState::default();
        let (kok, par) = (DownloadTarget::KokoroModel, DownloadTarget::ParakeetModel);
        assert!(!s.any_active(), "fresh state has nothing in flight");

        assert!(begin_download(&mut s, kok));
        assert!(s.any_active(), "an Active entry reads in flight");

        finish_download(&mut s, kok, &Ok(()));
        assert!(begin_download(&mut s, par));
        finish_download(&mut s, par, &Err(std::io::Error::other("boom")));
        assert!(
            !s.targets.is_empty() && !s.any_active(),
            "Done/Failed entries persist but must NOT read as in flight"
        );
    }

    #[test]
    fn download_event_msg_formats_phase_with_and_without_detail() {
        assert_eq!(
            download_event_msg(DownloadTarget::KokoroModel, "started", None),
            "model download (kokoro_model) started"
        );
        assert_eq!(
            download_event_msg(DownloadTarget::KokoroModel, "failed", Some("boom")),
            "model download (kokoro_model) failed: boom"
        );
    }

    /// Unknown total → 0.0 (not NaN); known total → byte ratio.
    #[test]
    fn download_progress_frac_handles_unknown_total() {
        assert_eq!(DownloadProgress::default().frac(), 0.0);
        assert_eq!(
            DownloadProgress {
                done: 25,
                total: 100
            }
            .frac(),
            0.25
        );
    }

    /// CUDA prefetch vetoes; which selections set `wanted` is
    /// `ds_model::cuda_runtime_wanted`'s own test.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn cuda_prefetch_requires_a_wanted_runtime_a_driver_and_an_absent_runtime() {
        use super::should_prefetch_cuda;

        assert!(should_prefetch_cuda(true, true, false));
        assert!(!should_prefetch_cuda(false, true, false));
        assert!(!should_prefetch_cuda(true, false, false));
        assert!(!should_prefetch_cuda(true, true, true));
    }
}
