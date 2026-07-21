//! Background model-download state + auto-fetch / provider-apply orchestration.
//!
//! Targets fetch in PARALLEL (own thread + progress entry each). Single-flight per
//! target: a re-request ATTACHES (progress via `model_status`) rather than retriggering.
//! Shared files (onnxruntime dylib, voices npz) are deduped by ds-model's per-path
//! flight lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ds_config::{Paths, Provider, VoiceConfig};
use ds_model::DownloadTarget;

use crate::config_gate::{mlx_shim_available, mlx_tts_active, stt_uses_onnx_runtime};
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

/// One target's lifecycle. Absent from the map = never started this session
/// (absence IS idle — no `Idle` variant, which would reintroduce ambiguity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetState {
    /// In flight with live byte progress (parallel targets each own theirs).
    Active(DownloadProgress),
    /// Last fetch failed; kept until a new download for this target starts.
    Failed(String),
    /// Last fetch succeeded — keep final progress so the row's ring doesn't fall
    /// through to an unrelated still-live fetch (e.g. Cuda). Replaced on a fresh
    /// [`begin_download`].
    Done(DownloadProgress),
}

/// Background download progress for `model_status` (orange ring / red failed dot).
#[derive(Default)]
pub(crate) struct DownloadState {
    /// Per-target lifecycle (Active XOR Failed XOR Done). See [`TargetState`].
    pub targets: HashMap<DownloadTarget, TargetState>,
    /// Warm-child self-heal hook (wired once at boot via [`wire`]). On success,
    /// [`start_download`] restarts the child iff it hosts the new model
    /// ([`download_needs_child_reload`]). `None` until boot / in tests.
    pub warm: Option<Arc<TtsManager>>,
    pub paths: Option<Paths>,
    /// Same `reload_requested` the boot loop reads. On success, set so the daemon
    /// re-runs `build_stt`/`build_tts`: selection at startup may have fallen to the
    /// inert `ds-engines` placeholder when the model was absent (no silent
    /// substitution). Without this, dictation stays inert after download.
    pub reload: Option<Arc<AtomicBool>>,
    /// Same `running` flag `ds-core`'s `engine_stop()` clears. Detached downloads can
    /// finish after stop — completion then skips warm-child restart / reload nudge.
    /// `None` ⇒ unconditional (tests / unwired callers).
    pub shutdown: Option<Arc<AtomicBool>>,
}

impl DownloadState {
    /// Any target in flight — poll loop's "nudge status gate" predicate.
    /// Not `targets.is_empty()`: Done/Failed persist after fetch ends.
    pub fn any_active(&self) -> bool {
        self.targets
            .values()
            .any(|t| matches!(t, TargetState::Active(_)))
    }
}

pub(crate) type DownloadProg = Arc<Mutex<DownloadState>>;

/// Engine-lifetime flags for [`wire`] — named so the two same-typed `Arc<AtomicBool>`s
/// can't be transposed (same hazard as `RowState`/`SpawnPrefs`/`ListenerShared`).
pub(crate) struct DownloadFlags {
    pub reload: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
}

/// Wire warm-child reload + shutdown observer once at boot (after the warm child exists).
/// One call / one mutex take for both fields. See [`DownloadState`] field docs.
pub(crate) fn wire(dl: &DownloadProg, warm: Arc<TtsManager>, paths: Paths, flags: DownloadFlags) {
    let mut s = dl.lock().unwrap_or_else(|e| e.into_inner());
    s.warm = Some(warm);
    s.paths = Some(paths);
    s.reload = Some(flags.reload);
    s.shutdown = Some(flags.running);
}

/// Restart warm child after a completed download so it loads the new model(s).
pub(crate) fn download_needs_child_reload(target: DownloadTarget, cfg: &VoiceConfig) -> bool {
    let builtin_tts = cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn);
    // Shim-aware (same predicate as [`compute_needs`]): provider=mlx without the loaded
    // shim runs ONNX, so the ONNX model download is the one the child actually hosts.
    let mlx_tts = mlx_tts_active(cfg);
    let tts_target_matches =
        target.tts_model() == Some(cfg.tts_model) && target.is_mlx_tts() == mlx_tts;
    (builtin_tts && tts_target_matches)
        || (mlx_tts
            && cfg.tts_model == ds_config::TtsModel::Kokoro
            && target == DownloadTarget::KokoroFrontend)
        || (cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
            && matches!(
                target,
                DownloadTarget::ParakeetModel | DownloadTarget::ParakeetMlx
            ))
        || (target == DownloadTarget::Cuda
            && ((builtin_tts && cfg.resolved_tts_provider() == Provider::OrtCuda)
                || (cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
                    && cfg.resolved_stt_provider() == Provider::OrtCuda)))
}

/// Mark `which` Active (clears prior Failed/Done). `false` if already Active — attach.
/// Pure; split out of [`start_download`] for unit tests.
fn begin_download(s: &mut DownloadState, which: DownloadTarget) -> bool {
    if matches!(s.targets.get(&which), Some(TargetState::Active(_))) {
        return false; // already downloading — attach, don't retrigger
    }
    // Active overwrites prior Done/Failed (no stale ring / error).
    s.targets
        .insert(which, TargetState::Active(DownloadProgress::default()));
    true
}

/// Retire `which`: Err → Failed always; Ok only Active → Done (keeps final %).
fn finish_download(s: &mut DownloadState, which: DownloadTarget, result: &std::io::Result<()>) {
    match result {
        // Always record Err (active or not) so a red-dot path stays visible.
        Err(e) => {
            s.targets.insert(which, TargetState::Failed(e.to_string()));
        }
        // Ok on non-Active leaves state untouched.
        Ok(()) => {
            if let Some(TargetState::Active(p)) = s.targets.get(&which) {
                let p = *p;
                s.targets.insert(which, TargetState::Done(p));
            }
        }
    }
}

/// Lifecycle log line (`started`/`finished`/`failed` [+ detail]). Pure for unit tests.
fn download_event_msg(which: DownloadTarget, phase: &str, detail: Option<&str>) -> String {
    match detail {
        Some(d) => format!("model download ({}) {phase}: {d}", which.as_str()),
        None => format!("model download ({}) {phase}", which.as_str()),
    }
}

/// Kick off a background download for `which` (returns immediately; see crate doc).
pub(crate) fn start_download(dl: &DownloadProg, which: DownloadTarget) {
    if !begin_download(&mut dl.lock().unwrap_or_else(|e| e.into_inner()), which) {
        return; // this target is already downloading — attach, don't retrigger
    }
    log::info!(target: "engine", "{}", download_event_msg(which, "started", None));
    let dl = dl.clone();
    std::thread::spawn(move || {
        // Hooks cloned under lock; used only after fetch completes.
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
            // Only this target; late callbacks after finish no longer match Active.
            if let Some(TargetState::Active(p)) = dl
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .targets
                .get_mut(&which)
            {
                *p = DownloadProgress { done, total };
            }
        };
        // `which` carries no provider, so the effective one is read from config here. It MUST
        // match `compute_needs`: a narrower set leaves presence unsatisfied and the boot
        // autofetch re-queues this target forever.
        let cuda_assets = |model: ds_config::TtsModel| {
            paths.as_ref().is_some_and(|paths| {
                ds_model::tts_wants_cuda_assets(
                    model,
                    VoiceConfig::load(paths).tts_provider_token(),
                )
            })
        };
        // One shared host gate — not per-arm cfg error strings (uniform red-dot path).
        let result: std::io::Result<()> = if !which.is_supported_on_this_host() {
            Err(std::io::Error::other(format!(
                "'{}' is not available on this platform",
                which.as_str()
            )))
        } else {
            match which {
                DownloadTarget::KokoroModel => ds_model::run_setup_tts_model_with_progress(
                    ds_config::TtsModel::Kokoro,
                    cuda_assets(ds_config::TtsModel::Kokoro),
                    &prog,
                )
                .map(|_| ()),
                // MLX frontend: vocabulary + OOV G2P + ORT (not the synth graph).
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
                // Shared CUDA EP runtime (~1.4 GB) — same completion hook as model fetch.
                #[cfg(all(
                    any(target_os = "windows", target_os = "linux"),
                    target_arch = "x86_64"
                ))]
                DownloadTarget::Cuda => {
                    ds_model::ensure_cuda_runtime_with_progress(&prog).map(|_| ())
                }
                // MLX diarization — engine-managed fetch (real %), offline shim load.
                #[cfg(target_os = "macos")]
                DownloadTarget::DiarizationMlx => ds_model::mlx_repo::ensure_mlx_repos(
                    &ds_model::mlx_repo::DIARIZATION_MLX_SET,
                    &prog,
                ),
                // MLX sets: standard path (not helper self-fetch).
                #[cfg(target_os = "macos")]
                target @ (DownloadTarget::KokoroMlx
                | DownloadTarget::ChatterboxMlx
                | DownloadTarget::QwenMlx
                | DownloadTarget::OmniVoiceMlx) => ds_model::mlx_repo::ensure_mlx_repos(
                    ds_model::mlx_repo::tts_mlx_set(
                        target.tts_model().expect("MLX TTS target has a model"),
                    ),
                    &prog,
                ),
                #[cfg(target_os = "macos")]
                DownloadTarget::ParakeetMlx => ds_model::mlx_repo::ensure_mlx_repos(
                    &ds_model::mlx_repo::PARAKEET_MLX_SET,
                    &prog,
                ),
                // SepFormer speaker-lock — re-resolved per dictation; no warm-child restart.
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
        // Detached thread can outlive `ds_engine_stop()` — re-check shutdown before
        // side effects. `None` (unwired) reads as still running. See `DownloadState::shutdown`.
        let still_running = shutdown
            .as_ref()
            .map(|s| s.load(Ordering::Relaxed))
            .unwrap_or(true);
        // Self-heal: restart warm child if it hosts this target (config read LIVE).
        if result.is_ok()
            && still_running
            && let (Some(tts), Some(paths)) = (warm, paths)
        {
            let cfg = VoiceConfig::load(&paths);
            // Refresh the preload pref first: the pre-download pref was computed with
            // the model absent (tts_preload=false), so restarting with it would leave
            // TTS unloaded and force a second restart on the daemon-reload pass below.
            tts.set_tts_wanted(crate::config_gate::helper_preloads_tts(&cfg));
            if download_needs_child_reload(which, &cfg) && tts.reload_models() {
                log::info!(
                    target: "engine",
                    "warm child restarted to load freshly-downloaded '{}'",
                    which.as_str()
                );
            }
        }
        // Daemon reload: rebuild Stt/Tts selection (inert placeholder → real model).
        // Separate from warm-child reload above (inference child vs engine Stt object).
        if result.is_ok()
            && still_running
            && let Some(flag) = reload
        {
            flag.store(true, Ordering::Relaxed);
        }
    });
}

/// MLX diarization fetch only on hosts that can run it (Apple Silicon) — elsewhere an
/// enabled diarizer must not loop a doomed fetch on an unsupported target. Pure for tests.
fn diarization_mlx_needed(host_supported: bool, diarization_on: bool, set_present: bool) -> bool {
    host_supported && diarization_on && !set_present
}

/// MLX Kokoro still needs shared Rust frontend assets (`KokoroFrontend`). Pure for tests.
fn mlx_needs_frontend_assets(
    tts_is_kokoro: bool,
    mlx_active: bool,
    frontend_assets_present: bool,
) -> bool {
    tts_is_kokoro && mlx_active && !frontend_assets_present
}

/// "Enabled but files missing" flags → download targets. Named (not positional) so needs
/// can't transpose. ONNX vs MLX are mutually exclusive via `mlx_active`.
#[derive(Default)]
struct DownloadNeeds {
    tts_model: Option<DownloadTarget>,
    kokoro_frontend: bool,
    parakeet_model: bool,
    parakeet_mlx: bool,
    diarization_mlx: bool,
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
    if need.diarization_mlx {
        targets.push(DownloadTarget::DiarizationMlx);
    }
    if need.sepformer_model {
        targets.push(DownloadTarget::SepformerModel);
    }
    targets
}

/// Boot/reload fetch plan: CUDA first when wanted (gates both engines; old single-flight
/// could drop it when a model won the race), then missing models. Pure, pinned by test.
fn fetch_plan(prefetch_cuda: bool, need: &DownloadNeeds) -> Vec<DownloadTarget> {
    let mut plan = Vec::new();
    if prefetch_cuda {
        plan.push(DownloadTarget::Cuda);
    }
    plan.extend(needed_downloads(need));
    plan
}

/// Auto-fetch missing models for enabled engines (no manual Download button).
/// Idempotent; retries on boot, reload, and a slow poll-loop tick.
pub(crate) fn auto_download_missing(downloads: &DownloadProg, cfg: &VoiceConfig) {
    for which in fetch_plan(false, &compute_needs(cfg)) {
        start_download(downloads, which);
    }
}

/// Boot/reload: apply TTS provider, then full [`fetch_plan`] (only CUDA-folding caller).
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

/// Live probe: which model sets need fetching. Impure; [`fetch_plan`] is the pure part.
fn compute_needs(cfg: &VoiceConfig) -> DownloadNeeds {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    let builtin_tts = cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn);
    let mlx_active = builtin_tts && mlx_tts_active(cfg);
    let tts_model = if !builtin_tts {
        None
    } else if mlx_active {
        (!ds_model::mlx_repo::is_mlx_set_present(ds_model::mlx_repo::tts_mlx_set(cfg.tts_model)))
            .then(|| DownloadTarget::mlx_for_tts(cfg.tts_model))
    } else {
        let target = DownloadTarget::portable_for_tts(cfg.tts_model);
        // Same effective-provider predicate `start_download` fetches with, or the pair loops.
        let cuda_assets = ds_model::tts_wants_cuda_assets(cfg.tts_model, cfg.tts_provider_token());
        (!(ds_model::tts_model_files_present(cfg.tts_model, cuda_assets)
            && exists(ds_model::onnxruntime_dylib_path())))
        .then_some(target)
    };
    // Shared Rust frontend assets provide the vocabulary and phonemizer.
    let kokoro_frontend = mlx_needs_frontend_assets(
        builtin_tts && cfg.tts_model == ds_config::TtsModel::Kokoro,
        mlx_active,
        exists(ds_model::model_path(ds_model::KOKORO_G2P_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_G2P_DECODER_FILE))
            && exists(ds_model::onnxruntime_dylib_path()),
    );
    // Same arch-blind trap for STT; `stt_uses_onnx_runtime` is the shim-aware truth.
    let stt_is_builtin = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn);
    let stt_onnx_runtime = stt_uses_onnx_runtime(cfg.resolved_stt_provider(), mlx_shim_available());
    let parakeet_model = stt_is_builtin
        && stt_onnx_runtime
        && !(exists(ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_DECODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_JOINER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_TOKENS_FILE))
            && exists(ds_model::onnxruntime_dylib_path()));
    let parakeet_mlx = stt_is_builtin
        && !stt_onnx_runtime
        && !ds_model::mlx_repo::is_mlx_set_present(&ds_model::mlx_repo::PARAKEET_MLX_SET);
    let diarization_mlx = diarization_mlx_needed(
        DownloadTarget::DiarizationMlx.is_supported_on_this_host(),
        cfg.is_diarization_on(),
        ds_model::mlx_repo::is_mlx_set_present(&ds_model::mlx_repo::DIARIZATION_MLX_SET),
    );
    // Speaker-lock on + model absent: without it lock fails open (unfiltered).
    let sepformer_model = cfg!(target_os = "macos")
        && cfg.speaker_lock
        && cfg.is_diarization_on()
        && !exists(ds_model::model_path(ds_model::SEPFORMER_FILE));
    DownloadNeeds {
        tts_model,
        kokoro_frontend,
        parakeet_model,
        parakeet_mlx,
        diarization_mlx,
        sepformer_model,
    }
}

/// Prefetch ~1.4 GB CUDA runtime only on `Provider::OrtCuda` + real driver + missing runtime.
/// `auto` excluded (never silent large pull). Typed Provider — never a `"cuda"` string.
/// Pure; caller supplies live probe results. Platform-gated to where the runtime exists.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn should_prefetch_cuda(
    which: Provider,
    has_cuda_consumer: bool,
    driver_present: bool,
    runtime_present: bool,
) -> bool {
    which == Provider::OrtCuda && has_cuda_consumer && driver_present && !runtime_present
}

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn cuda_prefetch_provider(
    tts_provider: Provider,
    stt_provider: Provider,
    stt_consumer: bool,
) -> Provider {
    if stt_consumer && stt_provider == Provider::OrtCuda {
        Provider::OrtCuda
    } else {
        tts_provider
    }
}

/// Apply provider to warm child; report whether CUDA runtime should enter [`fetch_plan`].
/// Always false off x86_64 Windows/Linux.
fn apply_tts_provider(tts: &Arc<TtsManager>, cfg: &VoiceConfig, which: Provider) -> bool {
    // Token string only at the child/FFI edge; gating uses typed Provider.
    tts.set_provider(which.as_str());
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    {
        let tts_consumer = cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
            && cfg
                .tts_model_descriptor()
                .supports_provider(ds_config::Provider::OrtCuda);
        let stt_consumer = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
            && cfg.resolved_stt_provider() == Provider::OrtCuda;
        let cuda_provider =
            cuda_prefetch_provider(which, cfg.resolved_stt_provider(), stt_consumer);
        should_prefetch_cuda(
            cuda_provider,
            tts_consumer || stt_consumer,
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
        DownloadNeeds, DownloadProgress, DownloadState, TargetState, begin_download,
        diarization_mlx_needed, download_event_msg, download_needs_child_reload, fetch_plan,
        finish_download, mlx_needs_frontend_assets, needed_downloads,
    };
    use ds_model::DownloadTarget;

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

    /// `diarization_mlx` need only when host supports the target.
    #[test]
    fn diarization_mlx_need_is_gated_on_host_support() {
        let host = DownloadTarget::DiarizationMlx.is_supported_on_this_host();
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        assert!(!host, "MLX diarization is Apple Silicon only");
        // Diarizer on + set absent: the need still follows host support.
        assert_eq!(diarization_mlx_needed(host, true, false), host);
        assert!(!diarization_mlx_needed(false, true, false));
        assert!(!diarization_mlx_needed(true, false, false));
        assert!(!diarization_mlx_needed(true, true, true));
        assert!(diarization_mlx_needed(true, true, false));
    }

    #[test]
    fn mlx_path_still_needs_the_shared_frontend_assets() {
        // MLX uses the shared Rust frontend assets.
        assert!(
            mlx_needs_frontend_assets(true, true, false),
            "MLX active + frontend assets missing ⇒ must fetch them"
        );
        assert!(
            !mlx_needs_frontend_assets(true, true, true),
            "frontend assets already present ⇒ nothing to fetch"
        );
        // ONNX pulls frontend via kokoro_model — don't double-fetch.
        assert!(
            !mlx_needs_frontend_assets(true, false, false),
            "ONNX path fetches the frontend via kokoro_model, not this trigger"
        );
        assert!(
            !mlx_needs_frontend_assets(false, true, false),
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

        finish_download(&mut s, par, &Err(std::io::Error::other("boom")));
        assert_eq!(s.targets[&par], TargetState::Failed("boom".into()));
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress {
                done: 10,
                total: 100
            }),
            "other target unaffected"
        );

        assert!(begin_download(&mut s, par));
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

    /// CUDA prefetch: typed `Provider::OrtCuda` only; driver/runtime vetoes.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn cuda_prefetch_requires_cuda_rung_driver_and_absent_runtime() {
        use super::{cuda_prefetch_provider, should_prefetch_cuda};
        use ds_config::Provider;

        assert!(should_prefetch_cuda(Provider::OrtCuda, true, true, false));
        assert!(!should_prefetch_cuda(Provider::OrtCuda, false, true, false));
        assert!(!should_prefetch_cuda(Provider::OrtCuda, true, false, false));
        assert!(!should_prefetch_cuda(Provider::OrtCuda, true, true, true));
        for p in [Provider::OrtCpu, Provider::Mlx] {
            assert!(
                !should_prefetch_cuda(p, true, true, false),
                "{p:?} must not prefetch"
            );
        }

        assert_eq!(
            cuda_prefetch_provider(Provider::OrtCpu, Provider::OrtCuda, true),
            Provider::OrtCuda,
            "CUDA STT must prefetch even when the selected TTS model is CPU-only"
        );
        assert_eq!(
            cuda_prefetch_provider(Provider::OrtCpu, Provider::OrtCuda, false),
            Provider::OrtCpu
        );
    }
}
