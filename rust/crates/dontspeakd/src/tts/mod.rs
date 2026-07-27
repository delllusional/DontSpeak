//! Warm `ds-helper --serve` owner for built-in TTS and local STT residency.
//!
//! Enable spawns, disable kills. Full-duplex: one reader demuxes stdout into
//! [`SpeakSlot`] / [`ListenSlot`]; `stop` needs only the brief stdin lock.
//! Protocol: `ds_helper` / `ds-helper-proto`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use ds_helper_proto as proto;

use crate::child_slot::ChildSlot;
use crate::model_slot::{ModelSlot, ModelState};
use crate::status::StatusGate;

mod reader;
use reader::*;

const SPEAK_TERMINAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// CUEDONE wait: exceed long custom cues (~30 s); reap-on-timeout closes late races.
const CUE_TERMINAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const CAPTURE_TERMINAL_GRACE: u64 = 25;
/// Pre-READY bound (issue #59: silent-but-alive child).
const READY_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Last-resort helper stderr → aux log beside engine log (abort/panic/pre-init).
fn helper_stderr(engine_log_file: &std::path::Path) -> Stdio {
    ds_log::open_aux_log(engine_log_file, "ds-helper.log")
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null)
}

/// `Some` set, `None` clear (block ambient `DONTSPEAK_*` leak).
fn child_env(prefs: &SpawnPrefs) -> [(&'static str, Option<String>); 6] {
    [
        ("DONTSPEAK_PROVIDER", Some(prefs.provider.clone())),
        (
            "DONTSPEAK_TTS_MODEL",
            Some(prefs.tts_model.as_str().to_string()),
        ),
        ("DONTSPEAK_STT_PROVIDER", Some(prefs.stt_provider.clone())),
        (
            "DONTSPEAK_FULL_DUPLEX",
            prefs.full_duplex.then(|| "1".to_string()),
        ),
        (
            "DONTSPEAK_STT_PRELOAD",
            prefs.stt_preload.then(|| "1".to_string()),
        ),
        (
            "DONTSPEAK_TTS_PRELOAD",
            prefs.tts_preload.then(|| "1".to_string()),
        ),
    ]
}

/// Next-child spawn prefs (one mutex; always r/w together). See [`child_env`].
#[derive(Clone)]
struct SpawnPrefs {
    provider: String,
    tts_model: ds_config::TtsModel,
    stt_provider: String,
    full_duplex: bool,
    /// `DONTSPEAK_STT_PRELOAD` — provider alone is not on/off.
    stt_preload: bool,
    /// Built-in TTS + output open (STT-only helper skips).
    tts_preload: bool,
}

/// Test-only ctor knobs (prefer here over post-construction setters).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TtsManagerTestOptions {
    finalize_timeout: Option<std::time::Duration>,
    ready_timeout: Option<std::time::Duration>,
    cue_timeout: Option<std::time::Duration>,
    /// First-spawn env only (READY-wedge tests without mutating manager/global env).
    first_spawn_env: Mutex<Option<Vec<(String, String)>>>,
}

#[cfg(test)]
impl TtsManagerTestOptions {
    pub(crate) fn with_finalize_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.finalize_timeout = Some(timeout);
        self
    }

    pub(crate) fn with_ready_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.ready_timeout = Some(timeout);
        self
    }

    pub(crate) fn with_cue_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.cue_timeout = Some(timeout);
        self
    }

    pub(crate) fn with_first_spawn_env(mut self, env: &[(&str, &str)]) -> Self {
        self.first_spawn_env = Mutex::new(Some(
            env.iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        ));
        self
    }
}

pub struct TtsManager {
    bin: PathBuf,
    helper_log_file: PathBuf,
    /// Lifecycle (start/stop/mark_dead). Not held across slot Condvar waits.
    lifecycle: Mutex<()>,
    last_restart: Mutex<Option<std::time::Instant>>,
    /// Live child + gen + deliberate teardown; shared with reader.
    child: Arc<ChildSlot>,
    stdin: Mutex<Option<ChildStdin>>,
    /// Joined after kill so next start doesn't race slots.
    reader: Mutex<Option<JoinHandle<()>>>,
    speak_slot: Arc<(Mutex<SpeakSlot>, Condvar)>,
    cue_slot: Arc<(Mutex<CueSlot>, Condvar)>,
    /// Separate from speak (speak+listen on one stdout).
    listen_slot: Arc<(Mutex<ListenSlot>, Condvar)>,
    /// One untagged listen (tests fail busy vs stealing Caps).
    listen_lease: Mutex<()>,
    cue_lease: Mutex<()>,
    /// Stop cancels through gen N (early stop vs delayed start).
    next_listen_generation: AtomicU64,
    active_listen_generation: AtomicU64,
    listen_stopped_through: AtomicU64,
    listen_stop_started: Mutex<Option<(u64, std::time::Instant)>>,
    #[cfg(test)]
    test_options: TtsManagerTestOptions,
    diarize_slot: Arc<(Mutex<DiarizeSlot>, Condvar)>,
    enroll_slot: Arc<(Mutex<EnrollSlot>, Condvar)>,
    /// System TTS one-shot.
    say_child: Mutex<Option<Child>>,
    /// START failure while no child installed.
    last_error: Mutex<Option<String>>,
    stats: Arc<crate::stats::TtsStats>,
    stt_stats: Arc<crate::stats::SttStats>,
    lifetime: Arc<crate::stats::LifetimeSeconds>,
    /// Realized TTS EP from `PROVIDER` — never a guess (`None` until reported).
    tts_realized: Arc<Mutex<Option<String>>>,
    /// Realized STT EP from `STT_PROVIDER` — never a guess.
    stt_realized: Arc<Mutex<Option<String>>>,
    spawn_prefs: Mutex<SpawnPrefs>,
    full_duplex_active: Mutex<bool>,
    stt_provider_active: Mutex<String>,
    tts_wanted_active: Mutex<bool>,
    tts_selection_active: Mutex<Option<ds_config::TtsModel>>,
    /// Arc so reader unloads on unexpected EOF.
    tts_model: Arc<ModelSlot>,
    stt_model: Arc<ModelSlot>,
    muted: AtomicBool,
    gate: OnceLock<Arc<StatusGate>>,
    last_heal: Mutex<Option<std::time::Instant>>,
}

/// Crash-heal spacing (deterministic crasher vs transient kill).
const HEAL_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

impl TtsManager {
    #[cfg(not(test))]
    pub fn new(
        bin: PathBuf,
        helper_log_file: PathBuf,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
    ) -> Self {
        Self::new_inner(bin, helper_log_file, stats, stt_stats, lifetime)
    }

    #[cfg(test)]
    pub fn new(
        bin: PathBuf,
        helper_log_file: PathBuf,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
    ) -> Self {
        Self::new_for_test(
            bin,
            helper_log_file,
            stats,
            stt_stats,
            lifetime,
            TtsManagerTestOptions::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        bin: PathBuf,
        helper_log_file: PathBuf,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
        test_options: TtsManagerTestOptions,
    ) -> Self {
        Self::new_inner(
            bin,
            helper_log_file,
            stats,
            stt_stats,
            lifetime,
            test_options,
        )
    }

    fn new_inner(
        bin: PathBuf,
        helper_log_file: PathBuf,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
        #[cfg(test)] test_options: TtsManagerTestOptions,
    ) -> Self {
        Self {
            bin,
            helper_log_file,
            lifecycle: Mutex::new(()),
            last_restart: Mutex::new(None),
            child: Arc::new(ChildSlot::new()),
            stdin: Mutex::new(None),
            reader: Mutex::new(None),
            speak_slot: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
            cue_slot: Arc::new((Mutex::new(CueSlot::default()), Condvar::new())),
            listen_slot: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
            listen_lease: Mutex::new(()),
            cue_lease: Mutex::new(()),
            next_listen_generation: AtomicU64::new(1),
            active_listen_generation: AtomicU64::new(0),
            listen_stopped_through: AtomicU64::new(0),
            listen_stop_started: Mutex::new(None),
            #[cfg(test)]
            test_options,
            diarize_slot: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
            enroll_slot: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            say_child: Mutex::new(None),
            last_error: Mutex::new(None),
            stats,
            stt_stats,
            lifetime,
            tts_realized: Arc::new(Mutex::new(None)),
            stt_realized: Arc::new(Mutex::new(None)),
            spawn_prefs: Mutex::new(SpawnPrefs {
                provider: "auto".to_string(),
                tts_model: ds_config::TtsModel::Kokoro,
                stt_provider: "mlx".to_string(),
                full_duplex: false,
                stt_preload: false,
                tts_preload: false,
            }),
            full_duplex_active: Mutex::new(false),
            stt_provider_active: Mutex::new(String::new()),
            tts_wanted_active: Mutex::new(false),
            tts_selection_active: Mutex::new(None),
            tts_model: Arc::new(ModelSlot::new()),
            stt_model: Arc::new(ModelSlot::new()),
            muted: AtomicBool::new(false),
            gate: OnceLock::new(),
            last_heal: Mutex::new(None),
        }
    }

    /// Install status gate once at boot (mute bumps WaitModelStatus).
    pub fn set_status_gate(&self, gate: Arc<StatusGate>) {
        let _ = self.gate.set(gate);
    }

    /// Full-duplex AEC active (VPIO mic always live — skip half-duplex mic gates).
    pub fn is_full_duplex_active(&self) -> bool {
        *self.full_duplex_active.lock().unwrap()
    }

    pub fn is_tts_loaded(&self) -> bool {
        self.tts_model.is_loaded()
    }
    pub fn is_stt_loaded(&self) -> bool {
        self.stt_model.is_loaded()
    }

    /// Last STTLOADERR (parakeet row).
    pub fn stt_load_error(&self) -> Option<String> {
        self.stt_model.error()
    }
    /// Last TTSLOADERR.
    pub fn tts_load_error(&self) -> Option<String> {
        self.tts_model.error()
    }
    /// Change-gated setter for [`stt_load_error`](Self::stt_load_error) — see
    /// [`ModelSlot::transition`].
    fn set_stt_load_error(&self, msg: impl Into<String>) {
        self.stt_model.transition(
            ModelState::Failed(msg.into()),
            self.gate.get().map(|g| g.as_ref()),
        );
    }
    /// Change-gated clear for [`stt_load_error`](Self::stt_load_error) — see
    /// [`ModelSlot::clear_error`].
    fn clear_stt_load_error(&self) {
        self.stt_model
            .clear_error(self.gate.get().map(|g| g.as_ref()));
    }
    /// Change-gated setter for [`tts_load_error`](Self::tts_load_error) — see
    /// [`ModelSlot::transition`].
    fn set_tts_load_error(&self, msg: impl Into<String>) {
        self.tts_model.transition(
            ModelState::Failed(msg.into()),
            self.gate.get().map(|g| g.as_ref()),
        );
    }
    /// Change-gated clear for [`tts_load_error`](Self::tts_load_error) — see
    /// [`ModelSlot::clear_error`].
    pub(crate) fn clear_tts_load_error(&self) {
        self.tts_model
            .clear_error(self.gate.get().map(|g| g.as_ref()));
    }
    /// Realized TTS EP (`PROVIDER` line).
    pub fn provider(&self) -> Option<String> {
        self.tts_realized.lock().unwrap().clone()
    }

    /// Realized STT EP (`STT_PROVIDER`); counterpart to [`provider`](Self::provider).
    pub fn stt_realized_provider(&self) -> Option<String> {
        self.stt_realized.lock().unwrap().clone()
    }

    pub fn selected_tts_model(&self) -> ds_config::TtsModel {
        self.spawn_prefs.lock().unwrap().tts_model
    }

    /// Set provider pref; restart only when the resolved EP differs. Returns whether restarted.
    pub fn set_provider(&self, which: &str) -> bool {
        let (model, tts_preload) = {
            let mut prefs = self.spawn_prefs.lock().unwrap();
            prefs.provider = which.to_string();
            (prefs.tts_model, prefs.tts_preload)
        };
        let resolved = Self::resolve_provider(which, model);
        if !self.is_running() {
            return false; // takes effect on next start; nothing active to change
        }
        if !provider_restart_needed(
            tts_preload,
            resolved,
            self.provider()
                .as_deref()
                .map(ds_config::RealizedProvider::parse),
        ) {
            return false; // same provider, or no ready TTS model is resident
        }
        self.restart_child();
        true
    }

    /// Restart warm child + reset both engines' stats (shared built-in TTS+Parakeet process).
    fn restart_child(&self) {
        // Debounce rapid config churn; sleep with last_restart unlocked.
        const MIN_RESTART_GAP: std::time::Duration = std::time::Duration::from_secs(1);
        let now = std::time::Instant::now();
        let prev = self.last_restart.lock().unwrap().replace(now);
        if let Some(prev) = prev {
            let elapsed = now.duration_since(prev);
            if elapsed < MIN_RESTART_GAP {
                log::warn!(
                    target: "engine",
                    "TTS warm child restart {}ms after the previous one — rapid config churn?",
                    elapsed.as_millis()
                );
                let wait = MIN_RESTART_GAP - elapsed;
                log::info!(
                    target: "engine",
                    "TTS warm child restart debounced — waiting {}ms for the previous child to settle",
                    wait.as_millis()
                );
                std::thread::sleep(wait);
                // Re-anchor so a third rapid call debounces off real spacing.
                *self.last_restart.lock().unwrap() = Some(std::time::Instant::now());
            }
        }
        self.stop_child();
        self.ensure_started();
        self.stats.reset();
        self.stt_stats.reset();
    }

    /// After download: restart (or start) to load new files. Returns is_running after.
    pub(crate) fn reload_models(&self) -> bool {
        if !self.is_running() {
            self.ensure_started();
            return self.is_running();
        }
        self.restart_child();
        true
    }

    /// Post-READY crash heal ([`warm_child_heal_action`]). Observe+act under one lifecycle lock.
    pub(crate) fn restart_if_crashed(&self) {
        use crate::config_gate::HealAction;
        let _lifecycle = self.lifecycle.lock().unwrap();
        let (present, exited) = self.child.probe();
        let error = self.last_error().is_some();
        let action = crate::config_gate::warm_child_heal_action(present, exited, error);
        if action == HealAction::Nothing {
            return;
        }
        // One attempt per HEAL_COOLDOWN (deterministic crasher throttle).
        {
            let mut last = self.last_heal.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < HEAL_COOLDOWN) {
                return;
            }
            *last = Some(std::time::Instant::now());
        }
        match action {
            HealAction::Nothing => {}
            HealAction::ReapAndStart => {
                log::info!(target: "engine", "TTS warm child found dead — reaping and restarting it");
                self.mark_dead_locked();
                self.start_locked();
            }
            HealAction::Start => {
                log::info!(target: "engine", "TTS warm child is gone — restarting it for the queued speak");
                self.start_locked();
            }
        }
    }

    /// Store full-duplex pref; pair with restart_if_full_duplex_stale for running child.
    pub fn set_full_duplex_pref(&self, on: bool) {
        self.spawn_prefs.lock().unwrap().full_duplex = on;
    }

    /// Store STT provider token; restart_if_full_duplex_stale applies.
    pub fn set_stt_provider_pref(&self, engine: &str) {
        self.spawn_prefs.lock().unwrap().stt_provider = engine.to_string();
    }

    /// STT preload pref (`DONTSPEAK_STT_PRELOAD` on next start).
    pub fn set_stt_wanted(&self, wanted: bool) {
        self.spawn_prefs.lock().unwrap().stt_preload = wanted;
    }

    /// Built-in TTS/output preload pref for next start.
    pub fn set_tts_wanted(&self, wanted: bool) {
        self.spawn_prefs.lock().unwrap().tts_preload = wanted;
    }

    /// Built-in model for the next child. Language is supplied per utterance.
    pub fn set_tts_selection(&self, model: ds_config::TtsModel) {
        self.spawn_prefs.lock().unwrap().tts_model = model;
    }

    /// Restart if running prefs mismatch (fd / STT provider / tts_preload). Safe every reload.
    pub fn restart_if_full_duplex_stale(&self) {
        if !self.is_running() {
            return;
        }
        // Drop spawn_prefs before other locks.
        let prefs = self.spawn_prefs.lock().unwrap().clone();
        let fd_stale = prefs.full_duplex != *self.full_duplex_active.lock().unwrap();
        let stt_stale = prefs.stt_provider != *self.stt_provider_active.lock().unwrap();
        let tts_stale =
            tts_preload_restart_needed(prefs.tts_preload, *self.tts_wanted_active.lock().unwrap());
        let selection_stale = prefs.tts_preload
            && Self::tts_assets_ready(&prefs)
            && Some(prefs.tts_model) != *self.tts_selection_active.lock().unwrap();
        if !fd_stale && !stt_stale && !tts_stale && !selection_stale {
            return;
        }
        self.restart_child();
    }

    /// Resolved EP the child will report (cuda only if runtime present — no restart loop).
    fn resolve_provider(which: &str, model: ds_config::TtsModel) -> ds_config::RealizedProvider {
        Self::resolve_provider_with_availability(
            which,
            model,
            crate::config_gate::NativeShims::probe(),
            ds_model::cuda_runtime_available(),
        )
    }

    /// `shims` is `None` off macOS, where no native rung exists at all.
    fn resolve_provider_with_availability(
        which: &str,
        model: ds_config::TtsModel,
        shims: Option<crate::config_gate::NativeShims>,
        cuda_available: bool,
    ) -> ds_config::RealizedProvider {
        use ds_config::RealizedProvider;
        let descriptor = model.descriptor();
        if which.eq_ignore_ascii_case(ds_config::Provider::OrtCoreMl.as_str())
            && descriptor.supports_provider(ds_config::Provider::OrtCoreMl)
        {
            return RealizedProvider::CoreMl;
        }
        // Fluid before MLX, and each reads only its OWN dylib -- they are separate files, so
        // a Fluid-present/MLX-absent host must still realize Fluid. Only an explicit `fluid`
        // token -- never `auto` -- selects the ANE Kokoro backend.
        if let Some(shims) = shims
            && which.eq_ignore_ascii_case(ds_config::Provider::Fluid.as_str())
            && descriptor.supports_provider(ds_config::Provider::Fluid)
        {
            return if shims.fluid {
                RealizedProvider::Fluid
            } else {
                RealizedProvider::Cpu
            };
        }
        if let Some(shims) = shims
            && (which.eq_ignore_ascii_case(ds_config::Provider::Mlx.as_str())
                || which.eq_ignore_ascii_case("auto"))
            && descriptor.supports_provider(ds_config::Provider::Mlx)
        {
            return if shims.mlx {
                RealizedProvider::Mlx
            } else {
                RealizedProvider::Cpu
            };
        }
        // Share the effective-provider predicate with asset presence checks.
        if ds_model::tts_wants_cuda_assets_with(model, which, cuda_available) {
            return RealizedProvider::Cuda;
        }
        RealizedProvider::Cpu
    }

    fn tts_assets_ready(prefs: &SpawnPrefs) -> bool {
        let model = prefs.tts_model;
        match Self::resolve_provider(&prefs.provider, model) {
            // Fluid Kokoro: the Core ML set + the shared frontend (G2P/ORT) + the voices npz
            // the ANE chain materializes packs from (through `roots`, never ambient -- #212).
            ds_config::RealizedProvider::Fluid => {
                crate::config_gate::kokoro_g2p_files_present()
                    && ds_model::ModelRoots::ambient().is_some_and(|roots| {
                        ds_model::hf_repo::is_hf_set_present(
                            &roots,
                            &ds_model::coreml_repo::KOKORO_COREML_SET,
                        ) && roots.model.join(ds_model::KOKORO_VOICES_FILE).is_file()
                    })
            }
            ds_config::RealizedProvider::Mlx => {
                let frontend_ready = model != ds_config::TtsModel::Kokoro
                    || crate::config_gate::kokoro_g2p_files_present();
                frontend_ready
                    && ds_model::ModelRoots::ambient().is_some_and(|roots| {
                        ds_model::hf_repo::is_hf_set_present(
                            &roots,
                            ds_model::mlx_repo::tts_mlx_set(model),
                        )
                    })
            }
            _ => {
                ds_model::tts_model_files_present(
                    model,
                    ds_model::tts_wants_cuda_assets(model, &prefs.provider),
                ) && ds_model::onnxruntime_dylib_path()
                    .map(|path| path.is_file())
                    .unwrap_or(false)
            }
        }
    }

    /// True when a warm child is running.
    pub fn is_running(&self) -> bool {
        self.child.is_running()
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    fn set_error(&self, msg: impl Into<String>) {
        let msg = msg.into();
        let mut guard = self.last_error.lock().unwrap();
        if guard.as_deref() != Some(msg.as_str()) {
            *guard = Some(msg);
            drop(guard);
            // Bump status gate only when the error text changes (WaitModelStatus).
            if let Some(gate) = self.gate.get() {
                gate.bump();
            }
        }
    }
    fn clear_error(&self) {
        let mut guard = self.last_error.lock().unwrap();
        if guard.take().is_some() {
            drop(guard);
            if let Some(gate) = self.gate.get() {
                gate.bump();
            }
        }
    }

    pub fn set_enabled(&self, on: bool) {
        if on {
            self.ensure_started();
        } else {
            self.stop_child();
        }
    }

    pub fn ensure_started(&self) {
        if !self.is_running() {
            self.start();
        }
    }

    /// Spawn `ds-helper --serve` and wait — bounded by [`READY_HANDSHAKE_TIMEOUT`] —
    /// for its `READY` line (model warm). On any failure (including a child that
    /// never answers) the manager stays "not running": the queue worker surfaces the
    /// error and the utterance is dropped. Hooks never synthesize,
    /// so there is no fallback path.
    fn start(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.start_locked();
    }

    /// The body of [`start`](Self::start), with the `lifecycle` lock ALREADY HELD by the
    /// caller — `start` itself, or [`restart_if_crashed`](Self::restart_if_crashed), whose
    /// observe-then-act must be one atomic section.
    fn start_locked(&self) {
        // Re-check under the lifecycle lock: another thread may have started (or a
        // crashing one may still be tearing down) between the caller's
        // `is_running()` gate and here. Idempotent — never spawn a second child.
        if self.is_running() {
            return;
        }
        // Copy the whole spawn-prefs struct out from under its lock up front: the new
        // model-presence gate below AND the env-assembly further down both read it, and a
        // guard must never be held across the blocking spawn+read-loop that follows.
        let prefs = self.spawn_prefs.lock().unwrap().clone();

        let tts_ready = Self::tts_assets_ready(&prefs);
        if prefs.tts_preload && !tts_ready {
            // Mirrors every OTHER early-return below (spawn error, missing stdio, ERR line):
            // set_error() so `warm_child_heal_action` sees error=true and resolves to
            // `HealAction::Nothing` — a Caps-Lock-triggered `restart_if_crashed` must NOT retry
            // this doomed spawn on every tap; only the download-completion hook retries it, once,
            // when the fetch actually lands. Safe for the status UI: `combined_error` only
            // surfaces a model's `last_error` while that SAME model reads `present` — false here
            // by construction — so the TTS row shows "Missing" (offer Download), never a
            // stale "Failed".
            self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
            log::info!(
                target: "engine",
                "TTS/STT warm child start skipped — {} model not yet present on disk \
                 (provider={}); the background download will restart it automatically once it \
                 finishes",
                prefs.tts_model.as_str(),
                prefs.provider
            );
            return;
        }

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Helper stderr → a log file (full-duplex status, capture levels,
            // barge-debug, errors) so the warm child is diagnosable; was discarded.
            .stderr(helper_stderr(&self.helper_log_file));
        // The daemon→helper env contract, resolved from the spawn prefs:
        //   • DONTSPEAK_PROVIDER      — built-in TTS execution provider.
        //   • DONTSPEAK_TTS_MODEL     — model registry id.
        //   • DONTSPEAK_STT_PROVIDER  — local STT backend the child serves ("cpu"|"mlx"|…).
        //   • DONTSPEAK_FULL_DUPLEX   — AEC duplex mode (Parakeet+Kokoro only); off ⇒ half-duplex.
        //   • DONTSPEAK_STT_PRELOAD   — preload STT in parallel with the TTS load; only when STT
        //                               is the built-in engine (`stt_provider` alone can't tell —
        //                               it resolves to "cpu" even for Off/ClaudeCode).
        // Applied as ONE set-or-remove pass so every OFF flag is explicitly CLEARED — an
        // inherited ambient value can't override the config-resolved intent. See [`child_env`].
        for (key, val) in child_env(&prefs) {
            match val {
                Some(v) => cmd.env(key, v),
                None => cmd.env_remove(key),
            };
        }
        // Constructor-injected, first-spawn-only fixture controls. Per-manager command env
        // avoids the process-global environment race while letting the same manager recover.
        #[cfg(test)]
        if let Some(env) = self.test_options.first_spawn_env.lock().unwrap().take() {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }
        // Windows: the engine runs inside a windowless GUI host (the WinUI app), so
        // spawning this CONSOLE-subsystem helper would pop a stray terminal window.
        // CREATE_NO_WINDOW suppresses it; the piped stdio still works without a console.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
                log::warn!(
                    target: "engine",
                    "TTS warm child spawn failed ({}): {e}",
                    self.bin.display()
                );
                return;
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        let (Some(stdin), Some(mut stdout)) = (stdin, stdout) else {
            let _ = child.kill();
            let _ = child.wait();
            self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
            log::warn!(target: "engine", "TTS warm child missing stdio pipes");
            return;
        };

        // Wait for READY (model loaded) or ERR (fatal) — bounded by
        // `ready_handshake_timeout()`. A child that fails normally closes stdout (EOF) or
        // prints ERR, but a child that stays ALIVE without ever answering (issue #59 —
        // ORT provider init spinning, the model file on a stalled mount or held by
        // an AV scanner) used to block this loop, and the `lifecycle` lock with it,
        // forever. A pipe read is not portably interruptible, so a dedicated handshake
        // thread owns the `BufReader` and feeds lines over a channel; the main thread
        // bounds the wait with `recv_timeout`. On READY the thread stops reading and
        // RETURNS the same buffered reader, so the persistent demux reader below takes
        // over the stream with no data loss.
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        let handshake = std::thread::spawn(move || {
            loop {
                let mut line = String::new();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        let _ = line_tx.send(Ok(None));
                        break;
                    }
                    Ok(_) => {
                        let ready = line.trim() == proto::READY;
                        let _ = line_tx.send(Ok(Some(line)));
                        if ready {
                            // Success terminal: stop reading so `start_locked` can hand the
                            // stream to the persistent reader.
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = line_tx.send(Err(e));
                        break;
                    }
                }
            }
            stdout
        });
        /// Why the pre-READY wait ended without a READY. Carried OUT of the loop so every
        /// failure leaves through the one teardown below instead of its own `return`.
        struct PreReadyFailure {
            /// [`set_error`](TtsManager::set_error) text — the helper's own `ERR` payload,
            /// or the generic reason for the transport failures.
            error: String,
            log: String,
        }

        let deadline = std::time::Instant::now() + self.ready_handshake_timeout();
        let failure = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match line_rx.recv_timeout(remaining) {
                Ok(Ok(Some(line))) => {
                    let l = line.trim();
                    if l == proto::READY {
                        break None;
                    }
                    // STT preloads in PARALLEL, so its terminal can land on either side of
                    // READY — this pre-READY wait loop and the post-READY reader both route
                    // STTLOADED through the SAME `ModelSlot::transition`. (The helper's WARMING
                    // trace lines fall through to the ignore arm: model downloads run in the
                    // engine's download manager, so there is no per-child fetch state here.)
                    let gate = self.gate.get().map(|g| g.as_ref());
                    if l == proto::STTLOADED {
                        self.stt_model.transition(ModelState::Loaded, gate);
                        continue;
                    }
                    // Symmetric with STTLOADED: a mid-session `load tts` confirms residency here
                    // (though it normally lands post-READY, in the persistent reader below).
                    if l == proto::TTSLOADED {
                        self.tts_model.transition(ModelState::Loaded, gate);
                        continue;
                    }
                    // STT preloads in PARALLEL, so a failed preload can also report here
                    // (before READY) rather than only in the post-READY persistent reader —
                    // see `set_stt_load_error`'s doc.
                    if let Some(msg) = l.strip_prefix(proto::STTLOADERR_PREFIX) {
                        self.set_stt_load_error(msg.trim());
                        continue;
                    }
                    if let Some(msg) = l.strip_prefix(proto::TTSLOADERR_PREFIX) {
                        self.set_tts_load_error(msg.trim());
                        continue;
                    }
                    if let Some(p) = l.strip_prefix(proto::STT_PROVIDER_PREFIX) {
                        store_realized(&self.stt_realized, realized_backend_token(p), gate);
                        continue;
                    }
                    if let Some(p) = l.strip_prefix(proto::PROVIDER_PREFIX) {
                        store_realized(&self.tts_realized, realized_backend_token(p), gate);
                        continue;
                    }
                    if let Some(msg) = l.strip_prefix(proto::ERR) {
                        break Some(PreReadyFailure {
                            error: msg.trim().to_string(),
                            log: format!("TTS warm child failed to load:{msg}"),
                        });
                    }
                    // ignore any other chatter before READY
                }
                Ok(Ok(None)) => {
                    break Some(PreReadyFailure {
                        error: ds_i18n::t("status.engine.reason.tts_failed"),
                        log: "TTS warm child closed before READY".to_string(),
                    });
                }
                Ok(Err(e)) => {
                    break Some(PreReadyFailure {
                        error: ds_i18n::t("status.engine.reason.tts_failed"),
                        log: format!("TTS warm child read error before READY: {e}"),
                    });
                }
                Err(_) => {
                    // Timeout (or the handshake thread vanished): the child is alive but
                    // never answered — the teardown below kills it rather than waiting
                    // forever. The manager parks "not running" with `last_error` set, so
                    // `warm_child_heal_action` (absent, error=true) resolves to `Nothing` —
                    // no automatic retry storm; recovery is owned by the download-completion
                    // hook, a config change, or the next `set_enabled`, exactly like every
                    // other start failure.
                    break Some(PreReadyFailure {
                        error: ds_i18n::t("status.engine.reason.tts_failed"),
                        log: format!(
                            "TTS warm child (pid {}) never printed READY within {:?} — killed",
                            child.id(),
                            self.ready_handshake_timeout()
                        ),
                    });
                }
            }
        };
        if let Some(failure) = failure {
            // ONE teardown for every pre-READY failure. Kill even on EOF: a child that
            // closed stdout without exiting would block `wait()` — and the `lifecycle`
            // lock with it. The kill also closes the pipe → the handshake thread EOFs, so
            // the join is bounded.
            let _ = child.kill();
            let _ = child.wait();
            let _ = handshake.join();
            // This child is never installed, so no persistent reader exists to see its EOF
            // and clear for us. STT preloads in PARALLEL, so a healthy Parakeet reports
            // STTLOADED (+ STT_PROVIDER) before a failing TTS load lands here — which used
            // to leave the STT row green, with its realized provider, behind a process that
            // is gone (issue #213). Clear BEFORE `set_error`, so a waiter woken by the
            // error bump can't still read the dead child's residency.
            self.clear_loaded_flags();
            self.set_error(failure.error);
            log::warn!(target: "engine", "{}", failure.log);
            return;
        }
        let stdout = handshake
            .join()
            .expect("the READY-handshake reader thread panicked");

        self.clear_error();
        // A fresh child is about to be installed: any stale per-model load error from a
        // PRIOR child is no longer relevant — clear both (gated, so this is a no-op unless
        // one was actually set).
        self.clear_stt_load_error();
        self.clear_tts_load_error();
        // Install the new child: handle + generation bump + expected-EOF reset are
        // ONE `ChildSlot` transition (see `ChildSlot::install`) — anyone who next
        // observes this child is guaranteed to see its new generation too (see
        // `mark_dead_if_current`), and from here an EOF is a CRASH unless a
        // deliberate teardown (`stop_child`/`mark_dead`) re-marks it expected
        // before killing.
        self.child.install(child);
        *self.stdin.lock().unwrap() = Some(stdin);
        // Spawn the persistent demux reader: it owns stdout and routes the child's
        // lines into the speak/listen slots, so a speak and a listen can be in
        // flight at once (full-duplex coexist). It exits on EOF (child killed).
        let handle = {
            let speak_slot = self.speak_slot.clone();
            let cue_slot = self.cue_slot.clone();
            let listen_slot = self.listen_slot.clone();
            let diarize_slot = self.diarize_slot.clone();
            let enroll_slot = self.enroll_slot.clone();
            let stats = self.stats.clone();
            let stt_stats = self.stt_stats.clone();
            let lifetime = self.lifetime.clone();
            let tts_model = self.tts_model.clone();
            let stt_model = self.stt_model.clone();
            // So the reader's unexpected-EOF handler can classify the EOF (deliberate
            // vs crash) and try_wait() the real exit status/signal (peek only — the
            // actual reap stays with mark_dead/restart_if_crashed, so no
            // double-teardown race).
            let child_slot = self.child.clone();
            // STT preloads on a PARALLEL thread, so its `STT_PROVIDER` line often lands AFTER READY
            // (and always for a lazy `load stt`) — i.e. in THIS persistent reader, not start()'s
            // pre-READY wait loop. Clone the realized-provider slot in so the reader can capture it;
            // without this the STT status row stays "CPU" while STT actually ran on the GPU.
            let stt_realized = self.stt_realized.clone();
            // Cloned in NOT to set (the helper never emits `PROVIDER` post-READY) but so the
            // reader's unexpected-EOF branch can CLEAR the realized TTS token with the child —
            // else a crash leaves the dead process's backend showing in the status row.
            let tts_realized = self.tts_realized.clone();
            // The status push-gate, so a post-READY STTLOADED pushes LIVE instead of waiting
            // for the next poll.
            let gate = self.gate.get().cloned();
            std::thread::spawn(move || {
                reader_loop(
                    stdout,
                    ReaderSlots {
                        speak: speak_slot,
                        cue: cue_slot,
                        listen: listen_slot,
                        diarize: diarize_slot,
                        enroll: enroll_slot,
                    },
                    ReaderStats {
                        tts: stats,
                        stt: stt_stats,
                        lifetime,
                    },
                    ReaderModelState {
                        tts_model,
                        stt_model,
                        stt_realized,
                        tts_realized,
                        gate,
                        child: child_slot,
                    },
                );
            })
        };
        *self.reader.lock().unwrap() = Some(handle);
        // Record what this child was started with, so a later pref change restarts.
        *self.full_duplex_active.lock().unwrap() = prefs.full_duplex;
        *self.stt_provider_active.lock().unwrap() = prefs.stt_provider;
        *self.tts_wanted_active.lock().unwrap() = prefs.tts_preload;
        *self.tts_selection_active.lock().unwrap() = Some(prefs.tts_model);
        // Kokoro is eager-loaded by the helper before READY. STT (Parakeet) now preloads in
        // PARALLEL and reports its own STTLOADED (possibly BEFORE this READY), so we must NOT
        // reset stt_model here — it's initialized before the wait loop and set by the STT
        // signal handlers.
        if prefs.tts_preload {
            self.tts_model
                .transition(ModelState::Loaded, self.gate.get().map(|g| g.as_ref()));
        } else {
            self.tts_model
                .transition(ModelState::Idle, self.gate.get().map(|g| g.as_ref()));
        }
        // Re-apply the CURRENT global-mute state to this freshly (re)spawned child. Every
        // start (provider switch, post-download restart, crash-heal via `restart_if_crashed`)
        // installs a brand-new child that inits UNMUTED — without this push, speech would
        // play audibly at full volume right after the switch while the UI still shows
        // "muted" (mirrors the `mute` op `set_muted` sends to an already-running child on
        // a live toggle).
        let _ = self.write_request(
            &serde_json::json!({
                "op": ds_helper_proto::HelperOp::Mute,
                "text": if self.is_muted() { "on" } else { "off" },
            })
            .to_string(),
        );
        log::info!(target: "engine", "TTS/STT warm helper READY");
    }

    /// Reset BOTH models to `Idle` (the process is gone → both models go with it). Each
    /// [`ModelSlot::transition`] call is already change-gated (a no-op, no bump, if that
    /// model was already `Idle`), so a blocked `WaitModelStatus` still sees a real
    /// transition immediately instead of at some unrelated later status change (the
    /// caps-dot bug class; see `set_caps_gate` in engine.rs) — without the old manual
    /// "only bump if at least one was loaded" bookkeeping this used to need. Also drops
    /// the realized STT and TTS tokens, which go with the process. Shared by `stop_child`
    /// and `mark_dead_locked`.
    fn clear_loaded_flags(&self) {
        let gate = self.gate.get().map(|g| g.as_ref());
        // Clear BEFORE the transitions: a waiter woken by a transition bump must not
        // still read the dead child's token. Each clear is change-gated because
        // `stop_child` can reach here with both models already Idle, where the
        // transitions bump nothing.
        store_realized(&self.stt_realized, None, gate);
        store_realized(&self.tts_realized, None, gate);
        self.tts_model.transition(ModelState::Idle, gate);
        self.stt_model.transition(ModelState::Idle, gate);
    }

    /// Kill + reap the warm child, freeing the model. Safe to call when stopped.
    fn stop_child(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        // This teardown is DELIBERATE — the reader must not report the kill's EOF as a crash.
        self.child.begin_deliberate_stop();
        // Toggled off ⇒ not a failure; clear any stale start error.
        self.clear_error();
        // Drop stdin first so the child sees EOF, then hard-kill to be sure.
        *self.stdin.lock().unwrap() = None;
        // The process is gone → both models go with it. A restart_child()'s stop+start
        // pair still only flashes once: `mark_loaded` bumps again when the fresh child is
        // READY.
        self.clear_loaded_flags();
        // `reap` has already released the slot's lock when it returns, so the
        // kill/wait below run OUTSIDE any lock.
        if let Some(mut child) = self.child.reap() {
            let _ = child.kill();
            let _ = child.wait();
            log::info!(target: "engine", "TTS/STT warm helper stopped (models freed)");
        }
        // Killing the child closes its stdout → the reader EOFs and returns; join
        // it so a stale reader can't touch the next child's slots.
        self.join_reader();
    }

    /// Mark the child as dead after an IO error so the next speak restarts it — via the
    /// worker's [`restart_if_crashed`](Self::restart_if_crashed) (the not-ready guard alone
    /// would DROP that speak instead of restarting).
    fn mark_dead(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.mark_dead_locked();
    }

    /// The body of [`mark_dead`](Self::mark_dead), with the `lifecycle` lock ALREADY HELD
    /// by the caller (mirror of [`start_locked`](Self::start_locked)).
    fn mark_dead_locked(&self) {
        // The kill below is deliberate reaping; the reader (if still up) already saw —
        // and reported — the child's own EOF.
        self.child.begin_deliberate_stop();
        *self.stdin.lock().unwrap() = None;
        // A dead child holds no models — clear the residency flags so the dot doesn't
        // show a stale "running" until the next start (this comment used to claim that
        // already, without actually doing it — the exact bug class fixed in
        // set_caps_gate, engine.rs).
        self.clear_loaded_flags();
        // `reap` has already released the slot's lock when it returns, so the
        // try_wait/kill/wait below run OUTSIDE any lock.
        if let Some(mut child) = self.child.reap() {
            let pid = child.id();
            // Debug aid: mark_dead runs after an IO error already suggested the
            // child is gone — try_wait() BEFORE kill() so a genuine crash's real
            // ExitStatus (code, or on unix the terminating signal) is captured
            // rather than clobbered by our own kill signal. Falls back to kill+wait
            // (whose status is uninformative — it's just our SIGKILL) only if the
            // child is somehow still alive.
            let status = match child.try_wait() {
                Ok(Some(status)) => Some(status),
                _ => {
                    let _ = child.kill();
                    child.wait().ok()
                }
            };
            log::warn!(
                target: "engine",
                "TTS warm child (pid {pid}) reaped by mark_dead: {}",
                describe_exit(status)
            );
        }
        self.join_reader();
    }

    /// Like [`mark_dead`](Self::mark_dead), but only reaps the child if `expected_gen`
    /// (the generation the caller captured when it sent its request) still matches the
    /// CURRENT child generation. A `play`/`listen`/`diarize`/`enroll` call can block on a
    /// slot `Condvar` and only wake (fatal/dead) on an EOF from an OLD, already-killed
    /// child — if a concurrent provider-switch/download-restart/crash-heal has ALREADY
    /// installed a fresh replacement by the time it wakes, `expected_gen` is stale and
    /// this must be a no-op; otherwise it would win the `lifecycle` lock race and
    /// silently kill the brand-new child, with no error logged.
    fn mark_dead_if_current(&self, expected_gen: u64) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        if self.child.generation() != expected_gen {
            log::info!(
                target: "engine",
                "TTS: stale child-death signal from a superseded child ignored \
                 (already restarted)"
            );
            return;
        }
        self.mark_dead_locked();
    }

    /// Join the persistent stdout reader (after the child has been killed, so it
    /// has EOF'd). No-op when no reader is running. Must not be called while
    /// holding a slot lock — the reader briefly locks the slots on its way out.
    fn join_reader(&self) {
        if let Some(h) = self.reader.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    /// Tell the warm helper to free a cached model it no longer needs while the
    /// OTHER engine keeps it warm — universal: TTS → selected built-in model,
    /// STT → Parakeet. The helper lazily reloads on next use. Fire-and-forget; no-op
    /// when the helper isn't running (nothing to free).
    pub fn unload_engine(&self, engine: ds_helper_proto::HelperModel) {
        let req = serde_json::json!({
            "op": ds_helper_proto::HelperOp::Unload,
            "engine": engine,
        });
        if self.write_request(&req.to_string()).is_ok() {
            // `ModelSlot::transition` is itself change-gated (mirrors `mark_loaded`'s push on
            // the "true" direction) — otherwise an ordinary TTS/STT engine switch would leave
            // a blocked `WaitModelStatus` showing a stale "Running" dot for up to the poll
            // window, while `reconcile_helper_models`'s UNCONDITIONAL ~20s-tick call for any
            // engine that isn't currently wanted would wake every connected client every tick
            // forever even when nothing changed — reintroducing the poll-churn regression this
            // whole gating scheme exists to fix. Transitioning straight to `Idle` also clears
            // any stale "failed to load" state — a deliberately unloaded model has none anymore.
            let gate = self.gate.get().map(|g| g.as_ref());
            let changed = match engine {
                ds_helper_proto::HelperModel::Tts => {
                    self.tts_model.transition(ModelState::Idle, gate)
                }
                ds_helper_proto::HelperModel::Stt => {
                    self.stt_model.transition(ModelState::Idle, gate)
                }
            };
            // Log like the gate: real transitions only, or the tick floods the log.
            if changed {
                log::info!(target: "engine", "helper: requested unload of {engine:?} model");
            }
        }
    }

    /// Tell the warm helper to eagerly (pre)load a model so it's resident the moment
    /// its engine is selected — the symmetric counterpart to [`unload_engine`], so
    /// "loaded" reflects residency before first use (Parakeet is otherwise lazy).
    /// Fire-and-forget; no-op when the helper isn't running.
    pub fn load_engine(&self, engine: ds_helper_proto::HelperModel) {
        let req = serde_json::json!({
            "op": ds_helper_proto::HelperOp::Load,
            "engine": engine,
        });
        if self.write_request(&req.to_string()).is_ok() {
            // Neither engine lights optimistically: TTS waits for the helper's `TTSLOADED`
            // confirmation (after `load_backend`) exactly as STT waits for `STTLOADED` (after
            // preload + graph warmup), so the dot stays "warming" until the model is truly
            // resident — never greening on the mere `load` request.
            let already_loaded = match engine {
                ds_helper_proto::HelperModel::Tts => self.tts_model.is_loaded(),
                ds_helper_proto::HelperModel::Stt => self.stt_model.is_loaded(),
            };
            // Already-Loaded = the reconcile tick's steady-state re-send; log only
            // requests that can start a real load, or the tick floods the log.
            if !already_loaded {
                log::info!(target: "engine", "helper: requested preload of {engine:?} model");
            }
        }
    }

    /// Write one JSON request line to the child's stdin. Err if not running.
    fn write_request(&self, json: &str) -> std::io::Result<()> {
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "TTS child not running")
        })?;
        stdin.write_all(json.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// Speak `text` through the warm child and block until it finishes (or is
    /// cancelled — the child reports `DONE` for both). Err ⇒ either the engine could
    /// not speak (no child / IO error / terminal timeout — fatal, the child is
    /// reaped) or the helper reported a per-request `ERR` (frontend or transactional
    /// synthesis failure — soft, the child stays alive). There is no fallback: the
    /// queue worker logs the Err and the utterance is dropped.
    ///
    /// `skip` = frontend batches an earlier run of this exact text already played
    /// (0 = from the top); the helper clamps it, so a stale value degrades to an
    /// empty no-op request, never a panic. See [`last_speak_progress`](Self::last_speak_progress).
    pub fn speak(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        rate: f32,
        params: &ds_config::ResolvedTtsParams,
        skip: usize,
    ) -> std::io::Result<()> {
        self.play(text, voice, language, rate, params, skip)
    }

    /// The helper's ABSOLUTE played-batch high-water mark for the most recent speak
    /// request (`PROGRESS` lines; 0 = none seen — older helper, full-duplex path, or
    /// nothing played ⇒ resume from the top). Coherent once `speak` returns: PROGRESS
    /// demuxes on the same reader thread BEFORE the terminal `DONE`, and `play()`
    /// resets the slot per request.
    pub fn last_speak_progress(&self) -> usize {
        self.speak_slot.0.lock().unwrap().progress
    }

    /// Count one utterance only after the queue exhausts transparent recovery.
    pub(crate) fn record_speak_failure(&self) {
        self.stats.record_failure();
    }

    #[cfg(test)]
    pub(crate) fn speak_failures_for_test(&self) -> u64 {
        self.stats.snapshot().failures
    }

    /// Block on OS speech. Muted requests are consumed without spawning; live mute kills
    /// the synthesizer because System TTS cannot volume-drain.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn speak_system(
        &self,
        text: &str,
        voice: &str,
        _language: &str,
        rate: f32,
    ) -> std::io::Result<()> {
        // Check before stop(), which also cancels built-in speech.
        if self.is_muted() {
            return Ok(());
        }
        let Some(mut cmd) = ds_tts::system::speech_command(Some(voice), rate, text) else {
            return Ok(());
        };
        // Mute may land during command construction.
        if self.is_muted() {
            return Ok(());
        }
        self.stop();
        if !self.spawn_say_child_if_unmuted(&mut cmd)? {
            return Ok(());
        }
        self.wait_for_system_child()
    }

    /// Hold the live-mute slot across the final check and spawn to close the race.
    #[cfg(any(test, target_os = "macos", target_os = "windows"))]
    fn spawn_say_child_if_unmuted(&self, cmd: &mut Command) -> std::io::Result<bool> {
        let mut child = self.say_child.lock().unwrap();
        if self.is_muted() {
            return Ok(false);
        }
        *child = Some(cmd.spawn()?);
        Ok(true)
    }

    /// Wait for an owned system-speech child without holding the slot across the poll sleep.
    /// A missing child means a concurrent barge took and killed it; a real process or polling
    /// failure must propagate so the queue never records failed OS speech as played.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn wait_for_system_child(&self) -> std::io::Result<()> {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let mut child = self.say_child.lock().unwrap();
            let Some(process) = child.as_mut() else {
                return Ok(());
            };
            match process.try_wait() {
                Ok(Some(status)) => {
                    *child = None;
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(std::io::Error::other(format!(
                            "system TTS process exited with {status}"
                        )))
                    };
                }
                Ok(None) => {}
                Err(e) => {
                    // The handle is no longer pollable, so do not leave a possibly-live speech
                    // process orphaned after reporting failure.
                    let _ = process.kill();
                    let _ = process.wait();
                    *child = None;
                    return Err(e);
                }
            }
        }
    }

    /// Linux returns Unsupported (issue #74), except muted requests still drain silently.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn speak_system(
        &self,
        _text: &str,
        _voice: &str,
        _language: &str,
        _rate: f32,
    ) -> std::io::Result<()> {
        if self.is_muted() {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "dontspeakd System (say) TTS is not yet wired up on this platform",
        ))
    }

    fn play(
        &self,
        text: &str,
        voice: &str,
        language: &str,
        rate: f32,
        params: &ds_config::ResolvedTtsParams,
        skip: usize,
    ) -> std::io::Result<()> {
        // Fresh request: reset the speak slot so an error before dispatch cannot expose the
        // previous request's progress to queue retry/resume accounting.
        {
            let (m, _cv) = &*self.speak_slot;
            *m.lock().unwrap() = SpeakSlot::default();
        }
        // Snapshot the child's generation for THIS request (one acquisition also serves
        // as the is-running gate) — if the reader only wakes us (fatal) after a
        // concurrent restart has ALREADY installed a new child, this lets us tell "our
        // child died" apart from "a stale EOF from an old, superseded child".
        let Some(my_gen) = self.child.running_gen() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "TTS child not running",
            ));
        };

        let req = serde_json::json!({
            "op": ds_helper_proto::HelperOp::Speak,
            "voice": voice,
            "language": language,
            "rate": rate,
            "params": params,
            "text": text,
            "skip": skip,
        });
        if let Err(e) = self.write_request(&req.to_string()) {
            self.mark_dead();
            return Err(e);
        }
        // The helper lazily (re)loads Kokoro to serve this — it's resident now. Optimistic
        // (no gate bump — `play()` runs on EVERY speak): the authoritative "just became
        // resident" push stays the helper's own `TTSLOADED` confirmation. See
        // `ModelSlot::mark_loaded_optimistic`.
        self.tts_model.mark_loaded_optimistic();

        // Block until the reader signals this speak's terminal DONE (or ERR/EOF).
        // We hold ONLY the speak-slot lock here — a concurrent `listen` drains its
        // own slot, and `stop` takes the stdin lock — so nothing is serialized.
        let (m, cv) = &*self.speak_slot;
        let mut s = m.lock().unwrap();
        let deadline = std::time::Instant::now() + SPEAK_TERMINAL_TIMEOUT;
        while !s.done {
            let now = std::time::Instant::now();
            if now >= deadline {
                drop(s);
                self.mark_dead_if_current(my_gen);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "TTS helper did not finish the speak request",
                ));
            }
            let (next, _) = cv.wait_timeout(s, deadline - now).unwrap();
            s = next;
        }
        let err = s.err.take();
        let fatal = s.fatal;
        drop(s);
        if let Some(e) = err {
            // EOF/read-error ⇒ the child died: reap it so the next speak restarts — but
            // only if it's STILL the child we sent this request to (see
            // `mark_dead_if_current`). A soft `ERR` line (child alive) just fails this
            // one utterance — but it still counts as a failure in the stats, or a helper
            // failing every utterance would look identical to a healthy idle one.
            if fatal {
                self.mark_dead_if_current(my_gen);
            }
            return Err(if fatal {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)
            } else {
                std::io::Error::other(e)
            });
        }
        Ok(())
    }

    /// Run an STT (listen) session on the warm helper: stream `PARTIAL` text to
    /// `on_partial`, return the FINAL transcript. The helper opens the mic and
    /// re-transcribes periodically; end it with `stop()` (from a second caller).
    /// Starts the helper if it isn't running. Holds the stdout reader for the
    /// session (speak/listen are mutually exclusive). Err ⇒ the helper is gone.
    ///
    /// Both production callers (`HelperStt`, `TestSession`) run this call on a background
    /// thread while their OWN stop can arrive on a DIFFERENT thread (`HelperStt::start`/
    /// `stop`, `TestSession::run`/`stop`): `cancelled_early` is the caller's own flag, set by
    /// its `stop()` before this call is even guaranteed to have started running. Without it,
    /// `active_listen_generation`/`listen_stopped_through` — the mechanism that closes finding
    /// #3's "stop races the queued start" race at the ds-helper/serve.rs layer — isn't
    /// published until THIS function actually runs on its thread, so a stop that fires between
    /// the caller spawning/dispatching the work and this function starting would otherwise be
    /// silently lost (the caller's `stop_listen()` sees `active_listen_generation == 0` and
    /// no-ops). Checked both before the helper is started (so an already-cancelled session
    /// never spins one up) and again at the same point the generation-based cancellation
    /// already is, right before the request is sent.
    pub fn listen_cancellable(
        &self,
        cancelled_early: &AtomicBool,
        on_partial: &mut dyn FnMut(&str),
    ) -> std::io::Result<String> {
        let _lease = self.listen_lease.try_lock().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "another speech-recognition session is already active",
            )
        })?;
        let generation = self.next_listen_generation.fetch_add(1, Ordering::SeqCst);
        self.active_listen_generation
            .store(generation, Ordering::SeqCst);
        struct ActiveGeneration<'a> {
            mgr: &'a TtsManager,
            generation: u64,
        }
        impl Drop for ActiveGeneration<'_> {
            fn drop(&mut self) {
                let _ = self.mgr.active_listen_generation.compare_exchange(
                    self.generation,
                    0,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                let mut stopped = self.mgr.listen_stop_started.lock().unwrap();
                if stopped.is_some_and(|(g, _)| g == self.generation) {
                    *stopped = None;
                }
            }
        }
        let _active = ActiveGeneration {
            mgr: self,
            generation,
        };
        // Checked BEFORE starting the helper too: a caller whose stop already fired before
        // this function got scheduled shouldn't pay for spinning up (or waking) the child at
        // all, and — usefully for tests — this path needs no real helper process to exercise.
        if cancelled_early.load(Ordering::SeqCst) {
            return Ok(String::new());
        }
        self.ensure_started();
        if !self.is_running() {
            return Err(std::io::Error::other("STT helper not running"));
        }
        if cancelled_early.load(Ordering::SeqCst)
            || self.listen_stopped_through.load(Ordering::SeqCst) >= generation
        {
            return Ok(String::new());
        }
        // Fresh session: drop any stale events / dead flag from a prior listen.
        {
            let (m, _cv) = &*self.listen_slot;
            let mut s = m.lock().unwrap();
            s.events.clear();
            s.dead = false;
        }
        let request = serde_json::json!({
            "op": ds_helper_proto::HelperOp::Listen,
            "session": generation,
        })
        .to_string();
        if let Err(e) = self.write_request(&request) {
            self.mark_dead();
            return Err(e);
        }
        // The helper lazily loads Parakeet on first listen — it's resident now. Optimistic
        // (no gate bump — `listen()` runs on EVERY listen, and for a lazy first listen with
        // STT preload off, this may be the ONLY writer of "loaded" ever on this path, since
        // `ds-helper`'s `run_listen`/`run_concurrent_listen` never print `STTLOADED`
        // themselves). See `ModelSlot::mark_loaded_optimistic`.
        self.stt_model.mark_loaded_optimistic();

        let mut final_text = String::new();
        let (m, cv) = &*self.listen_slot;
        loop {
            // Pop one event under a brief lock; drop it BEFORE calling on_partial so
            // the single reader thread is never blocked by the partial callback.
            enum WaitResult {
                Event(Option<ListenEvt>),
                FinalizeTimeout,
            }
            let waited = {
                let mut s = m.lock().unwrap();
                loop {
                    if let Some(e) = s.events.pop_front() {
                        break WaitResult::Event(Some(e));
                    }
                    if s.dead {
                        break WaitResult::Event(None);
                    }
                    let (next, timeout) = cv
                        .wait_timeout(s, std::time::Duration::from_secs(1))
                        .unwrap();
                    s = next;
                    if timeout.timed_out() && self.listen_finalize_timed_out(generation) {
                        break WaitResult::FinalizeTimeout;
                    }
                }
            };
            let evt = match waited {
                WaitResult::Event(evt) => evt,
                WaitResult::FinalizeTimeout => {
                    log::warn!(
                        target: "engine",
                        "STT helper generation {generation} did not finalize after stop; restarting helper"
                    );
                    self.mark_dead();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "STT helper finalization timed out",
                    ));
                }
            };
            match evt {
                Some(ListenEvt::Partial(t)) => on_partial(&t),
                Some(ListenEvt::Final(t)) => final_text = t,
                Some(ListenEvt::Done) => return Ok(final_text),
                Some(ListenEvt::Err(e)) => {
                    self.stt_stats.record_failure();
                    return Err(std::io::Error::other(format!("STT:{e}")));
                }
                None => {
                    // Child gone with no LDONE: reap so the next listen restarts.
                    self.mark_dead();
                    return Err(std::io::Error::other("STT helper closed mid-listen"));
                }
            }
        }
    }

    /// Kill and reap System TTS without cancelling built-in speech.
    fn kill_say_child(&self) {
        if let Some(mut c) = self.say_child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Barge-in: cancel any in-flight playback. Fire-and-forget (no stdout read),
    /// so it can run while a `speak` is blocked awaiting its `DONE`. Stops BOTH the
    /// warm child's playback and any in-flight System `say`. Only the macOS/Windows
    /// `speak_system` path calls this (Linux has no System engine), so it's gated to those
    /// targets to stay dead-code-clean.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn stop(&self) {
        let _ = self.write_request(
            &serde_json::json!({ "op": ds_helper_proto::HelperOp::Stop }).to_string(),
        );
        self.kill_say_child();
    }

    /// Whether global mute is on.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Set global mute. Built-in speech drains silently; cues are suppressed.
    /// Idempotent. macOS full-duplex mutes at RENDER time: the VPIO callback keeps
    /// consuming the ring at wall rate but zero-fills the output (the ring holds real
    /// audio), so mute lands within one audio quantum, unmute resumes at the playhead
    /// instantly, and audio elapsed while muted is still skipped — the same
    /// mute-consumes-speech semantics as the rodio volume path.
    /// System TTS skips new spawns and kills in-flight speech without fade or resume.
    pub fn set_muted(&self, on: bool) {
        let changed = self.muted.swap(on, Ordering::Relaxed) != on;
        let _ = self.write_request(
            &serde_json::json!({
                "op": ds_helper_proto::HelperOp::Mute,
                "text": if on { "on" } else { "off" },
            })
            .to_string(),
        );
        // Push the mute transition to a blocked `WaitModelStatus` (the flag is part of
        // `model_status`). Only on a real change so an idempotent re-set wakes no one.
        if changed && let Some(gate) = self.gate.get() {
            gate.bump();
        }
        if on {
            self.kill_say_child();
        }
    }

    /// Like [`stop`](Self::stop) but asks the warm helper to FADE the rodio player
    /// out over a short window before stopping, so a user-facing barge (clear-on-submit,
    /// window close, newest-reply preempt, the caps long-press reset, and the mic
    /// record-barge) tapers off instead of clicking. The system `say` path can't fade,
    /// so it's killed outright exactly as in `stop`.
    pub fn stop_fade(&self) {
        let _ = self.write_request(
            &serde_json::json!({ "op": ds_helper_proto::HelperOp::Stopfade }).to_string(),
        );
        self.kill_say_child();
    }

    /// Play one EARCON on the warm child and block until it finishes, is muted, or is
    /// explicitly cancelled. Revalidating the path here keeps the helper protocol safe.
    pub fn cue(&self, path: &std::path::Path) -> std::io::Result<()> {
        let Some(path) = ds_earcon::canonical_sound_path(path) else {
            return Ok(());
        };
        self.cue_validated(&path)
    }

    /// [`cue`](Self::cue) past its `canonical_sound_path` trust-boundary check — split out so
    /// tests can drive the helper protocol with a tempdir path that can never pass that check.
    fn cue_validated(&self, path: &std::path::Path) -> std::io::Result<()> {
        // At most ONE cue op in flight (the queue worker plus ttsq's out-of-band
        // needs-input path can now call concurrently): two waiters on the one CueSlot
        // race — the second caller's slot reset can erase a `done` the first waiter
        // hasn't consumed, and CUEDONE↔waiter pairing turns ambiguous. Holding the
        // lease across reset-slot → write-request → wait keeps the helper's
        // one-CUEDONE-per-op mapping 1:1. The dangerous cross-satisfaction path (op A
        // times out, its late CUEDONE lands after op B resets the slot) is closed
        // because the timeout/dead arms call mark_dead_if_current, which kills the
        // child and joins its reader. Worst-case queue-worker wait behind an
        // out-of-band cue: one cue duration, bounded by CUE_TERMINAL_TIMEOUT — plus,
        // pathologically, when the timeout arm's mark_dead_if_current (still holding
        // this lease) contends with a concurrent restart's lifecycle hold, the READY
        // handshake.
        let _lease = self.cue_lease.lock().unwrap();
        let Some(my_gen) = self.child.running_gen() else {
            return Err(std::io::Error::other("TTS child not running"));
        };
        {
            let (m, _) = &*self.cue_slot;
            *m.lock().unwrap() = CueSlot::default();
        }
        if let Err(error) = self.write_request(
            &serde_json::json!({
                "op": ds_helper_proto::HelperOp::Cue,
                "text": path.to_string_lossy(),
            })
            .to_string(),
        ) {
            self.mark_dead();
            return Err(error);
        }

        let (m, cv) = &*self.cue_slot;
        let mut state = m.lock().unwrap();
        let deadline = std::time::Instant::now() + self.cue_terminal_timeout();
        while !state.done && !state.dead {
            let now = std::time::Instant::now();
            if now >= deadline {
                drop(state);
                self.mark_dead_if_current(my_gen);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "TTS helper did not finish the cue request",
                ));
            }
            let (next, _) = cv.wait_timeout(state, deadline - now).unwrap();
            state = next;
        }
        let dead = state.dead;
        drop(state);
        if dead {
            self.mark_dead_if_current(my_gen);
            return Err(std::io::Error::other("TTS child closed mid-cue"));
        }
        Ok(())
    }

    /// End an in-flight `listen` WITHOUT cancelling a concurrent `speak` (the
    /// `lstop` op). In full-duplex coexist a dictation and a reply run at once, so
    /// the STT path must end its listen alone; in half-duplex `lstop` ends the
    /// serve-loop listen just like `stop`. Fire-and-forget over stdin.
    pub fn stop_listen(&self) {
        let generation = self.active_listen_generation.load(Ordering::SeqCst);
        if generation == 0 {
            return;
        }
        self.listen_stopped_through
            .fetch_max(generation, Ordering::SeqCst);
        // Only START the wedge-recovery clock the FIRST time this generation is stopped — a
        // duplicate/late second `stop_listen()` call (e.g. `HelperStt::stop()` racing its own
        // detached joiner, or a caller retrying) must not push the finalize-timeout deadline
        // back out, which would delay `listen_finalize_timed_out`'s recovery kill instead of
        // just being a harmless no-op.
        let mut stop_started = self.listen_stop_started.lock().unwrap();
        if !matches!(*stop_started, Some((g, _)) if g == generation) {
            *stop_started = Some((generation, std::time::Instant::now()));
        }
        drop(stop_started);
        let request = serde_json::json!({
            "op": ds_helper_proto::HelperOp::Lstop,
            "session": generation,
        })
        .to_string();
        let _ = self.write_request(&request);
    }

    fn listen_finalize_timed_out(&self, generation: u64) -> bool {
        let started = self.listen_stop_started.lock().unwrap();
        let Some((stopped_generation, at)) = *started else {
            return false;
        };
        if stopped_generation != generation {
            return false;
        }
        at.elapsed() >= self.finalize_timeout_limit()
    }

    /// The wedge-recovery bound `listen_finalize_timed_out` waits out before `mark_dead()`.
    /// Apple's System recognizer intentionally has a 35s OS-level finalization bound;
    /// native Parakeet should settle much sooner. Both remain finite and recoverable.
    /// PRODUCTION path (outside `#[cfg(test)]` builds) ALWAYS returns one of these two
    /// literal values — `test_options.finalize_timeout` only exists in test builds, so a
    /// shipped binary can never take the override branch; see `tts::wedge_recovery_tests`
    /// for how a test exercises this in ~1-2s instead of the real 10s/35s window (#34) —
    /// floored by `listen_cancellable`'s pre-existing 1s `cv.wait_timeout` poll tick, not
    /// however small the override is set to.
    fn finalize_timeout_limit(&self) -> std::time::Duration {
        #[cfg(test)]
        if let Some(d) = self.test_options.finalize_timeout {
            return d;
        }
        let system = self
            .stt_provider_active
            .lock()
            .unwrap()
            .eq_ignore_ascii_case("system");
        if system {
            std::time::Duration::from_secs(35)
        } else {
            std::time::Duration::from_secs(10)
        }
    }

    /// The bound `start_locked`'s pre-READY wait enforces before killing the child.
    /// PRODUCTION path (outside `#[cfg(test)]` builds) ALWAYS returns
    /// [`READY_HANDSHAKE_TIMEOUT`] — `test_options.ready_timeout` only exists in test
    /// builds (same idiom as [`finalize_timeout_limit`](Self::finalize_timeout_limit)),
    /// so a shipped binary can never take the override branch.
    fn ready_handshake_timeout(&self) -> std::time::Duration {
        #[cfg(test)]
        if let Some(d) = self.test_options.ready_timeout {
            return d;
        }
        READY_HANDSHAKE_TIMEOUT
    }

    /// The bound on `cue_validated`'s CUEDONE wait. PRODUCTION path (outside `#[cfg(test)]`
    /// builds) ALWAYS returns [`CUE_TERMINAL_TIMEOUT`] — same idiom as
    /// [`ready_handshake_timeout`](Self::ready_handshake_timeout).
    fn cue_terminal_timeout(&self) -> std::time::Duration {
        #[cfg(test)]
        if let Some(d) = self.test_options.cue_timeout {
            return d;
        }
        CUE_TERMINAL_TIMEOUT
    }

    /// Test-only: force the TTS residency slot to `Loaded` without a live helper, so
    /// `ttsq`'s gate tests can flip readiness mid-`wait_until_ready` deterministically.
    #[cfg(test)]
    pub(crate) fn set_tts_loaded_for_test(&self) {
        self.tts_model
            .transition(ModelState::Loaded, self.gate.get().map(|g| g.as_ref()));
    }

    /// Test-only: force the "running child is full-duplex" flag without a live helper.
    /// `ttsq`'s earcon-routing tests neutralize `mic_holds` through the full-duplex arm
    /// so a dev machine's genuinely live microphone can't skew the hold under test.
    #[cfg(test)]
    pub(crate) fn set_full_duplex_active_for_test(&self, on: bool) {
        *self.full_duplex_active.lock().unwrap() = on;
    }

    /// Test-only: pre-arm the [`HEAL_COOLDOWN`] throttle so `restart_if_crashed` is a no-op.
    /// A queue test exercising the readiness wait must not spawn (and fail to spawn) the
    /// stub's nonexistent helper — that failure would land in `last_error` and short-circuit
    /// the wait under test.
    #[cfg(test)]
    pub(crate) fn suppress_heal_for_test(&self) {
        *self.last_heal.lock().unwrap() = Some(std::time::Instant::now());
    }

    /// Test-only: seed the TTS residency slot with a `Failed` state, simulating a
    /// cached `TTSLOADERR` from an earlier load attempt.
    #[cfg(test)]
    pub(crate) fn set_tts_load_error_for_test(&self, msg: &str) {
        self.set_tts_load_error(msg);
    }

    /// One-shot diarization on the warm helper: record `seconds` of mic, then return
    /// the `{"segments":[…]}` JSON (who spoke when). Starts the helper if needed.
    /// Blocks until the helper's terminal `DDONE`. Err ⇒ the helper reported a failure
    /// or died mid-diarize. Mutually exclusive with speak/listen (one capture thread).
    pub fn diarize(&self, seconds: u64) -> std::io::Result<String> {
        self.ensure_started();
        if !self.is_running() {
            return Err(std::io::Error::other("diarize helper not running"));
        }
        // Fresh job: clear any stale result / done / dead from a prior diarize.
        {
            let (m, _cv) = &*self.diarize_slot;
            let mut s = m.lock().unwrap();
            s.result = None;
            s.done = false;
            s.dead = false;
        }
        if let Err(e) = self.write_request(
            &serde_json::json!({
                "op": ds_helper_proto::HelperOp::Diarize,
                "seconds": seconds,
            })
            .to_string(),
        ) {
            self.mark_dead();
            return Err(e);
        }
        let (m, cv) = &*self.diarize_slot;
        let mut s = m.lock().unwrap();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(seconds.clamp(1, 60) + CAPTURE_TERMINAL_GRACE);
        loop {
            if s.done || s.dead {
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                drop(s);
                self.mark_dead();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "diarize helper timed out",
                ));
            }
            let (next, _) = cv.wait_timeout(s, deadline - now).unwrap();
            s = next;
        }
        match s.result.take() {
            Some(Ok(json)) => Ok(json),
            Some(Err(e)) => Err(std::io::Error::other(format!("diarize:{e}"))),
            None => {
                // DDONE/dead with no DIAR/DIARERR: child gone mid-diarize.
                drop(s);
                self.mark_dead();
                Err(std::io::Error::other("diarize helper closed mid-diarize"))
            }
        }
    }

    /// One-shot enrollment on the warm helper: record `seconds`, return the extracted
    /// WeSpeaker voiceprint as a `Vec<f32>`. Starts the helper if needed. Blocks until
    /// the terminal `EDONE`. Mutually exclusive with speak/listen/diarize.
    pub fn enroll(&self, seconds: u64) -> std::io::Result<Vec<f32>> {
        self.ensure_started();
        if !self.is_running() {
            return Err(std::io::Error::other("enroll helper not running"));
        }
        {
            let (m, _cv) = &*self.enroll_slot;
            let mut s = m.lock().unwrap();
            s.result = None;
            s.done = false;
            s.dead = false;
        }
        if let Err(e) = self.write_request(
            &serde_json::json!({
                "op": ds_helper_proto::HelperOp::Enroll,
                "seconds": seconds,
            })
            .to_string(),
        ) {
            self.mark_dead();
            return Err(e);
        }
        let (m, cv) = &*self.enroll_slot;
        let mut s = m.lock().unwrap();
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_secs(seconds.clamp(1, 60) + CAPTURE_TERMINAL_GRACE);
        loop {
            if s.done || s.dead {
                break;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                drop(s);
                self.mark_dead();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "enroll helper timed out",
                ));
            }
            let (next, _) = cv.wait_timeout(s, deadline - now).unwrap();
            s = next;
        }
        match s.result.take() {
            Some(Ok(json)) => serde_json::from_str::<Vec<f32>>(&json)
                .map_err(|e| std::io::Error::other(format!("enroll: bad embedding json: {e}"))),
            Some(Err(e)) => Err(std::io::Error::other(format!("enroll:{e}"))),
            None => {
                drop(s);
                self.mark_dead();
                Err(std::io::Error::other("enroll helper closed mid-enroll"))
            }
        }
    }
}

/// A realized-backend line's payload (`PROVIDER` for TTS, `STT_PROVIDER` for STT) →
/// realized backend token. A blank payload carries no observation, so it reads as
/// "nothing realized" rather than fail-closed "CPU": claiming nothing is strictly safer
/// than claiming a backend. An unrecognized non-blank token is still an observation —
/// `RealizedProvider::parse` fails it closed to CPU downstream.
fn realized_backend_token(payload: &str) -> Option<String> {
    let trimmed = payload.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Change-gated write of a realized-backend slot (TTS or STT), mirroring
/// [`ModelSlot::transition`]: bumps (and returns `true`) only on a real change. The bump
/// is load-bearing — see the paired-line race note at the STT provider call sites.
fn store_realized(
    slot: &Mutex<Option<String>>,
    token: Option<String>,
    gate: Option<&StatusGate>,
) -> bool {
    let mut guard = slot.lock().unwrap();
    if *guard != token {
        *guard = token;
        drop(guard);
        if let Some(g) = gate {
            g.bump();
        }
        return true;
    }
    false
}

fn provider_restart_needed(
    tts_preload: bool,
    desired: ds_config::RealizedProvider,
    active: Option<ds_config::RealizedProvider>,
) -> bool {
    match active {
        Some(active) => tts_preload && desired != active,
        None => false, // nothing realized yet ⇒ no live backend to switch away from
    }
}

/// Restart only to GAIN preload. The asymmetry is intentional: on true→false the child
/// keeps running — dropping an idle audio sink (the rodio stream just sits paused) is
/// not worth killing warm STT.
fn tts_preload_restart_needed(desired: bool, active: bool) -> bool {
    desired && desired != active
}

#[cfg(test)]
pub(crate) mod wedge_recovery_tests {
    use super::*;
    use std::time::Duration;

    /// Resolve the `dontspeakd-fake-helper` fixture's executable — `pub(crate)` because
    /// `ttsq.rs`'s readiness-gate tests spawn the same fixture.
    ///
    /// Cargo's own CARGO_BIN_EXE_<name> mechanism for locating a sibling `[[bin]]`
    /// target's executable is NOT available here — it's only set when building an
    /// INTEGRATION test/benchmark, not a unit test module like this one (confirmed: a
    /// `env!("CARGO_BIN_EXE_dontspeakd-fake-helper")` here is a hard compile error).
    /// Instead, resolve it relative to the CURRENTLY RUNNING test binary:
    /// `current_exe()` for a unit test binary is `target/<profile>/deps/dontspeakd-<hash>`,
    /// and Cargo places this crate's `[[bin]]` output in `target/<profile>` (its
    /// grandparent) — this self-adapts to profile (debug/release) and to a redirected
    /// `CARGO_TARGET_DIR`, unlike a path hardcoded relative to `CARGO_MANIFEST_DIR`
    /// (a hardcoded target/debug path would break under either variation).
    pub(crate) fn fake_helper_bin() -> std::path::PathBuf {
        let bin_name = if cfg!(windows) {
            "dontspeakd-fake-helper.exe"
        } else {
            "dontspeakd-fake-helper"
        };
        let bin = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent()?.parent().map(std::path::Path::to_path_buf))
            .map(|dir| dir.join(bin_name))
            .expect("could not resolve the running test binary's own directory");
        assert!(
            bin.exists(),
            "fixture not built at {} — run `cargo build --workspace` (or -p dontspeakd) first",
            bin.display()
        );
        // `cargo test --lib` builds THIS binary but not the sibling `[[bin]]`, so a fixture
        // left over from older sources keeps answering the protocol it was built with. That
        // reads as a bug in the code under test rather than a build gap (#217).
        if let Some(built) = modified_at(&bin)
            && let Some(source) = newest_source_mtime()
            && source > built
        {
            panic!(
                "fixture at {} predates its sources — `cargo test --lib` does not rebuild it; \
                 run `cargo test -p dontspeakd --no-run` (or `cargo build -p dontspeakd \
                 --bins`) and retry",
                bin.display()
            );
        }
        bin
    }

    /// Everything `dontspeakd-fake-helper` is compiled from: its own source and the shared
    /// protocol tokens it answers with.
    fn fixture_sources() -> [std::path::PathBuf; 2] {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        [
            crate_dir.join("src").join("bin").join("fake_ds_helper.rs"),
            crate_dir.join("..").join("ds-helper-proto").join("src"),
        ]
    }

    fn modified_at(path: &std::path::Path) -> Option<std::time::SystemTime> {
        std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok()
    }

    /// `None` when no source resolves — a relocated crate must degrade to the old
    /// existence-only check, never fail every test that spawns the fixture.
    /// `fixture_sources_resolve` is what keeps that from going unnoticed.
    fn newest_source_mtime() -> Option<std::time::SystemTime> {
        fn newest(path: &std::path::Path) -> Option<std::time::SystemTime> {
            if path.is_dir() {
                return std::fs::read_dir(path)
                    .ok()?
                    .filter_map(|entry| newest(&entry.ok()?.path()))
                    .max();
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                return None;
            }
            modified_at(path)
        }
        fixture_sources().iter().filter_map(|p| newest(p)).max()
    }

    /// The staleness guard is only as good as its input paths, and both are reached by a
    /// hardcoded relative path that a crate move would silently break.
    #[test]
    fn fixture_sources_resolve() {
        for source in fixture_sources() {
            assert!(
                source.exists(),
                "fixture source {} no longer exists — the #217 staleness guard now passes \
                 vacuously",
                source.display()
            );
        }
        assert!(newest_source_mtime().is_some());
    }

    /// One shared constructor for this module's managers (mirrors `ttsq.rs`'s `mk_queue`):
    /// tempdir-rooted log/lifetime paths plus the caller's helper binary — the nonexistent
    /// stub for the no-process pin tests, [`fake_helper_bin`] for the integration tests.
    fn mk_mgr_with(
        dir: &tempfile::TempDir,
        bin: std::path::PathBuf,
        opts: TtsManagerTestOptions,
    ) -> TtsManager {
        TtsManager::new_for_test(
            bin,
            dir.path().join("engine.log"),
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                dir.path().join("lifetime.json"),
            )),
            opts,
        )
    }

    /// [`mk_mgr_with`] with default options and the nonexistent helper — the pin tests' shape.
    fn mk_mgr(dir: &tempfile::TempDir) -> TtsManager {
        mk_mgr_with(
            dir,
            dir.path().join("ds-test-nonexistent-helper"),
            TtsManagerTestOptions::default(),
        )
    }

    #[test]
    fn speak_without_a_child_is_not_connected_and_clears_stale_progress() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = mk_mgr(&dir);
        mgr.speak_slot.0.lock().unwrap().progress = 9;

        let error = mgr
            .speak("hello", "af_sarah", "en", 1.0, &Default::default(), 0)
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotConnected);
        assert_eq!(mgr.last_speak_progress(), 0);
    }

    /// Pins the ACTUAL production 10s/35s values directly (no process, no real wait) —
    /// so a future edit to either number is a deliberate, visible diff. The integration
    /// test below deliberately does NOT wait out these real durations; it injects a short
    /// bound at construction.
    #[test]
    fn finalize_timeout_limit_pins_the_system_vs_native_bound() {
        let dir = tempfile::tempdir().unwrap();
        let tts = mk_mgr(&dir);
        assert_eq!(tts.finalize_timeout_limit(), Duration::from_secs(10));
        *tts.stt_provider_active.lock().unwrap() = "system".to_string();
        assert_eq!(tts.finalize_timeout_limit(), Duration::from_secs(35));
    }

    /// End-to-end through a REAL (fake) child: a `listen` that never gets a response
    /// must still recover — `mark_dead()` fires and kills the child within the
    /// configured bound, a `speak` queued behind the wedge (half-duplex shares this one
    /// child) fails fast rather than hanging, and the NEXT `ensure_started()` + `speak()`
    /// (the real `ttsq.rs` caller pattern) succeeds on a freshly restarted child.
    ///
    /// Both the helper log path and shortened finalize timeout are injected at construction,
    /// so this runs on every commit without touching live resources.
    #[test]
    fn a_wedged_listen_is_recovered_and_a_queued_speak_succeeds_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(mk_mgr_with(
            &dir,
            fake_helper_bin(),
            TtsManagerTestOptions::default().with_finalize_timeout(Duration::from_millis(50)),
        ));
        mgr.ensure_started();
        assert!(
            mgr.is_running(),
            "fixture failed to start: {:?}",
            mgr.last_error()
        );

        // Signalled from INSIDE listen_cancellable's own event loop (tts/mod.rs:1384,
        // `Some(ListenEvt::Partial(t)) => on_partial(&t)`) the moment the fixture's
        // `PARTIAL wedge-ack` is demuxed — i.e. only after write_request (line 1330)
        // has ACTUALLY run and the fixture has ACTUALLY read the "listen" line. This is
        // what closes the race: polling `active_listen_generation` (set at line
        // 1284-1285, well before the write) is NOT sufficient — a stop_listen() that
        // lands before the write hits the early-return guard at lines 1318-1321
        // (`listen_stopped_through >= generation` ⇒ `Ok(String::new())`) and the fixture
        // never wedges at all, silently defeating the test.
        let (wedge_ack_tx, wedge_ack_rx) = std::sync::mpsc::channel();
        let listen_mgr = mgr.clone();
        let listen = std::thread::spawn(move || {
            listen_mgr.listen_cancellable(&AtomicBool::new(false), &mut |_partial| {
                let _ = wedge_ack_tx.send(());
            })
        });

        assert!(
            wedge_ack_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "listen never reached the fake helper (no wedge-ack)"
        );

        mgr.stop_listen();

        // A speak queued RIGHT BEHIND the wedge, on the same (about-to-be-reaped) child.
        let speak_mgr = mgr.clone();
        let speak_attempt_1 = std::thread::spawn(move || {
            speak_mgr.speak("hello", "af_sarah", "en", 1.0, &Default::default(), 0)
        });

        let t0 = std::time::Instant::now();
        let listen_result = listen.join().expect("listen thread panicked");
        let elapsed = t0.elapsed();

        assert!(matches!(&listen_result, Err(e) if e.kind() == std::io::ErrorKind::TimedOut));
        assert!(
            elapsed < Duration::from_secs(5),
            "recovery took {elapsed:?}"
        );
        assert!(
            !mgr.is_running(),
            "the wedged child must be reaped by mark_dead"
        );

        let speak_1_result = speak_attempt_1.join().expect("speak thread panicked");
        assert!(
            speak_1_result.is_err(),
            "a speak on the wedged child must fail fast, not hang"
        );

        mgr.ensure_started();
        let speak_2_result =
            mgr.speak("hello again", "af_sarah", "en", 1.0, &Default::default(), 0);
        assert!(
            speak_2_result.is_ok(),
            "speak after the wedge is killed must succeed: {speak_2_result:?}"
        );

        mgr.set_enabled(false);
    }

    /// Pins the ACTUAL production READY-handshake bound directly (no process, no real
    /// wait) — so a future edit to the number is a deliberate, visible diff (mirrors
    /// `finalize_timeout_limit_pins_the_system_vs_native_bound`). The integration test
    /// below deliberately does NOT wait out the real 120 s; it injects a short bound.
    #[test]
    fn ready_handshake_timeout_pins_the_production_bound() {
        let dir = tempfile::tempdir().unwrap();
        let tts = mk_mgr(&dir);
        assert_eq!(tts.ready_handshake_timeout(), Duration::from_secs(120));
    }

    /// Issue #59: a spawned child that stays ALIVE without ever printing READY, ERR, or
    /// closing stdout (ORT provider init spinning, the model on a stalled mount, an AV
    /// scanner holding the `.onnx`) used to block `start_locked` — and the `lifecycle`
    /// lock, wedging every other lifecycle operation — forever. Now it is killed at the
    /// handshake bound, the manager parks in the ordinary "failed start" state, and the
    /// next start (here: with the wedge switch cleared) fully recovers the slot.
    #[test]
    fn a_child_that_wedges_before_ready_is_killed_at_the_handshake_bound() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = mk_mgr_with(
            &dir,
            fake_helper_bin(),
            TtsManagerTestOptions::default()
                .with_ready_timeout(Duration::from_millis(200))
                .with_first_spawn_env(&[("DONTSPEAK_FAKE_WEDGE_PRE_READY", "1")]),
        );

        let t0 = std::time::Instant::now();
        mgr.ensure_started();
        let elapsed = t0.elapsed();
        // Generous CI bound — the point is "finite", not "exactly 200 ms".
        assert!(
            elapsed < Duration::from_secs(5),
            "the handshake must be bounded, took {elapsed:?}"
        );
        assert!(
            !mgr.is_running(),
            "a wedged child must be killed, never installed"
        );
        assert!(
            mgr.last_error().is_some(),
            "a timed-out handshake parks the manager with last_error set"
        );

        // Recovery: the constructor-injected wedge switch was consumed by the first spawn,
        // so the same manager now starts a healthy child. This proves the wedged one was
        // killed (not orphaned holding the slot) and a successful start clears the failure.
        mgr.ensure_started();
        assert!(
            mgr.is_running(),
            "restart after the wedge must succeed: {:?}",
            mgr.last_error()
        );
        assert!(
            mgr.last_error().is_none(),
            "a successful start clears the parked error"
        );
        mgr.set_enabled(false);
    }

    /// Issue #213, driven through a REAL (fake) child for each pre-READY failure arm: STT
    /// preloads on a PARALLEL thread, so `STTLOADED` + `STT_PROVIDER` land before READY
    /// whenever the TTS half is the one that fails. The failed start must hand back the
    /// residency it accepted — the child is never installed, so no persistent reader will
    /// ever see its EOF and clear it, and `warm_child_heal_action` (absent + error)
    /// resolves to `Nothing`, so a stale green STT row would simply persist.
    fn pre_ready_failure_clears_residency(mode: &str) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = mk_mgr_with(
            &dir,
            fake_helper_bin(),
            TtsManagerTestOptions::default()
                .with_ready_timeout(Duration::from_millis(200))
                .with_first_spawn_env(&[("DONTSPEAK_FAKE_STT_LOADED_THEN", mode)]),
        );

        mgr.ensure_started();

        assert!(
            !mgr.is_running(),
            "[{mode}] the child must never be installed"
        );
        assert!(
            mgr.last_error().is_some(),
            "[{mode}] a failed start parks the manager with last_error set"
        );
        assert!(
            !mgr.is_stt_loaded(),
            "[{mode}] a pre-READY STTLOADED must not outlive the failed start"
        );
        assert!(!mgr.is_tts_loaded(), "[{mode}] no child, no TTS residency");
        assert_eq!(
            mgr.stt_realized_provider(),
            None,
            "[{mode}] the realized STT provider goes with the process that reported it"
        );
    }

    #[test]
    fn a_pre_ready_err_clears_the_residency_flags() {
        pre_ready_failure_clears_residency("err");
    }

    #[test]
    fn a_pre_ready_eof_clears_the_residency_flags() {
        pre_ready_failure_clears_residency("eof");
    }

    #[test]
    fn a_pre_ready_handshake_timeout_clears_the_residency_flags() {
        pre_ready_failure_clears_residency("hang");
    }

    /// Pins the ACTUAL production cue bound directly (no process, no real wait) — so a
    /// future edit to the number is a deliberate, visible diff (mirrors
    /// `ready_handshake_timeout_pins_the_production_bound`). The integration test below
    /// deliberately does NOT wait out the real 120 s; it injects a short bound.
    #[test]
    fn cue_terminal_timeout_pins_the_production_bound() {
        let dir = tempfile::tempdir().unwrap();
        let tts = mk_mgr(&dir);
        assert_eq!(tts.cue_terminal_timeout(), Duration::from_secs(120));
    }

    /// A helper that never answers a `cue` op with CUEDONE — exactly the shape of a stale
    /// helper binary predating the cue protocol (the fixture's `_ => {}` arm silently
    /// swallows `"cue"`) — must time out at the dedicated cue bound and reap the child,
    /// not ride the 600 s speak timeout with the audio queue wedged behind it.
    #[test]
    fn a_cue_with_no_cuedone_times_out_and_reaps_the_child() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = mk_mgr_with(
            &dir,
            fake_helper_bin(),
            TtsManagerTestOptions::default().with_cue_timeout(Duration::from_millis(100)),
        );

        mgr.ensure_started();
        assert!(
            mgr.is_running(),
            "fixture failed to start: {:?}",
            mgr.last_error()
        );

        let t0 = std::time::Instant::now();
        let result = mgr.cue_validated(&dir.path().join("cue.wav"));
        let elapsed = t0.elapsed();

        assert!(matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::TimedOut));
        // Generous CI bound — the point is "finite", not "exactly 100 ms".
        assert!(
            elapsed < Duration::from_secs(5),
            "cue wait took {elapsed:?}"
        );
        assert!(
            !mgr.is_running(),
            "the unanswered child must be reaped by mark_dead_if_current"
        );

        mgr.set_enabled(false);
    }
}

#[cfg(test)]
mod child_env_tests {
    use super::{SpawnPrefs, child_env};
    use ds_config::TtsModel;

    #[test]
    fn providers_always_set_conditionals_cleared_when_off() {
        // Both providers are ALWAYS `Some` (always overwrite any ambient value); the two
        // conditional flags are `Some("1")` when on and `None` when off — and `None` drives
        // `env_remove` in `start`, so an inherited `DONTSPEAK_FULL_DUPLEX=1` / `_STT_PRELOAD=1`
        // can't leak past the config-resolved intent.
        let on = child_env(&SpawnPrefs {
            provider: "cuda".into(),
            tts_model: TtsModel::Chatterbox,
            stt_provider: "mlx".into(),
            full_duplex: true,
            stt_preload: true,
            tts_preload: true,
        });
        assert_eq!(on[0], ("DONTSPEAK_PROVIDER", Some("cuda".into())));
        assert_eq!(on[1], ("DONTSPEAK_TTS_MODEL", Some("chatterbox".into())));
        assert_eq!(on[2], ("DONTSPEAK_STT_PROVIDER", Some("mlx".into())));
        assert_eq!(on[3], ("DONTSPEAK_FULL_DUPLEX", Some("1".into())));
        assert_eq!(on[4], ("DONTSPEAK_STT_PRELOAD", Some("1".into())));
        assert_eq!(on[5], ("DONTSPEAK_TTS_PRELOAD", Some("1".into())));

        let off = child_env(&SpawnPrefs {
            provider: "cpu".into(),
            tts_model: TtsModel::Kokoro,
            stt_provider: "cpu".into(),
            full_duplex: false,
            stt_preload: false,
            tts_preload: false,
        });
        assert_eq!(off[0], ("DONTSPEAK_PROVIDER", Some("cpu".into())));
        assert_eq!(off[1], ("DONTSPEAK_TTS_MODEL", Some("kokoro".into())));
        assert_eq!(off[2], ("DONTSPEAK_STT_PROVIDER", Some("cpu".into())));
        assert_eq!(off[3], ("DONTSPEAK_FULL_DUPLEX", None));
        assert_eq!(off[4], ("DONTSPEAK_STT_PRELOAD", None));
        assert_eq!(off[5], ("DONTSPEAK_TTS_PRELOAD", None));
    }
}

#[cfg(test)]
mod status_gate_tests {
    use super::*;

    /// A `TtsManager` with no real helper binary (never spawned in these tests — every
    /// function exercised here is "safe to call when stopped") and a fresh status-push
    /// gate wired in, so a bump can be observed via `gate.seq()`.
    fn mk() -> (TtsManager, Arc<StatusGate>) {
        let dir = tempfile::tempdir().unwrap();
        let tts = TtsManager::new(
            dir.path().join("ds-test-nonexistent-helper"),
            dir.path().join("engine.log"),
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                dir.path().join("ds-tts-status-gate-test-lifetime.json"),
            )),
        );
        let gate = StatusGate::new();
        tts.set_status_gate(gate.clone());
        (tts, gate)
    }

    fn write_verified_tts_fixture_marker(root: &std::path::Path, model: ds_config::TtsModel) {
        let mut rows = Vec::new();
        for file in ds_model::tts_ort_asset_set(model).files_for(false) {
            let metadata = std::fs::metadata(root.join(file.file_name)).unwrap();
            let modified = match metadata
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
            {
                Ok(duration) => i128::try_from(duration.as_nanos()).unwrap(),
                Err(error) => -i128::try_from(error.duration().as_nanos()).unwrap(),
            };
            rows.push(format!(
                "{}\t{}\t{}\t{}",
                file.file_name,
                file.sha256,
                metadata.len(),
                modified
            ));
        }
        rows.sort_unstable();
        let mut marker = format!("dontspeak-tts-pin-v1\t{}\n", model.as_str());
        marker.push_str(&rows.join("\n"));
        marker.push('\n');
        std::fs::write(
            root.join(format!(".dontspeak-{}-pin", model.as_str())),
            marker,
        )
        .unwrap();
    }

    #[test]
    fn realized_backend_token_parses_only_a_real_observation() {
        assert_eq!(realized_backend_token("MLX").as_deref(), Some("MLX"));
        assert_eq!(realized_backend_token("MLX ").as_deref(), Some("MLX"));
        assert_eq!(realized_backend_token(""), None);
        assert_eq!(realized_backend_token("   "), None);
        // Unknown-but-reported is still an observation; the fail-closed mapping to "cpu"
        // belongs to `RealizedProvider::parse`.
        assert_eq!(
            realized_backend_token("surprise").as_deref(),
            Some("surprise")
        );
    }

    #[test]
    fn store_realized_bumps_only_on_a_real_change() {
        // This bump is what stops a waiter woken by the paired `*LOADED` line from latching
        // a null it never gets corrected out of — only the `*_PROVIDER` line carries the token.
        let slot = Mutex::new(None);
        let gate = StatusGate::new();
        assert!(store_realized(&slot, Some("MLX".to_string()), Some(&gate)));
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "a fresh realized backend bumps the gate");
        assert!(!store_realized(&slot, Some("MLX".to_string()), Some(&gate)));
        assert_eq!(gate.seq(), seq1, "an identical repeat must not bump");
        assert!(store_realized(&slot, None, Some(&gate)));
        let seq2 = gate.seq();
        assert_ne!(seq2, seq1, "clearing a live token bumps");
        assert!(!store_realized(&slot, None, Some(&gate)));
        assert_eq!(gate.seq(), seq2, "a repeat clear must not bump");
    }

    #[test]
    fn teardown_clears_the_realized_tts_provider() {
        // `clear_loaded_flags` is the single teardown site for BOTH `stop_child` and
        // `mark_dead_locked`, so a reaped/crashed child can't leave its realized TTS token
        // (e.g. "CUDA") to be read with no process behind it. The clear is change-gated and
        // must be an observable bump for a blocked `WaitModelStatus` waiter.
        let (tts, gate) = mk();
        *tts.tts_realized.lock().unwrap() = Some("CUDA".to_string());
        let before = gate.seq();
        tts.clear_loaded_flags();
        assert_eq!(tts.provider(), None);
        assert_ne!(
            gate.seq(),
            before,
            "clearing a live TTS token must bump the gate"
        );
    }

    #[test]
    fn set_error_bumps_gate_only_on_a_real_change() {
        // A blocked WaitModelStatus must see a start failure land immediately (last_error
        // is surfaced per-engine in model_status) — and must NOT be woken for a repeat of
        // the SAME error, which would spam every failed retry.
        let (tts, gate) = mk();
        tts.set_error("kokoro model not downloaded");
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "a fresh error bumps the gate");

        tts.set_error("kokoro model not downloaded");
        assert_eq!(gate.seq(), seq1, "the identical error must not bump again");

        tts.set_error("onnxruntime dylib mismatch");
        assert_ne!(gate.seq(), seq1, "a DIFFERENT error bumps again");
    }

    #[test]
    fn clear_error_bumps_gate_only_when_an_error_was_actually_set() {
        let (tts, gate) = mk();
        tts.clear_error();
        assert_eq!(gate.seq(), 0, "clearing a not-set error must not bump");

        tts.set_error("boom");
        let seq_after_set = gate.seq();
        tts.clear_error();
        assert_ne!(
            gate.seq(),
            seq_after_set,
            "resolving a real error bumps the gate"
        );
    }

    #[test]
    fn mark_loaded_bumps_gate_only_on_a_real_transition() {
        // Section E's periodic self-heal reconcile can re-report an ALREADY-loaded model's
        // STTLOADED/TTSLOADED repeatedly (e.g. every 20s tick); each repeat must be a no-op
        // for the gate — otherwise StatusGate spam reintroduces the poll-churn it exists to
        // eliminate (mirrors set_error/clear_error's own change-gating above). Exercised
        // directly on `ModelSlot::transition`, shared by the pre-READY wait loop and the
        // persistent `reader_loop`.
        let slot = ModelSlot::new();
        let gate = StatusGate::new();

        slot.transition(ModelState::Loaded, Some(&gate));
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "the FIRST transition to loaded bumps the gate");
        assert!(slot.is_loaded());

        slot.transition(ModelState::Loaded, Some(&gate));
        assert_eq!(
            gate.seq(),
            seq1,
            "an already-loaded model reported loaded again must NOT bump"
        );
    }

    #[test]
    #[cfg(unix)]
    fn unload_engine_bumps_gate_only_on_a_real_transition() {
        // Section E's unconditional 20s-tick `reconcile_helper_models` call means
        // `unload_engine` can now be invoked for an engine that is ALREADY unloaded —
        // every repeat must be a no-op for the gate, or the periodic tick wakes every
        // connected client forever with no real state change (mirrors
        // `mark_loaded_bumps_gate_only_on_a_real_transition` above). `unload_engine` only
        // does anything once `write_request` succeeds, so this needs a real child with a
        // live piped stdin (a canned `stdin: None` would make the whole call a no-op and
        // prove nothing).
        let (tts, gate) = mk();
        let mut child = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn `cat`");
        *tts.stdin.lock().unwrap() = child.stdin.take();

        // Simulate a genuinely loaded TTS engine, then unload it: a REAL true→false
        // transition, so the first call must bump.
        tts.tts_model.transition(ModelState::Loaded, None);
        tts.unload_engine(ds_helper_proto::HelperModel::Tts);
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "a real loaded→unloaded transition bumps the gate");
        assert!(!tts.tts_model.is_loaded());

        // Repeat on an already-unloaded engine: no real transition, must NOT bump again.
        tts.unload_engine(ds_helper_proto::HelperModel::Tts);
        assert_eq!(
            gate.seq(),
            seq1,
            "unloading an already-unloaded engine must NOT bump again"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn set_load_error_bumps_gate_only_on_a_real_change() {
        // Same change-gating as set_error, exercised directly on `ModelSlot::transition`,
        // shared by the pre-READY wait loop and the persistent reader_loop.
        let slot = ModelSlot::new();
        let gate = StatusGate::new();

        slot.transition(
            ModelState::Failed("read encoder.int8.onnx: os error 2".into()),
            Some(&gate),
        );
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "a fresh load error bumps the gate");

        slot.transition(
            ModelState::Failed("read encoder.int8.onnx: os error 2".into()),
            Some(&gate),
        );
        assert_eq!(
            gate.seq(),
            seq1,
            "an IDENTICAL repeat (e.g. the same transient AV-scan failure recurring) must not bump"
        );

        slot.transition(
            ModelState::Failed("a different failure".into()),
            Some(&gate),
        );
        assert_ne!(gate.seq(), seq1, "a DIFFERENT message bumps again");
    }

    #[test]
    fn clear_load_error_bumps_gate_only_when_an_error_was_actually_set() {
        let slot = ModelSlot::new();
        let gate = StatusGate::new();

        slot.clear_error(Some(&gate));
        assert_eq!(gate.seq(), 0, "clearing a not-set load error must not bump");

        slot.transition(ModelState::Failed("boom".into()), Some(&gate));
        let seq_after_set = gate.seq();
        slot.clear_error(Some(&gate));
        assert_ne!(
            gate.seq(),
            seq_after_set,
            "resolving a real load error bumps the gate"
        );
    }

    #[test]
    fn stop_child_bumps_gate_only_when_a_model_was_actually_loaded() {
        // stop_child's own comment says "so the dot doesn't show a stale running" —
        // that's only true if a blocked WaitModelStatus is actually woken. No child is
        // spawned here (child stays None), exactly the "safe to call when stopped" path.
        let (tts, gate) = mk();
        tts.stop_child();
        assert_eq!(
            gate.seq(),
            0,
            "stopping an already-idle child must not bump"
        );

        tts.tts_model.transition(ModelState::Loaded, None);
        tts.stop_child();
        assert_ne!(
            gate.seq(),
            0,
            "tearing down a LOADED model bumps the gate immediately"
        );
        assert!(!tts.tts_model.is_loaded());
    }

    #[test]
    fn mark_dead_locked_bumps_gate_only_when_a_model_was_actually_loaded() {
        let (tts, gate) = mk();
        tts.mark_dead_locked();
        assert_eq!(gate.seq(), 0, "reaping an already-idle child must not bump");

        tts.stt_model.transition(ModelState::Loaded, None);
        tts.mark_dead_locked();
        assert_ne!(
            gate.seq(),
            0,
            "reaping a crashed child with a resident model bumps the gate immediately"
        );
        assert!(!tts.stt_model.is_loaded());
    }

    /// Both native dylibs present -- the resolve tests below pin the token half.
    const BOTH_SHIMS: crate::config_gate::NativeShims = crate::config_gate::NativeShims {
        mlx: true,
        fluid: true,
    };

    #[test]
    fn provider_resolution_is_deterministic_for_explicit_onnx_tokens() {
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "cpu",
                ds_config::TtsModel::Kokoro,
                Some(BOTH_SHIMS),
                true,
            ),
            ds_config::RealizedProvider::Cpu
        );
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "coreml",
                ds_config::TtsModel::Kokoro,
                None,
                false,
            ),
            ds_config::RealizedProvider::CoreMl
        );
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "cuda",
                ds_config::TtsModel::Chatterbox,
                None,
                true,
            ),
            ds_config::RealizedProvider::Cuda
        );
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "cuda",
                ds_config::TtsModel::Chatterbox,
                None,
                false,
            ),
            ds_config::RealizedProvider::Cpu
        );
        // OmniVoice ships a CUDA profile too, but only a present runtime realizes it — the
        // same predicate that decides whether its CUDA-only assets are downloaded.
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "cuda",
                ds_config::TtsModel::OmniVoice,
                None,
                true,
            ),
            ds_config::RealizedProvider::Cuda
        );
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "cuda",
                ds_config::TtsModel::OmniVoice,
                None,
                false,
            ),
            ds_config::RealizedProvider::Cpu
        );
    }

    /// The TTS mirror of `config_gate`'s cross-family matrix: `fluid` and `mlx` are separate
    /// dylibs, so each token must read only its own. A shared bool would realize `Mlx` here
    /// on a Fluid-only host and then fail at dlopen.
    #[test]
    fn provider_resolution_reads_only_the_selected_families_dylib() {
        let fluid_only = crate::config_gate::NativeShims {
            mlx: false,
            fluid: true,
        };
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "fluid",
                ds_config::TtsModel::Kokoro,
                Some(fluid_only),
                false,
            ),
            ds_config::RealizedProvider::Fluid
        );
        assert_eq!(
            TtsManager::resolve_provider_with_availability(
                "mlx",
                ds_config::TtsModel::Kokoro,
                Some(fluid_only),
                false,
            ),
            ds_config::RealizedProvider::Cpu
        );
    }

    #[test]
    fn absent_tts_assets_never_restart_the_shared_helper() {
        use ds_config::RealizedProvider::{Cpu, Cuda};

        assert!(!provider_restart_needed(false, Cpu, Some(Cuda)));
        assert!(provider_restart_needed(true, Cpu, Some(Cuda)));
        assert!(!provider_restart_needed(true, Cpu, Some(Cpu)));
        // Nothing realized yet ⇒ no live backend to switch away from, at either preload.
        assert!(!provider_restart_needed(true, Cuda, None));
        assert!(!provider_restart_needed(false, Cuda, None));
        assert!(!tts_preload_restart_needed(false, true));
        assert!(tts_preload_restart_needed(true, false));
    }

    #[test]
    fn setters_write_the_expected_field_into_spawn_prefs_while_stopped() {
        // set_provider's early-return-false-when-stopped path is already covered
        // elsewhere; what's missing is confirming the OTHER three setters — which have
        // no return value to assert on — actually persisted into spawn_prefs, the
        // struct start_locked reads from on the next real start.
        let (tts, _gate) = mk();

        tts.set_full_duplex_pref(true);
        assert!(tts.spawn_prefs.lock().unwrap().full_duplex);
        tts.set_full_duplex_pref(false);
        assert!(!tts.spawn_prefs.lock().unwrap().full_duplex);

        tts.set_stt_provider_pref("cuda");
        assert_eq!(tts.spawn_prefs.lock().unwrap().stt_provider, "cuda");

        tts.set_stt_wanted(true);
        assert!(tts.spawn_prefs.lock().unwrap().stt_preload);
        tts.set_stt_wanted(false);
        assert!(!tts.spawn_prefs.lock().unwrap().stt_preload);

        tts.set_tts_wanted(true);
        assert!(tts.spawn_prefs.lock().unwrap().tts_preload);
        tts.set_tts_wanted(false);
        assert!(!tts.spawn_prefs.lock().unwrap().tts_preload);

        // set_provider's own persisted-value half of its contract (the early-return
        // is covered elsewhere; this confirms the write happens before that check).
        assert!(!tts.set_provider("cpu"));
        assert_eq!(tts.spawn_prefs.lock().unwrap().provider, "cpu");
    }

    #[test]
    fn listen_lease_rejects_a_second_untagged_event_consumer() {
        let (tts, _gate) = mk();
        let _owner = tts.listen_lease.lock().unwrap();
        assert!(tts.listen_lease.try_lock().is_err());
    }

    #[test]
    fn stop_listen_records_the_active_generation_even_without_a_live_helper() {
        let (tts, _gate) = mk();
        tts.active_listen_generation.store(7, Ordering::SeqCst);
        tts.stop_listen();
        assert_eq!(tts.listen_stopped_through.load(Ordering::SeqCst), 7);
        assert!(tts.listen_stop_started.lock().unwrap().is_some());
    }

    #[test]
    fn stop_listen_does_not_restart_the_finalize_clock_on_a_duplicate_call() {
        // A second `stop_listen()` for the SAME generation (e.g. `HelperStt::stop()` racing
        // its own detached joiner) must not push `listen_finalize_timed_out`'s deadline back
        // out — that would delay wedge-recovery instead of being the harmless no-op it should
        // be for an already-recorded stop.
        let (tts, _gate) = mk();
        tts.active_listen_generation.store(3, Ordering::SeqCst);
        tts.stop_listen();
        let first = tts.listen_stop_started.lock().unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        tts.stop_listen();
        let second = tts.listen_stop_started.lock().unwrap().unwrap();

        assert_eq!(first.0, second.0, "same generation both times");
        assert_eq!(
            first.1, second.1,
            "the recorded Instant must not move on a duplicate stop"
        );
    }

    /// `stop()` may set the cancel flag before `listen` starts; must still short-circuit
    /// (not no-op on generation 0). End-to-end on production path; no helper process.
    #[test]
    fn listen_cancellable_honors_a_stop_that_raced_the_call_itself() {
        let (tts, _gate) = mk();
        let cancelled_early = AtomicBool::new(true); // stop() already fired before we got here
        let result = tts.listen_cancellable(&cancelled_early, &mut |_| {
            panic!("a session cancelled before it started must never deliver a partial");
        });
        assert_eq!(result.unwrap(), "");
        // The Drop guard released the generation — a later, unrelated listen must not
        // inherit a stale "already cancelled" state from this one.
        assert_eq!(tts.active_listen_generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn start_locked_skips_the_spawn_when_kokoro_is_not_present() {
        // The new cheap presence gate: on a fresh install / provider switch, before the
        // Kokoro model has been downloaded, `start_locked` must skip the spawn entirely
        // rather than pay the guaranteed-fail "kokoro model not downloaded" transient.
        // An EMPTY model dir with no dylib forces the ONNX branch on every OS.
        const TEST: &str =
            "tts::status_gate_tests::start_locked_skips_the_spawn_when_kokoro_is_not_present";
        let Some(_child) = crate::test_env::child_run() else {
            let tmp = tempfile::tempdir().unwrap();
            crate::test_env::run_child(
                TEST,
                crate::test_env::ChildEnv {
                    phase: "empty-model-dir",
                    model_dir: tmp.path(),
                    ort_dylib: None,
                },
            );
            return;
        };

        let (tts, gate) = mk();
        tts.set_tts_wanted(true);
        let seq0 = gate.seq();

        tts.start();

        assert!(
            !tts.is_running(),
            "the gate must skip the spawn, never even reaching Command::spawn"
        );
        assert_eq!(
            tts.last_error(),
            Some(ds_i18n::t("status.engine.reason.tts_failed")),
            "a skipped spawn surfaces the same start-error key every other early return uses"
        );
        assert_ne!(
            gate.seq(),
            seq0,
            "a fresh skip must bump the status-push gate"
        );
    }

    #[test]
    fn start_against_a_nonexistent_binary_sets_last_error_and_bumps_the_gate() {
        // Exercises start_locked's real spawn-failure branch (Command::spawn erroring on
        // a path that doesn't exist) — no mock, no real ds-helper binary needed. With the
        // presence gate now wired in, this must route through fixture files that read
        // "present" (else it would exercise the new skip path instead of ever reaching
        // Command::spawn, on any host whose real ambient model cache happens to be empty
        // OR already populated).
        const TEST: &str = "tts::status_gate_tests::start_against_a_nonexistent_binary_sets_last_error_and_bumps_the_gate";
        let Some(_child) = crate::test_env::child_run() else {
            let tmp = tempfile::tempdir().unwrap();
            // All four KOKORO_FILES: the presence gate also requires the two G2P files.
            for file in [
                ds_model::KOKORO_ONNX_FILE,
                ds_model::KOKORO_VOICES_FILE,
                ds_model::KOKORO_G2P_ENCODER_FILE,
                ds_model::KOKORO_G2P_DECODER_FILE,
            ] {
                std::fs::write(tmp.path().join(file), b"dummy").unwrap();
            }
            // This test targets Command::spawn, so seed the checksum-provenance marker that a
            // completed model install would have written for these synthetic fixture files.
            write_verified_tts_fixture_marker(tmp.path(), ds_config::TtsModel::Kokoro);
            let dylib = tmp.path().join("dummy-onnxruntime.dylib");
            std::fs::write(&dylib, b"dummy").unwrap();
            crate::test_env::run_child(
                TEST,
                crate::test_env::ChildEnv {
                    phase: "kokoro-files-present",
                    model_dir: tmp.path(),
                    ort_dylib: Some(&dylib),
                },
            );
            return;
        };

        let (tts, gate) = mk();
        tts.set_tts_wanted(true);
        assert!(
            TtsManager::tts_assets_ready(&tts.spawn_prefs.lock().unwrap().clone()),
            "fixture must read present — a not-ready gate would divert this test onto the \
             skip path without ever reaching Command::spawn"
        );
        assert_eq!(tts.last_error(), None);
        let seq0 = gate.seq();

        tts.start();

        assert!(!tts.is_running(), "a failed spawn must leave no child");
        assert!(
            tts.last_error().is_some(),
            "a spawn failure must surface as a start error"
        );
        assert_ne!(
            gate.seq(),
            seq0,
            "a fresh start failure must bump the status-push gate"
        );
    }
}

#[cfg(test)]
mod system_mute_tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn mk() -> TtsManager {
        let dir = tempfile::tempdir().unwrap();
        TtsManager::new(
            dir.path().join("ds-test-nonexistent-helper"),
            dir.path().join("engine.log"),
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                dir.path().join("ds-system-mute-test-lifetime.json"),
            )),
        )
    }

    /// Long-lived local process injected in place of System TTS.
    fn long_lived_fake_command() -> Command {
        #[cfg(unix)]
        {
            let mut cmd = Command::new("sleep");
            cmd.arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut cmd = Command::new("powershell");
            cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            cmd
        }
    }

    fn spawn_long_lived_fake() -> std::process::Child {
        long_lived_fake_command()
            .spawn()
            .expect("spawn long-lived fake")
    }

    #[test]
    fn speak_system_when_muted_returns_ok_without_stop_or_spawn() {
        let tts = mk();
        tts.set_muted(true);
        assert!(tts.is_muted());

        let child = spawn_long_lived_fake();
        let pid = child.id();
        *tts.say_child.lock().unwrap() = Some(child);

        tts.speak_system("hello", "", "", 1.0)
            .expect("muted system speak consumes without error");

        let mut guard = tts.say_child.lock().unwrap();
        let still = guard
            .as_mut()
            .expect("muted speak_system must not stop()/kill the pre-installed child");
        assert_eq!(still.id(), pid, "same injected process");
        assert!(
            still.try_wait().expect("try_wait").is_none(),
            "injected child must still be running (no stop/kill)"
        );
        let _ = still.kill();
        let _ = still.wait();
        *guard = None;
    }

    #[test]
    fn set_muted_true_kills_in_flight_say_child() {
        let tts = mk();
        *tts.say_child.lock().unwrap() = Some(spawn_long_lived_fake());

        tts.set_muted(true);

        assert!(
            tts.say_child.lock().unwrap().is_none(),
            "mute must clear the system-speech slot"
        );
        tts.set_muted(true);
        assert!(tts.say_child.lock().unwrap().is_none());
    }

    #[test]
    fn system_child_install_rechecks_mute_before_spawn() {
        let tts = mk();
        tts.set_muted(true);
        let mut cmd = long_lived_fake_command();

        assert!(
            !tts.spawn_say_child_if_unmuted(&mut cmd)
                .expect("muted install is consumed")
        );
        assert!(tts.say_child.lock().unwrap().is_none());
    }

    #[test]
    fn set_muted_false_does_not_clear_say_child() {
        let tts = mk();
        let child = spawn_long_lived_fake();
        let pid = child.id();
        *tts.say_child.lock().unwrap() = Some(child);

        tts.set_muted(false);

        let mut guard = tts.say_child.lock().unwrap();
        let still = guard
            .as_mut()
            .expect("unmute must not kill in-flight system speech");
        assert_eq!(still.id(), pid);
        assert!(still.try_wait().expect("try_wait").is_none());
        let _ = still.kill();
        let _ = still.wait();
        *guard = None;
    }
}
