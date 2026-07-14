//! Background model-download state + the auto-fetch / provider-apply orchestration.
//!
//! Downloads run in PARALLEL: each [`DownloadTarget`] fetches on its own thread with its
//! own progress entry, so the Kokoro and Parakeet rows (and any other target) advance
//! independently instead of queueing behind one shared `done/total` pair. Each target is
//! single-flight — a re-request while it's fetching ATTACHES (no-op here; progress is
//! observed via `model_status`) rather than retriggering it. Shared FILES between targets
//! (the onnxruntime dylib, the voices npz) are deduped one level down by ds-model's
//! per-path flight lock, so two targets never fetch the same bytes twice.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use ds_config::{Paths, Provider, VoiceConfig};
use ds_model::DownloadTarget;

use crate::config_gate::{
    apple_native_shim_available, apple_native_tts_active, stt_uses_onnx_runtime,
};
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

/// One target's download lifecycle. A target ABSENT from the map has never been
/// (re)started this session — absence IS the idle state (no explicit `Idle` variant:
/// `DownloadState::default()` is an empty map, and a second representation of "idle"
/// would reintroduce exactly the ambiguity this enum removes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetState {
    /// In flight, with its live byte progress — several targets download in
    /// parallel, each with its own `done/total`.
    Active(DownloadProgress),
    /// The most recent download of this target FAILED, with its error message —
    /// kept until a new download for that target starts.
    Failed(String),
    /// The most recent download SUCCEEDED, carrying its final progress — kept after
    /// the fetch retires so a row's ring reads its finished % (typically 100%)
    /// instead of falling through to an unrelated still-live fetch's progress (e.g.
    /// the shared Cuda runtime) the instant the row's own download completes.
    /// Replaced on a fresh [`begin_download`] for that target (a new fetch
    /// shouldn't show the PREVIOUS one's stale "done" progress).
    Done(DownloadProgress),
}

/// Background model-download progress, polled via `model_status` so the app's
/// status dots can show an orange progress ring (downloading) and a red dot
/// (a failed download).
#[derive(Default)]
pub(crate) struct DownloadState {
    /// Per-target download lifecycle. One entry per target that has ever started
    /// this session; downloading XOR failed XOR done is now structural — see
    /// [`TargetState`].
    pub targets: HashMap<DownloadTarget, TargetState>,
    /// Warm-child reload hook, wired ONCE at boot via [`wire`]: the warm-child
    /// owner plus the config paths. On a SUCCESSFUL download, [`start_download`] restarts the
    /// warm child iff it hosts the freshly-downloaded model (see [`download_needs_child_reload`])
    /// — the shared, cross-platform self-heal so a provider switch or a fresh install loads the
    /// new model WITHOUT a manual restart. Both `None` in tests / before boot wires them.
    pub warm: Option<Arc<TtsManager>>,
    pub paths: Option<Paths>,
    /// Engine hot-reload flag (the SAME `reload_requested` the boot poll loop reads). On a
    /// SUCCESSFUL download, [`start_download`] sets it so the daemon re-runs `build_stt`/
    /// `build_tts`: the dictation `Stt`/`Tts` engine SELECTION was decided at startup when the
    /// model may have been ABSENT (fresh install ⇒ Parakeet missing ⇒ `build_stt` falls to the
    /// `ds-engines` factory's INERT placeholder, no silent substitution), so without this nudge
    /// dictation stays inert even after Parakeet finishes downloading and the warm child loads
    /// it. `None` before boot wires it.
    pub reload: Option<Arc<AtomicBool>>,
    /// Shutdown observer, wired via [`wire`]: the SAME engine-lifetime `running`
    /// flag `ds-core`'s `engine_stop()` clears (to `false`) before it joins the engine thread.
    /// A background download is detached (see [`start_download`]) and can finish AFTER
    /// `ds_engine_stop()` has returned, i.e. after the caller already believes the engine is
    /// fully torn down — so its completion hook checks this flag and, once it reads `false`,
    /// skips every side-effecting action (restarting the warm child via `tts.reload_models()`,
    /// which can respawn a ds-helper child and reopen the mic, and nudging the daemon reload
    /// flag) instead of unconditionally acting. `None` (not wired) preserves the old
    /// unconditional behavior — needed so tests / a caller that never wires this keep working.
    pub shutdown: Option<Arc<AtomicBool>>,
}

impl DownloadState {
    /// True while ANY target is in flight — the poll loop's "keep nudging the
    /// status gate" predicate. NOT `targets.is_empty()`: Done/Failed entries
    /// persist after a fetch ends.
    pub fn any_active(&self) -> bool {
        self.targets
            .values()
            .any(|t| matches!(t, TargetState::Active(_)))
    }
}

pub(crate) type DownloadProg = Arc<Mutex<DownloadState>>;

/// Wire the warm-child reload hook AND the shutdown observer (call ONCE at boot, after the
/// warm-child owner exists). Merged into one call — both fields exist solely to feed the
/// SAME `DownloadState` at the SAME boot site, so wiring them separately meant taking the
/// `dl.lock()` mutex twice for one logical setup step.
///
/// `warm`/`paths`/`reload` let [`start_download`] restart the child to load a model that
/// finished downloading after the child was already started (a provider switch / fresh
/// install) — the SHARED self-heal used on every platform and by every download caller (see
/// [`download_needs_child_reload`]).
///
/// `running` is the SAME `Arc<AtomicBool>` that `ds-core`'s `host::engine_stop()` clears to
/// signal a graceful stop (the one already threaded through `boot::run`). It lets a
/// background download's completion hook ([`start_download`]) tell whether the engine it
/// would act on is still up, so a download that finishes after `ds_engine_stop()` has already
/// joined the engine thread becomes a no-op instead of respawning the warm child / nudging a
/// reload on an engine the caller believes is fully stopped.
/// The two engine-lifetime flags [`wire`] installs, named so the two same-typed
/// `Arc<AtomicBool>`s can't be transposed at the call site — the exact hazard this audit's
/// other bundling fixes (`RowState`/`SpawnPrefs`/`ListenerShared`) exist to prevent.
pub(crate) struct DownloadFlags {
    pub reload: Arc<AtomicBool>,
    pub running: Arc<AtomicBool>,
}

pub(crate) fn wire(dl: &DownloadProg, warm: Arc<TtsManager>, paths: Paths, flags: DownloadFlags) {
    let mut s = dl.lock().unwrap_or_else(|e| e.into_inner());
    s.warm = Some(warm);
    s.paths = Some(paths);
    s.reload = Some(flags.reload);
    s.shutdown = Some(flags.running);
}

/// Map a completed download `target` to whether the WARM CHILD hosts a model it produced —
/// the pure core of [`download_needs_child_reload`], split out so it is unit-testable on ANY
/// host without building a platform-resolved `VoiceConfig`. The warm child hosts Kokoro TTS
/// and/or Parakeet STT — on the ONNX path (the `*Model` targets) AND the apple-native Core ML
/// path (the `*Coreml` targets; the child's shim loads them offline, so it must restart to
/// pick up a fresh set). A `cuda` runtime fetch means whichever of those runs must restart to
/// bind the GPU execution provider. `diarization_coreml` (a separate Core ML path) and unknown
/// targets never touch the warm child.
fn target_hosts_engine(target: DownloadTarget, kokoro: bool, parakeet: bool) -> bool {
    match target {
        DownloadTarget::KokoroModel
        | DownloadTarget::KokoroFrontend
        | DownloadTarget::KokoroCoreml => kokoro,
        DownloadTarget::ParakeetModel | DownloadTarget::ParakeetCoreml => parakeet,
        DownloadTarget::Cuda => kokoro || parakeet,
        _ => false,
    }
}

/// Whether a just-COMPLETED download of `target` requires restarting the warm child so it
/// loads the freshly-arrived model(s). SHARED across platforms: the platform/provider
/// differences are already folded into `cfg.resolved_tts()` / `resolved_stt()`, so this
/// decision is identical everywhere — the only per-platform variance lives in those resolvers
/// (covered by their own tests). See [`target_hosts_engine`] for the pure mapping.
pub(crate) fn download_needs_child_reload(target: DownloadTarget, cfg: &VoiceConfig) -> bool {
    target_hosts_engine(
        target,
        cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro),
        cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn),
    )
}

/// Mark `which` as in flight, clearing its previous failure. `false` when this target is
/// ALREADY downloading — the caller attaches to that fetch (progress via `model_status`)
/// instead of retriggering it. Other targets' entries are untouched: downloads run in
/// parallel. Pure state transition, split out of [`start_download`] for unit tests.
fn begin_download(s: &mut DownloadState, which: DownloadTarget) -> bool {
    if matches!(s.targets.get(&which), Some(TargetState::Active(_))) {
        return false; // already downloading — attach, don't retrigger
    }
    // Inserting Active OVERWRITES any prior Done/Failed — a fresh fetch shouldn't
    // show the PREVIOUS one's stale error or finished "done" progress (the old
    // manual `last_error.remove` / `last_done.remove` are now one structural write).
    s.targets
        .insert(which, TargetState::Active(DownloadProgress::default()));
    true
}

/// Retire `which` from `Active`, recording its error message on failure, or (on
/// success) moving its final progress into `Done` so the row's ring can keep showing
/// its finished % instead of falling through to an unrelated still-live fetch (e.g. Cuda)
/// the instant it's retired. Pure counterpart of [`begin_download`].
fn finish_download(s: &mut DownloadState, which: DownloadTarget, result: &std::io::Result<()>) {
    match result {
        // An error is ALWAYS recorded, active or not (matching the old unconditional
        // `last_error` insert — an `Ok`-less caller path stays visible as a red dot).
        Err(e) => {
            s.targets.insert(which, TargetState::Failed(e.to_string()));
        }
        // Success only retires an Active fetch into Done, carrying its final
        // progress; Ok on a non-active target leaves state untouched (as before).
        Ok(()) => {
            if let Some(TargetState::Active(p)) = s.targets.get(&which) {
                let p = *p;
                s.targets.insert(which, TargetState::Done(p));
            }
        }
    }
}

/// Format one download lifecycle log line (`"started"` / `"finished"` / `"failed"`),
/// with an optional detail (e.g. the error string) appended after a colon. Pure, so
/// the exact wording is unit-testable without spawning a download thread.
fn download_event_msg(which: DownloadTarget, phase: &str, detail: Option<&str>) -> String {
    match detail {
        Some(d) => format!("model download ({}) {phase}: {d}", which.as_str()),
        None => format!("model download ({}) {phase}", which.as_str()),
    }
}

/// Kick off a background download for `which` (e.g. [`DownloadTarget::KokoroModel`] /
/// [`DownloadTarget::ParakeetModel`] / [`DownloadTarget::KokoroCoreml`]). Returns
/// immediately; progress is observed via `model_status`. Targets download in PARALLEL —
/// each on its own thread with its own progress entry; only a re-request of the SAME
/// in-flight target is a no-op (it attaches to the running fetch). Shared files between
/// targets (the onnxruntime dylib both ONNX model setups pull, the voices npz) are
/// deduped by ds-model's per-path flight lock — the second target waits, then finds the
/// file present and moves on.
pub(crate) fn start_download(dl: &DownloadProg, which: DownloadTarget) {
    if !begin_download(&mut dl.lock().unwrap_or_else(|e| e.into_inner()), which) {
        return; // this target is already downloading — attach, don't retrigger
    }
    log::info!(target: "engine", "{}", download_event_msg(which, "started", None));
    let dl = dl.clone();
    std::thread::spawn(move || {
        // Grab the warm-child reload hook up front (wired once at boot); used after the fetch.
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
            // Update ONLY this target's entry — concurrent targets own their own. A late
            // callback after `finish_download` no longer matches `Active`, so it can't
            // resurrect a retired entry's progress.
            if let Some(TargetState::Active(p)) = dl
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .targets
                .get_mut(&which)
            {
                *p = DownloadProgress { done, total };
            }
        };
        // Platform availability is decided by the ONE shared predicate
        // (`DownloadTarget::is_supported_on_this_host`), not per-arm `cfg(not(...))` error
        // branches each spelling its own gate — the `cfg`-gated arms below then only exist
        // where their fetcher compiles, and an off-platform request errors uniformly here
        // (a red dot + log line, never a wrong fetch).
        let result: std::io::Result<()> = if !which.is_supported_on_this_host() {
            Err(std::io::Error::other(format!(
                "'{}' is not available on this platform",
                which.as_str()
            )))
        } else {
            match which {
                DownloadTarget::KokoroModel => {
                    ds_model::run_setup_kokoro_with_progress(&prog).map(|_| ())
                }
                // Shared frontend assets for ANE: voices, OOV G2P graphs, and ORT, but not the
                // 310 MB portable synth graph. Requested by `EnsureKokoroFrontend`.
                DownloadTarget::KokoroFrontend => {
                    ds_model::run_setup_kokoro_frontend_with_progress(&prog).map(|_| ())
                }
                DownloadTarget::ParakeetModel => {
                    ds_model::run_setup_parakeet_with_progress(&prog).map(|_| ())
                }
                // Shared GPU runtime (~1.4 GB) for the ONNX CUDA EP — drives BOTH engines. Folded
                // in here (not a bespoke thread in `apply_tts_provider`) so the completion hook
                // below restarts the warm child onto the GPU UNIFORMLY, exactly like a model fetch.
                #[cfg(all(
                    any(target_os = "windows", target_os = "linux"),
                    target_arch = "x86_64"
                ))]
                DownloadTarget::Cuda => {
                    ds_model::ensure_cuda_runtime_with_progress(&prog).map(|_| ())
                }
                // Diarization Core ML models — we fetch them OURSELVES (real %) into the dir the
                // shim loads from offline, like Kokoro/Parakeet. macOS-only (ANE shim).
                #[cfg(target_os = "macos")]
                DownloadTarget::DiarizationCoreml => ds_model::coreml_repo::ensure_coreml_repo(
                    &ds_model::coreml_repo::DIARIZATION_COREML,
                    &prog,
                ),
                // Apple-native Kokoro / Parakeet Core ML sets — the SAME standard download path as
                // every other target (single-flight, real %, error surfaced, warm child restarted
                // on completion), NOT a helper-side self-fetch. One byte-weighted bar per set;
                // FluidAudio then only LOADS them (enforceOffline). macOS-only (ANE shim).
                #[cfg(target_os = "macos")]
                DownloadTarget::KokoroCoreml => ds_model::coreml_repo::ensure_coreml_repos(
                    &ds_model::coreml_repo::KOKORO_COREML_SET,
                    &prog,
                ),
                #[cfg(target_os = "macos")]
                DownloadTarget::ParakeetCoreml => ds_model::coreml_repo::ensure_coreml_repos(
                    &ds_model::coreml_repo::PARAKEET_COREML_SET,
                    &prog,
                ),
                // The SepFormer speaker-lock separator (~29 MB ONNX + the shared dylib) —
                // macOS-only (the lock path is macOS code); fetched into the flat
                // model_dir() like the Parakeet set. The listen-side loader re-resolves
                // the path per dictation, so no warm-child restart is needed on completion.
                #[cfg(target_os = "macos")]
                DownloadTarget::SepformerModel => {
                    ds_model::run_setup_sepformer_with_progress(&prog).map(|_| ())
                }
                // Onnxruntime / Models are installer-prefetch tokens; Dotnet / Winapp are
                // retained no-op wire tokens from before the Windows package became
                // self-contained. No engine caller passes any of them here. Error instead of
                // guessing a fetch so a future misroute surfaces as a red dot + log line.
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
        // This thread is DETACHED and can outlive `ds_engine_stop()` (which only joins the
        // top-level engine thread), so it can still be running here after the caller already
        // believes the engine is fully torn down. Re-check the shutdown observer right before
        // any side-effecting completion action: `false` means shutdown has been signaled, so
        // skip restarting the warm child (which can respawn a ds-helper child and reopen the
        // mic) and skip nudging the daemon reload flag. `None` (never wired) reads as "still
        // running" so a caller that doesn't wire this keeps today's unconditional behavior.
        let still_running = shutdown
            .as_ref()
            .map(|s| s.load(Ordering::Relaxed))
            .unwrap_or(true);
        // SHARED self-heal (every platform, every caller): the warm child may have been started
        // BEFORE this model existed (a provider switch or a fresh install), so it couldn't load
        // it. Now that the fetch succeeded, restart the child so it picks the model up — no
        // manual restart needed. No-op when the child is stopped or the target isn't one it
        // hosts (`download_needs_child_reload`). Config is read LIVE so a mid-download config
        // change is honored.
        if result.is_ok()
            && still_running
            && let (Some(tts), Some(paths)) = (warm, paths)
            && download_needs_child_reload(which, &VoiceConfig::load(&paths))
            && tts.reload_models()
        {
            // Supplementary detail AFTER the unconditional "finished" line logged above —
            // not the sole success signal anymore.
            log::info!(
                target: "engine",
                "warm child restarted to load freshly-downloaded '{}'",
                which.as_str()
            );
        }
        // Also nudge a DAEMON reload so `build_stt`/`build_tts` re-run: the engine's dictation
        // Stt/Tts SELECTION was fixed at startup, when this model may have been absent (fresh
        // install ⇒ Parakeet missing ⇒ `build_stt` fell to the `ds-engines` factory's INERT
        // placeholder — no silent substitution). Reloading swaps it to the local Parakeet/Kokoro
        // path now that the files exist — otherwise dictation stays inert even with the model
        // downloaded + the warm child loaded. Separate from the warm-child reload above (that
        // reloads the INFERENCE child, not the engine's Stt object).
        if result.is_ok()
            && still_running
            && let Some(flag) = reload
        {
            flag.store(true, Ordering::Relaxed);
        }
    });
}

/// The ANE synthesis graph still consumes DontSpeak's shared Rust frontend. Its voice tensors,
/// OOV graphs, and ORT therefore travel together under the `KokoroFrontend` target.
/// Pure so the backend-specific fetch policy stays unit-testable.
fn ane_needs_frontend_assets(
    tts_is_kokoro: bool,
    ane_active: bool,
    frontend_assets_present: bool,
) -> bool {
    tts_is_kokoro && ane_active && !frontend_assets_present
}

/// The computed "engine X is enabled but its files are missing" flags that
/// [`auto_download_missing`] maps to download targets. Named fields (not positional bools)
/// so a caller/test can't silently transpose two needs. The ONNX and Core ML flavors of
/// one engine are mutually exclusive by construction (gated on `ane_active`).
#[derive(Default)]
struct DownloadNeeds {
    // Named after the DownloadTarget each maps to (kokoro_model ⇒ KokoroModel, …), so the
    // flag and the target it kicks can't read as two different things.
    kokoro_model: bool,
    kokoro_coreml: bool,
    kokoro_frontend: bool,
    parakeet_model: bool,
    parakeet_coreml: bool,
    sepformer_model: bool,
}

/// EVERY download target the computed needs call for, in start order (TTS first — it
/// gates the engine entirely — then the shared frontend assets that gate the ACTIVE TTS
/// voice, then STT). The order only decides who begins first: ALL of them are kicked at
/// once and download in parallel, each status row tracking its own target's progress.
/// Empty ⇒ nothing missing. Pure/testable.
fn needed_downloads(need: &DownloadNeeds) -> Vec<DownloadTarget> {
    let mut targets = Vec::new();
    if need.kokoro_model {
        targets.push(DownloadTarget::KokoroModel);
    }
    if need.kokoro_coreml {
        targets.push(DownloadTarget::KokoroCoreml);
    }
    if need.kokoro_frontend {
        targets.push(DownloadTarget::KokoroFrontend);
    }
    if need.parakeet_model {
        targets.push(DownloadTarget::ParakeetModel);
    }
    if need.parakeet_coreml {
        targets.push(DownloadTarget::ParakeetCoreml);
    }
    if need.sepformer_model {
        targets.push(DownloadTarget::SepformerModel);
    }
    targets
}

/// The COMPLETE fetch plan for one boot/reload pass, in start order: the shared CUDA
/// runtime FIRST when the provider apply calls for it (it gates BOTH engines' compute
/// — and the old global single-flight used to silently drop it when a model fetch won
/// the race), then every missing model target. The order only decides who begins first;
/// everything downloads in parallel. Pure — this is the ONE spelling of the boot
/// download sequence, pinned by test.
fn fetch_plan(prefetch_cuda: bool, need: &DownloadNeeds) -> Vec<DownloadTarget> {
    let mut plan = Vec::new();
    if prefetch_cuda {
        plan.push(DownloadTarget::Cuda);
    }
    plan.extend(needed_downloads(need));
    plan
}

/// Full-auto model fetch: when an engine is ENABLED but a model file it needs is missing,
/// kick off the background download so first activation just works — there is no manual
/// Download button. ALL missing targets start at once and download in parallel. Idempotent
/// (file-presence gated here; [`start_download`] attaches to a target already in flight).
/// Covers EVERY DontSpeak-managed model set the warm child hosts: the
/// ONNX models, the apple-native Core ML sets (Kokoro chain + G2P, Parakeet EOU + fallback —
/// the warm child no longer self-fetches; FluidAudio only LOADS, enforceOffline), and the
/// Kokoro frontend assets on the ANE path (see [`ane_needs_frontend_assets`]). Called on startup, on
/// every config reload, and on a slow poll-loop tick (so a download that failed — e.g. no
/// network at launch — retries without any user action).
pub(crate) fn auto_download_missing(downloads: &DownloadProg, cfg: &VoiceConfig) {
    for which in fetch_plan(false, &compute_needs(cfg)) {
        start_download(downloads, which);
    }
}

/// One boot / config-reload pass: apply the resolved TTS provider to the warm child,
/// then kick the COMPLETE fetch plan — the shared CUDA runtime first when the provider
/// apply calls for it, then every missing model target — all downloading in parallel.
/// The start order is [`fetch_plan`]'s (pure, pinned by test); this is the only caller
/// that folds the provider's CUDA decision into the plan.
pub(crate) fn apply_provider_and_autofetch(
    tts: &Arc<TtsManager>,
    downloads: &DownloadProg,
    cfg: &VoiceConfig,
) {
    let prefetch_cuda = apply_tts_provider(tts, cfg.resolved_tts_provider());
    for which in fetch_plan(prefetch_cuda, &compute_needs(cfg)) {
        start_download(downloads, which);
    }
}

/// The live "engine is enabled but its files are missing" probe for the current config —
/// which model sets need fetching, from on-disk presence. Impure (stats the model files
/// and completion markers); the plan built from the result ([`fetch_plan`]) is the pure,
/// tested part.
fn compute_needs(cfg: &VoiceConfig) -> DownloadNeeds {
    let exists = |p: Option<std::path::PathBuf>| p.map(|p| p.is_file()).unwrap_or(false);
    // ANE only actually serves Kokoro when the shim dylib is present. `uses_apple_native_model()`
    // is arch-BLIND (resolves to ANE on ANY macOS incl. Intel), so on its own it would skip the
    // ONNX fetch; without the shim (Intel, or no SMKOKORO_DYLIB_PATH) the warm child falls back
    // to the ONNX path and needs those files instead. `apple_native_tts_active` is the shim-aware
    // runtime truth (shared with the status row), so exactly ONE flavor (ONNX vs Core ML) is
    // fetched — never both, never neither.
    let tts_is_kokoro = cfg.resolved_tts() == Some(ds_config::TtsEngine::Kokoro);
    let ane_active = apple_native_tts_active(cfg);
    let kokoro_model = tts_is_kokoro
        && !ane_active
        && !(exists(ds_model::model_path(ds_model::KOKORO_ONNX_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_G2P_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_G2P_DECODER_FILE))
            && exists(ds_model::onnxruntime_dylib_path()));
    // The ANE flavor: the Kokoro Core ML chain and its FluidAudio initialization assets,
    // revision-pinned completion markers the downloader writes (`is_coreml_set_present`), so a
    // partial fetch or a stale pin re-downloads instead of reading "present".
    let kokoro_coreml = tts_is_kokoro
        && ane_active
        && !ds_model::coreml_repo::is_coreml_set_present(&ds_model::coreml_repo::KOKORO_COREML_SET);
    // The Core ML chain ships only `af_heart.bin`, so the shared `voices-v1.0.bin` npz (the
    // source for EVERY other voice) must still be fetched on the ANE path — else any
    // configured voice ≠ af_heart silently degrades to af_heart at synth time
    // (`synth_coreml` materializes from this npz, never downloads it).
    let kokoro_frontend = ane_needs_frontend_assets(
        tts_is_kokoro,
        ane_active,
        exists(ds_model::model_path(ds_model::KOKORO_VOICES_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_G2P_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::KOKORO_G2P_DECODER_FILE))
            && exists(ds_model::onnxruntime_dylib_path()),
    );
    // SAME arch-blind trap as Kokoro above (STT resolves to `Ane` on ANY Mac): gating the ONNX
    // fetch on `provider ∈ {OrtCpu,OrtCuda}` skips it on Intel, but without the shim the built-in
    // recognizer runs the ONNX Parakeet path and needs these files. `stt_uses_onnx_runtime` is the
    // shared shim-aware truth (also used by `parakeet_present_for` + the status row) so the right
    // flavor auto-downloads — else dictation is dead: `encoder.int8.onnx: No such file`.
    let stt_is_builtin = cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn);
    let stt_onnx_runtime =
        stt_uses_onnx_runtime(cfg.resolved_stt_provider(), apple_native_shim_available());
    let parakeet_model = stt_is_builtin
        && stt_onnx_runtime
        && !(exists(ds_model::model_path(ds_model::PARAKEET_ENCODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_DECODER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_JOINER_FILE))
            && exists(ds_model::model_path(ds_model::PARAKEET_TOKENS_FILE))
            && exists(ds_model::onnxruntime_dylib_path()));
    // The ANE flavor: the streaming EOU set + the offline fallback, marker-gated like Kokoro.
    let parakeet_coreml = stt_is_builtin
        && !stt_onnx_runtime
        && !ds_model::coreml_repo::is_coreml_set_present(
            &ds_model::coreml_repo::PARAKEET_COREML_SET,
        );
    // The SepFormer separator: fetched when the SPEAKER-LOCK feature is actually on (lock
    // enabled + diarization on) and the model file is absent. Without it the lock silently
    // fails open (transcribes unfiltered) — this is what makes "turn the lock on" enough,
    // with no manual download step. macOS-only, like the lock path that consumes it.
    let sepformer_model = cfg!(target_os = "macos")
        && cfg.stt_speaker_lock
        && cfg.is_diarization_on()
        && !exists(ds_model::model_path(ds_model::SEPFORMER_FILE));
    DownloadNeeds {
        kokoro_model,
        kokoro_coreml,
        kokoro_frontend,
        parakeet_model,
        parakeet_coreml,
        sepformer_model,
    }
}

/// Whether to kick off the one-time ~1.4 GB CUDA-runtime prefetch: ONLY when the resolved
/// provider IS the CUDA rung, an NVIDIA driver is actually present, and the runtime isn't already
/// on disk. The runtime is SHARED by both engines (one fetch aligns Kokoro TTS and Parakeet STT
/// onto the GPU); `auto` is intentionally EXCLUDED — it uses the GPU only when the runtime is
/// already present and never pulls the large download silently. A pure decision (no probes / IO)
/// so it's unit-tested directly — the caller supplies the live
/// [`ds_model::is_cuda_driver_present`] / [`ds_model::is_cuda_runtime_present`] results. Typed
/// on [`Provider`], so the CUDA rung is matched by the enum variant, never a stray `"cuda"`
/// string literal (the regression this guards against). Platform-gated to where the runtime exists.
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn should_prefetch_cuda(which: Provider, driver_present: bool, runtime_present: bool) -> bool {
    which == Provider::OrtCuda && driver_present && !runtime_present
}

/// Apply the provider to the warm child and REPORT whether the shared CUDA runtime
/// should be prefetched — the caller ([`apply_provider_and_autofetch`]) folds that into
/// [`fetch_plan`], so the download start order lives in one pure, tested function
/// instead of a side-effecting kick here. The ~1.4 GB fetch is wanted only when the
/// resolved provider IS the CUDA rung AND an ACTUAL NVIDIA driver is present (a live
/// `LoadLibrary`/`dlopen` probe of `nvcuda.dll`/`libcuda.so.1` — a box that merely
/// RESOLVES to CUDA but has no GPU never pulls the runtime; the warm child falls back
/// to CPU) AND the runtime isn't already on disk. Always `false` off x86_64
/// Windows/Linux, where the CUDA runtime doesn't exist.
fn apply_tts_provider(tts: &Arc<TtsManager>, which: Provider) -> bool {
    // The warm child / FFI boundary still speaks the canonical token string; convert here, at the
    // edge, NOT before — the gating logic below compares the typed `Provider`, never a literal.
    tts.set_provider(which.as_str());
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    {
        should_prefetch_cuda(
            which,
            ds_model::is_cuda_driver_present(),
            ds_model::is_cuda_runtime_present(),
        )
    }
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    false
}

#[cfg(test)]
mod tests {
    use super::{
        DownloadNeeds, DownloadProgress, DownloadState, TargetState, ane_needs_frontend_assets,
        begin_download, download_event_msg, fetch_plan, finish_download, needed_downloads,
        target_hosts_engine,
    };
    use ds_model::DownloadTarget;

    /// The boot download sequence for a CUDA box, pinned: the shared CUDA runtime is
    /// FIRST in the plan (the old global single-flight silently DROPPED it when a model
    /// fetch won the race), followed by every missing model target — and without the
    /// provider's CUDA decision the plan is exactly the model list.
    #[test]
    fn fetch_plan_puts_cuda_first_then_all_models() {
        let needs = DownloadNeeds {
            kokoro_model: true,
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
    fn target_hosts_engine_maps_downloads_to_warm_child() {
        // The SHARED, platform-agnostic restart decision: given which engines the warm child
        // resolves to host (kokoro / parakeet booleans — the per-platform part lives in
        // `resolved_tts`/`resolved_stt`, tested separately), a completed download target maps
        // to "must restart the child" iff the child hosts a model that target produced.

        // Kokoro targets (full ONNX model, shared frontend assets, AND apple-native Core ML
        // set) restart iff Kokoro TTS runs.
        for t in [
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroFrontend,
            DownloadTarget::KokoroCoreml,
        ] {
            assert!(target_hosts_engine(t, true, false), "{t:?} (tts)");
            assert!(!target_hosts_engine(t, false, true), "{t:?} (stt only)");
        }

        // The Parakeet targets (ONNX and Core ML) restart iff the built-in (Parakeet) STT runs.
        for t in [
            DownloadTarget::ParakeetModel,
            DownloadTarget::ParakeetCoreml,
        ] {
            assert!(target_hosts_engine(t, false, true), "{t:?} (stt)");
            assert!(!target_hosts_engine(t, true, false), "{t:?} (tts only)");
        }

        // The shared CUDA runtime restarts iff EITHER engine runs — both engines share the
        // warm child and the compute provider.
        let cuda = DownloadTarget::Cuda;
        assert!(target_hosts_engine(cuda, true, false), "cuda (tts only)");
        assert!(target_hosts_engine(cuda, false, true), "cuda (stt only)");
        assert!(target_hosts_engine(cuda, true, true), "cuda (both)");
        assert!(!target_hosts_engine(cuda, false, false), "cuda (neither)");

        // Diarization is a SEPARATE Core ML path (not the warm child), and the SepFormer
        // separator is loaded per-dictation by the listen path (which re-resolves the model
        // path itself); other non-hosting targets (the bare runtime / installer groups)
        // never trigger a restart even when both engines run.
        for t in [
            DownloadTarget::DiarizationCoreml,
            DownloadTarget::SepformerModel,
            DownloadTarget::Onnxruntime,
            DownloadTarget::Models,
        ] {
            assert!(!target_hosts_engine(t, true, true), "{t:?}");
        }
    }

    #[test]
    fn ane_path_still_needs_the_shared_frontend_assets() {
        // The crux: the apple-native (ANE / Core ML) Kokoro chain self-manages, but it ships
        // only af_heart. The shared voices npz (the source for every OTHER voice, e.g.
        // af_nicole) must STILL be fetched on the ANE path, or the chosen voice silently
        // falls back to af_heart at synth time.
        assert!(
            ane_needs_frontend_assets(true, true, false),
            "ANE active + frontend assets missing ⇒ must fetch them"
        );
        assert!(
            !ane_needs_frontend_assets(true, true, true),
            "frontend assets already present ⇒ nothing to fetch"
        );
        // ONNX path (ane_active=false): the npz rides along with the full ONNX `kokoro_model`
        // fetch, so the ANE-specific trigger must stay OFF to avoid a redundant download.
        assert!(
            !ane_needs_frontend_assets(true, false, false),
            "ONNX path fetches the frontend via kokoro_model, not this trigger"
        );
        // TTS isn't Kokoro at all ⇒ no Kokoro assets needed.
        assert!(
            !ane_needs_frontend_assets(false, true, false),
            "non-Kokoro TTS needs no Kokoro frontend assets"
        );
    }

    #[test]
    fn needed_downloads_returns_all_targets_tts_first() {
        let need = |n: DownloadNeeds| needed_downloads(&n);
        // Nothing missing ⇒ nothing to fetch.
        assert_eq!(need(DownloadNeeds::default()), vec![]);
        // ONNX first boot: BOTH models are kicked at once (they download in parallel,
        // each row tracking its own %); Kokoro is merely FIRST in start order.
        assert_eq!(
            need(DownloadNeeds {
                kokoro_model: true,
                parakeet_model: true,
                ..Default::default()
            }),
            vec![DownloadTarget::KokoroModel, DownloadTarget::ParakeetModel]
        );
        // ANE first boot: all three at once — the Kokoro Core ML chain leads the start
        // order (it gates TTS entirely), then the voices npz, then Parakeet.
        assert_eq!(
            need(DownloadNeeds {
                kokoro_coreml: true,
                kokoro_frontend: true,
                parakeet_coreml: true,
                ..Default::default()
            }),
            vec![
                DownloadTarget::KokoroCoreml,
                DownloadTarget::KokoroFrontend,
                DownloadTarget::ParakeetCoreml,
            ]
        );
        // Voices-only (chain already present): the small npz is still fetched — it gates
        // the ACTIVE TTS voice — alongside Parakeet.
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
    }

    /// The crux of parallel downloads: two targets active AT ONCE, each owning its own
    /// progress entry; finishing one (with an error) records the error for THAT target
    /// only and leaves the other in flight; re-beginning a target clears its error.
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
        // A re-request of an in-flight target attaches (no restart, progress kept).
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

        // Independent progress per target.
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

        // Parakeet fails: ITS error is recorded; Kokoro is still downloading, untouched.
        // The variant IS `Failed` — no Active progress, no Done progress — structurally,
        // where the old maps only promised it by begin/finish discipline.
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

        // Re-beginning the failed target clears its error (fresh attempt).
        assert!(begin_download(&mut s, par));
        assert_eq!(
            s.targets[&par],
            TargetState::Active(DownloadProgress::default()),
            "a fresh begin over Failed drops the error"
        );

        // Kokoro succeeds: retired into `Done`, carrying its FINAL progress (so the
        // row's ring keeps reading its finished % afterward).
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

    /// `begin_download` on a target with a stale `Done` entry (from a PREVIOUS
    /// successful download) must replace it — a fresh fetch shouldn't show the previous
    /// download's finished % before its own progress arrives.
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

        // A fresh begin (e.g. re-download after the file was removed) replaces the stale entry.
        assert!(begin_download(&mut s, kok), "fresh start begins again");
        assert_eq!(
            s.targets[&kok],
            TargetState::Active(DownloadProgress::default()),
            "begin_download must replace a stale Done entry with a fresh Active one"
        );
    }

    /// The preserved non-active-finish edge: `finish_download` with an `Err` on a target
    /// ABSENT from the map still records `Failed` (the old unconditional `last_error`
    /// insert), while an `Ok` on an absent target leaves the map unchanged.
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

    /// `any_active` is the boot poll loop's "keep nudging the status gate" predicate —
    /// it must read false once every fetch has ENDED, even though Done/Failed entries
    /// persist in the map (`!targets.is_empty()` would bump the gate forever after the
    /// session's first download).
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

    /// `frac()` guards the unknown-total window (0.0, not NaN/inf) and reports the
    /// byte ratio once the total is known.
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

    /// REGRESSION GUARD (fcf2072): the ~1.4 GB CUDA prefetch is gated on the TYPED
    /// `Provider::OrtCuda` variant — never a `"cuda"` string literal that a typo or rename
    /// could silently break. Driver-absent and runtime-present must both veto the fetch.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn cuda_prefetch_requires_cuda_rung_driver_and_absent_runtime() {
        use super::should_prefetch_cuda;
        use ds_config::Provider;

        // The CUDA rung + a real NVIDIA driver + runtime not yet on disk ⇒ fetch.
        assert!(should_prefetch_cuda(Provider::OrtCuda, true, false));
        // No driver ⇒ never fetch (the live probe fcf2072 added is the whole point).
        assert!(!should_prefetch_cuda(Provider::OrtCuda, false, false));
        // Runtime already present ⇒ nothing to fetch.
        assert!(!should_prefetch_cuda(Provider::OrtCuda, true, true));
        // Every NON-CUDA rung ⇒ never fetch, even with a driver and no runtime.
        for p in [Provider::OrtCpu, Provider::OrtCoreMl, Provider::Ane] {
            assert!(
                !should_prefetch_cuda(p, true, false),
                "{p:?} must not prefetch"
            );
        }
    }
}
