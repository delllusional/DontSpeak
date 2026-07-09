//! TtsManager — the engine's warm Kokoro owner (Phase 2).
//!
//! The engine supervises ONE long-lived `ds-helper --serve` child that holds
//! the ~325 MB model warm, so no reply pays the cold model-load cost. Enabling TTS
//! spawns the child; disabling KILLS it (freeing the model with no ONNX-teardown
//! crash, since a killed process runs no destructors). Speak/preview/stop are
//! mediated over the child's stdio with the protocol documented in
//! `ds_helper/main.rs`.
//!
//! Concurrency (full-duplex coexist): ONE persistent reader thread owns the
//! child's stdout and DEMUXES its lines into two slots — a [`SpeakSlot`] (DONE/
//! STATS/ERR) and a [`ListenSlot`] (LISTENING/PARTIAL/FINAL/STTSTATS/
//! STTERR/LDONE). A `speak` waits on the speak slot while a `listen` drains the
//! listen slot AT THE SAME TIME — neither holds stdout, so they run concurrently
//! (dictate while the voice talks). `stop` only takes the brief `stdin` lock, so
//! barge-in still works while a speak is mid-flight.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use ds_helper_proto as proto;

use crate::child_slot::ChildSlot;
use crate::log;
use crate::model_slot::{ModelSlot, ModelState};
use crate::status::StatusGate;

mod reader;
use reader::*;

/// Where the warm child's stderr is sent (the app has no console, so anything that lands here
/// must go to a file). Routed through the shared `open_aux_log` so it sits BESIDE the engine's
/// unified log in the one per-OS logs dir AND is size-rotated like it (never a second location,
/// never unbounded). Null only if it can't open.
///
/// Routine `ds-helper` diagnostics (load attempts, unload confirmations, capture stats,
/// full-duplex active, wav dump ok, separator loaded, stream picked, and failure/fallback
/// conditions) now go through the unified activity log (source `helper`) via `ds-helper`'s own
/// `logging::log`, NOT this raw stderr redirect. This redirect is retained purely as a
/// last-resort safety net for anything that bypasses explicit logging entirely — a native-
/// library abort, an unhandled panic, or a startup failure before `ds_log::log_cached` can
/// even initialize.
fn helper_stderr() -> Stdio {
    ds_config::Paths::resolve()
        .and_then(|p| ds_log::open_aux_log(&p.log_file, "ds-helper.log"))
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null)
}

/// The daemon→helper env contract, resolved from the warm child's spawn prefs. Every flag is
/// returned as `Some(value)` to SET or `None` to CLEAR, so [`TtsManager::start`] applies the
/// whole set with ONE uniform set-or-remove pass. Clearing is the point: the two conditional
/// flags (`FULL_DUPLEX`, `STT_PRELOAD`) are `None` when off and get `env_remove`d, so an
/// inherited ambient value (e.g. the daemon itself was launched with `DONTSPEAK_FULL_DUPLEX=1`)
/// can NEVER leak into the child and override the config-resolved intent. The two provider
/// tokens are always `Some` (always overwritten), so a new conditional flag can't reintroduce
/// the leak — it just joins the table and inherits the clear-when-absent behaviour.
fn child_env(prefs: &SpawnPrefs) -> [(&'static str, Option<String>); 4] {
    [
        ("DONTSPEAK_PROVIDER", Some(prefs.provider.clone())),
        ("DONTSPEAK_STT_PROVIDER", Some(prefs.stt_provider.clone())),
        (
            "DONTSPEAK_FULL_DUPLEX",
            prefs.full_duplex.then(|| "1".to_string()),
        ),
        (
            "DONTSPEAK_STT_PRELOAD",
            prefs.stt_preload.then(|| "1".to_string()),
        ),
    ]
}

/// The next warm child's spawn preferences — what [`child_env`] resolves into the
/// daemon→helper env contract. Bundled into one struct behind one `Mutex` because every
/// reader (`start_locked`, assembling the spawn env) and every writer (`set_provider`/
/// `set_full_duplex_pref`/`set_stt_provider_pref`/`set_stt_wanted`) always touches the
/// whole logical "next child's prefs" value — four independent locks around four
/// fields that only ever change and get read together was needless ceremony (and four
/// separate acquisitions where one now suffices).
#[derive(Clone)]
struct SpawnPrefs {
    /// The user's provider PREFERENCE ("auto"|"cpu"|"cuda"|"coreml"|"ane") — drives the
    /// `DONTSPEAK_PROVIDER` env when (re)starting the warm child.
    provider: String,
    /// The local STT backend the next warm child should use — the resolved provider
    /// token ("cpu"|"cuda"|"ane"|"system"), from `helper_stt_provider`. Drives the
    /// `DONTSPEAK_STT_PROVIDER` env when (re)starting.
    stt_provider: String,
    /// Whether the next warm child should run in full-duplex AEC mode — the engine
    /// sets this to `full_duplex && stt provider == cpu`. Drives the
    /// `DONTSPEAK_FULL_DUPLEX` env when (re)starting the child.
    full_duplex: bool,
    /// Whether STT (Parakeet) should be PRELOADED in the warm child — `helper_uses_stt(cfg)`,
    /// i.e. STT is the built-in engine. `stt_provider` is NOT a usable on/off signal (it
    /// resolves to "cpu" even for Off/ClaudeCode), so this is tracked separately and
    /// drives `DONTSPEAK_STT_PRELOAD`, which gates the helper's parallel STT-preload thread.
    stt_preload: bool,
}

pub struct TtsManager {
    /// Path to the `ds-helper` helper binary.
    bin: PathBuf,
    /// Serializes warm-child LIFECYCLE transitions — `start` / `stop_child` /
    /// `mark_dead`. Without it, a crash-driven `mark_dead` from a concurrent
    /// play+listen pair (both wake fatal on the same EOF) could race a restart and
    /// `join` the WRONG reader. The OUTERMOST lock: always taken before
    /// `child`/`stdin`/`reader`. Brief, never held across a slot `Condvar` wait.
    lifecycle: Mutex<()>,
    /// When `restart_child` last actually bounced the process — gates its debounce
    /// (see `restart_child`) and doubles as a debug aid for spotting a rapid-fire
    /// restart storm. `None` until the first restart.
    last_restart: Mutex<Option<std::time::Instant>>,
    /// The warm child's process-lifecycle slot — the live Kokoro `--serve` handle
    /// (empty when not warm), its incarnation number, and the deliberate-teardown
    /// marker, consolidated behind named transitions so the three can't drift
    /// apart by convention (see [`ChildSlot`]). `Arc` so the persistent reader
    /// thread can share it too — its unexpected-EOF handler classifies the EOF
    /// (deliberate stop vs post-READY crash) and `try_wait`s the child (peek only,
    /// never taking/killing) to log the real exit status/signal instead of just
    /// "died, reason unknown"; the actual reap still happens later via
    /// `mark_dead`/`restart_if_crashed`.
    child: Arc<ChildSlot>,
    /// Kokoro child stdin — written by speak/preview/listen AND stop (brief sections).
    stdin: Mutex<Option<ChildStdin>>,
    /// The persistent stdout reader thread (one per warm child). Owns the child's
    /// `BufReader<ChildStdout>` and demuxes into the slots below. Joined by
    /// `stop_child`/`mark_dead` (after the child is killed → reader EOFs) so no
    /// stale reader races the next start's slots.
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Filled by the reader: a `speak`/`preview` waits here for its terminal DONE
    /// (or ERR/EOF). Reset at the start of each `play()`.
    speak_slot: Arc<(Mutex<SpeakSlot>, Condvar)>,
    /// Filled by the reader: a `listen` drains LISTENING/PARTIAL/FINAL/STTERR/LDONE
    /// events here. Cleared at the start of each `listen()`. Demuxing the one
    /// stdout into separate slots is what lets a speak and a listen coexist.
    listen_slot: Arc<(Mutex<ListenSlot>, Condvar)>,
    /// Filled by the reader: a one-shot `diarize` waits here for its DIAR/DIARERR +
    /// terminal DDONE. Cleared at the start of each `diarize()`. Its own slot (not
    /// `listen_slot`) so a diarize and a speak demux independently.
    diarize_slot: Arc<(Mutex<DiarizeSlot>, Condvar)>,
    /// Filled by the reader: a one-shot `enroll` waits here for its EMB/ENROLLERR +
    /// terminal EDONE. Cleared at the start of each `enroll()`.
    enroll_slot: Arc<(Mutex<EnrollSlot>, Condvar)>,
    /// The in-flight macOS `say` process (System engine), so a barge-in/stop can
    /// kill it. System TTS has no warm model — it spawns per request.
    say_child: Mutex<Option<Child>>,
    /// Last warm-child START failure (e.g. "onnxruntime dylib is not <ver>",
    /// "kokoro model not downloaded"), surfaced to the app's status dot as the
    /// red "failed" state. `None` once a start succeeds or TTS is toggled off.
    /// Invariant: only ever `Some` while no child is installed — set exclusively
    /// by `start_locked`'s failure returns, cleared by success and `stop_child`.
    last_error: Mutex<Option<String>>,
    /// Live TTS stats (realtime factor / latency / counts) for the app's stats
    /// view, fed by the child's per-utterance `STATS` line.
    stats: Arc<crate::stats::TtsStats>,
    /// Live STT stats, fed by the helper's per-listen `STTSTATS` line.
    stt_stats: Arc<crate::stats::SttStats>,
    /// Persisted lifetime seconds (spoken + heard), bumped from the same reader as
    /// the live stats. Survives across sessions — see [`crate::stats::LifetimeSeconds`].
    lifetime: Arc<crate::stats::LifetimeSeconds>,
    /// The warm child's active TTS ONNX execution provider ("CPU"/"CoreML"/"CUDA"), reported
    /// via its `PROVIDER` line at startup. For the engine stats.
    provider: Mutex<String>,
    /// The warm child's active STT execution provider ("CPU"/"CUDA"/"CoreML-ANE"/"System"),
    /// reported via its `STT_PROVIDER` line — the SAME realized-EP channel as `provider`, so the
    /// STT status row shows what ACTUALLY loaded (not a preference), mapped through the one shared
    /// `realized_ort_token`. Starts "CPU".
    stt_realized: Arc<Mutex<String>>,
    /// The next warm child's spawn preferences (provider/STT-provider/full-duplex/
    /// STT-preload) — see [`SpawnPrefs`]. Read together by `start_locked` to assemble
    /// the spawn env; written field-by-field by `set_provider`/`set_full_duplex_pref`/
    /// `set_stt_provider_pref`/`set_stt_wanted`.
    spawn_prefs: Mutex<SpawnPrefs>,
    /// The full-duplex mode the CURRENTLY running child was started with, so a
    /// changed `spawn_prefs.full_duplex` can trigger exactly one restart (mirrors how
    /// `provider` tracks the running provider for `set_provider`).
    full_duplex_active: Mutex<bool>,
    /// The STT engine the CURRENTLY running child was started with, so a changed
    /// `spawn_prefs.stt_provider` triggers exactly one restart (mirrors `full_duplex_active`).
    stt_provider_active: Mutex<String>,
    /// Which models are CURRENTLY resident in the warm helper. Kokoro is eager (true
    /// once the child is READY); Parakeet is lazy (true after the first `listen`). An
    /// `unload` clears the matching flag; the helper stopping clears both. Surfaced in
    /// `model_status` because the memory number is too noisy (ort retains freed arena).
    /// `Arc` (like `stt_loaded`) so the stdout reader can drop it on an UNEXPECTED EOF —
    /// a post-READY child death must unload both models immediately, not when the next
    /// write happens to fail. `Arc` so the persistent reader thread shares the same slot.
    tts_model: Arc<ModelSlot>,
    /// The STT (Parakeet) counterpart of `tts_model`: residency flips true on the helper's
    /// `STTLOADED` confirmation (emitted after preload + the graph WARMUP) — the dot only
    /// greens when the model is truly resident AND warm, not optimistically on the load
    /// request — and its load-error state (from `STTLOADERR`) so `model_status`'s `parakeet`
    /// row can show a failure without also tripping `kokoro`'s. Distinct from `last_error`
    /// (warm-CHILD start failures): this is per-MODEL.
    stt_model: Arc<ModelSlot>,
    /// Global MUTE: when true the warm child plays silence (queue still drains; only the audio
    /// is zeroed). Toggled by a Caps-tap (dictation off) and the tray checkbox. Read by the
    /// status snapshot; pushed to the child via the `mute` op.
    muted: AtomicBool,
    /// The shared status-push gate, installed once at boot via [`set_status_gate`]. A
    /// mute toggle bumps it so a blocked `WaitModelStatus` wakes immediately (the muted
    /// flag is part of `model_status`). `OnceLock`-empty in tests / before wiring, where
    /// `set_muted` simply skips the bump.
    gate: OnceLock<Arc<StatusGate>>,
    /// When [`restart_if_crashed`](Self::restart_if_crashed) last ATTEMPTED a heal — its
    /// [`HEAL_COOLDOWN`] throttle, so a deterministic crasher can't turn every speak/tap
    /// into a blocking spawn+load.
    last_heal: Mutex<Option<std::time::Instant>>,
}

/// Minimum spacing between crash-heal attempts — see
/// [`TtsManager::restart_if_crashed`]. Long enough that a helper crashing on EVERY start
/// costs one blocking retry per window (speaks in between drop fast, as before the heal
/// existed); short enough that a transient kill (AV scan, OOM pressure) recovers promptly.
const HEAL_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

impl TtsManager {
    pub fn new(
        bin: PathBuf,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
    ) -> Self {
        Self {
            bin,
            lifecycle: Mutex::new(()),
            last_restart: Mutex::new(None),
            child: Arc::new(ChildSlot::new()),
            stdin: Mutex::new(None),
            reader: Mutex::new(None),
            speak_slot: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
            listen_slot: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
            diarize_slot: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
            enroll_slot: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            say_child: Mutex::new(None),
            last_error: Mutex::new(None),
            stats,
            stt_stats,
            lifetime,
            provider: Mutex::new("CPU".to_string()),
            stt_realized: Arc::new(Mutex::new("CPU".to_string())),
            spawn_prefs: Mutex::new(SpawnPrefs {
                provider: "auto".to_string(),
                stt_provider: "ane".to_string(),
                full_duplex: false,
                stt_preload: false,
            }),
            full_duplex_active: Mutex::new(false),
            stt_provider_active: Mutex::new(String::new()),
            tts_model: Arc::new(ModelSlot::new()),
            stt_model: Arc::new(ModelSlot::new()),
            muted: AtomicBool::new(false),
            gate: OnceLock::new(),
            last_heal: Mutex::new(None),
        }
    }

    /// Install the shared status-push gate (called once at boot). Lets [`set_muted`]
    /// bump it so a mute change pushes to a blocked `WaitModelStatus` immediately.
    pub fn set_status_gate(&self, gate: Arc<StatusGate>) {
        let _ = self.gate.set(gate);
    }

    /// Whether the running warm child is in full-duplex AEC mode. Callers use it to
    /// bypass the half-duplex `is_mic_active()` gates — under VPIO the input device is
    /// always live, so `is_mic_active()` is permanently true and useless as a gate.
    pub fn is_full_duplex_active(&self) -> bool {
        *self.full_duplex_active.lock().unwrap()
    }

    /// Is the Kokoro (TTS) model currently resident in the warm helper?
    pub fn is_tts_loaded(&self) -> bool {
        self.tts_model.is_loaded()
    }
    /// Is the Parakeet (STT) model currently resident in the warm helper?
    pub fn is_stt_loaded(&self) -> bool {
        self.stt_model.is_loaded()
    }

    /// The last STT (re)load failure (the helper's `STTLOADERR`), if any — surfaced in
    /// `model_status`'s `parakeet` row alongside `last_error`/the download error.
    pub fn stt_load_error(&self) -> Option<String> {
        self.stt_model.error()
    }
    /// The last TTS (re)load failure (the helper's `TTSLOADERR`) — the `tts_load_error`
    /// counterpart of [`stt_load_error`](Self::stt_load_error).
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
    fn clear_tts_load_error(&self) {
        self.tts_model
            .clear_error(self.gate.get().map(|g| g.as_ref()));
    }
    /// The warm child's active ONNX execution provider ("CPU" until a child reports
    /// otherwise via its PROVIDER line).
    pub fn provider(&self) -> String {
        self.provider.lock().unwrap().clone()
    }

    /// The warm child's REALIZED STT execution provider ("CPU"/"CUDA"/"CoreML-ANE"/"System"), from
    /// its `STT_PROVIDER` line — what the STT sessions ACTUALLY loaded on, the STT counterpart to
    /// [`provider`](Self::provider). "CPU" until a child reports otherwise.
    pub fn stt_realized_provider(&self) -> String {
        self.stt_realized.lock().unwrap().clone()
    }

    /// Switch the execution-provider preference ("auto"|"cpu"|"cuda"|"coreml"|"ane"). Restarts
    /// the warm child ONLY when the RESOLVED provider differs from the active one
    /// (so picking "auto" while already on CPU is a no-op). Returns true if it
    /// actually restarted — the caller then resets the TTS stats.
    pub fn set_provider(&self, which: &str) -> bool {
        self.spawn_prefs.lock().unwrap().provider = which.to_string();
        let resolved = Self::resolve_provider(which);
        if !self.is_running() {
            return false; // takes effect on next start; nothing active to change
        }
        if resolved == ds_config::RealizedProvider::parse(&self.provider()) {
            return false; // already running on this provider
        }
        self.restart_child();
        true
    }

    /// Restart the warm child AND reset BOTH engines' stats. The single restart point:
    /// the child hosts Kokoro (TTS) and Parakeet (STT) together, so any restart tears
    /// down both and begins one fresh measurement window for both — even a change that
    /// touched only one engine.
    fn restart_child(&self) {
        // Debounce: back-to-back bounces (config churn landing faster than the child
        // can spawn/settle) are a suspected contributor to the "warm child exited
        // unexpectedly" self-heal firing — and, in turn, to a queued speak dropping
        // because the guard's readiness check raced the child's death. When two
        // restarts land inside MIN_RESTART_GAP, sleep out the remainder before
        // actually bouncing the child, so the previous one gets a chance to fully
        // settle first. The sleep happens with `last_restart` UNLOCKED (never hold a
        // guard across a blocking wait) — a concurrent restart_child racing in during
        // the sleep just computes its own elapsed-since-last and debounces too, it
        // doesn't skip its restart.
        const MIN_RESTART_GAP: std::time::Duration = std::time::Duration::from_secs(1);
        let now = std::time::Instant::now();
        let prev = self.last_restart.lock().unwrap().replace(now);
        if let Some(prev) = prev {
            let elapsed = now.duration_since(prev);
            if elapsed < MIN_RESTART_GAP {
                log(&format!(
                    "WARN: TTS warm child restart {}ms after the previous one — rapid config churn?",
                    elapsed.as_millis()
                ));
                let wait = MIN_RESTART_GAP - elapsed;
                log(&format!(
                    "TTS warm child restart debounced — waiting {}ms for the previous child to settle",
                    wait.as_millis()
                ));
                std::thread::sleep(wait);
                // Re-anchor to the moment we actually proceed (not the original call
                // time) so a THIRD rapid call debounces off the real spacing instead
                // of compounding against an already-stale timestamp.
                *self.last_restart.lock().unwrap() = Some(std::time::Instant::now());
            }
        }
        self.stop_child();
        self.ensure_started();
        self.stats.reset();
        self.stt_stats.reset();
    }

    /// Restart the warm child to pick up models that finished downloading AFTER it started —
    /// the self-heal a background fetch calls on success (see
    /// [`crate::downloads::start_download`]). Distinct from [`set_provider`](Self::set_provider)
    /// and [`restart_if_full_duplex_stale`](Self::restart_if_full_duplex_stale), which restart
    /// only on a provider/mode CHANGE: here the provider is UNCHANGED but the model files just
    /// appeared (a provider switch or fresh install started the child before they existed), so
    /// we restart unconditionally. If the child is running we restart it to pick up the files;
    /// if it is NOT running we START it — it EXITED when it tried to load before the model
    /// existed (fresh install / provider switch), and nothing else re-spawns it after a fatal
    /// "not downloaded", so without starting it here the engine would sit on its stale failure
    /// until a manual restart. "Not running" now also covers `start_locked`'s cheap presence
    /// gate having skipped the spawn entirely (not just "toggled off") — either way the fix is
    /// the same: try again now that the model is actually on disk. The sole caller (the
    /// download-completion hook) only reaches here for a target whose engine is wanted, so
    /// starting is correct. Reuses the shared start path.
    /// Returns whether a warm child is running afterwards.
    pub(crate) fn reload_models(&self) -> bool {
        if !self.is_running() {
            self.ensure_started(); // load the now-present model (child had exited before it existed)
            return self.is_running();
        }
        self.restart_child();
        true
    }

    /// Self-heal for a warm child that DIED post-READY (crash: AV false-positive on freshly
    /// written dylibs, OOM, GPU driver) — the queue worker calls this before dropping a
    /// not-playable Kokoro item. It completes `mark_dead`'s contract ("the next speak
    /// restarts it"): without it the worker's not-ready guard dropped that very speak, so a
    /// crashed child wedged BOTH models in "Starting" until an app restart. The decision is
    /// [`crate::config_gate::warm_child_heal_action`]: a dead child (still in the slot, or
    /// already reaped by an io error) restarts; a child that's alive (still loading) or
    /// whose last START failed (the download-completion hook owns that retry) is left alone.
    pub(crate) fn restart_if_crashed(&self) {
        use crate::config_gate::HealAction;
        // Observe AND act under ONE `lifecycle` acquisition: a snapshot taken outside it
        // could go stale across a concurrent `reload_models` restart (a full stop+start —
        // seconds), and the reap below would then kill the healthy replacement child.
        let _lifecycle = self.lifecycle.lock().unwrap();
        let (present, exited) = self.child.probe();
        let error = self.last_error().is_some();
        let action = crate::config_gate::warm_child_heal_action(present, exited, error);
        if action == HealAction::Nothing {
            return;
        }
        // Throttle: at most one attempt per window. A DETERMINISTIC crasher (corrupt model,
        // an AV that kills the helper on every start) would otherwise turn every speak/tap
        // into a blocking multi-second spawn+load; the incident class this heals (a one-shot
        // AV kill on freshly written dylibs) recovers on the first attempt anyway.
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
                log("TTS warm child found dead — reaping and restarting it");
                self.mark_dead_locked();
                self.start_locked();
            }
            HealAction::Start => {
                log("TTS warm child is gone — restarting it for the queued speak");
                self.start_locked();
            }
        }
    }

    /// Set whether the warm child should run in full-duplex AEC mode (the engine
    /// passes `full_duplex && Parakeet STT`, see `full_duplex_wanted`). Stores the preference only; the
    /// next (re)start uses it. Pair with [`restart_if_full_duplex_stale`](Self::restart_if_full_duplex_stale)
    /// to apply a change to an already-running child.
    pub fn set_full_duplex_pref(&self, on: bool) {
        self.spawn_prefs.lock().unwrap().full_duplex = on;
    }

    /// Set which local STT backend the warm child should use — the resolved provider token
    /// ("cpu"|"cuda"|"ane"|"system").
    /// Stores the preference only; [`restart_if_full_duplex_stale`](Self::restart_if_full_duplex_stale)
    /// applies a change to an already-running child.
    pub fn set_stt_provider_pref(&self, engine: &str) {
        self.spawn_prefs.lock().unwrap().stt_provider = engine.to_string();
    }

    /// Set whether STT should be preloaded in the warm child (= `helper_uses_stt(cfg)`).
    /// Applied on the next (re)start via the `DONTSPEAK_STT_PRELOAD` env.
    pub fn set_stt_wanted(&self, wanted: bool) {
        self.spawn_prefs.lock().unwrap().stt_preload = wanted;
    }

    /// Restart the warm child iff it is running with a mode that no longer matches the
    /// preference — either the full-duplex flag (toggled, or STT moved to/from a local
    /// engine) or the local STT engine itself (cpu ↔ ane, so the child picks
    /// up the new `DONTSPEAK_STT_PROVIDER`). No-op when stopped or already matching — safe
    /// to call on every config reload.
    pub fn restart_if_full_duplex_stale(&self) {
        if !self.is_running() {
            return; // takes effect on next start
        }
        // Copy the prefs out and drop `spawn_prefs` before touching the other two locks
        // (never hold a guard across another lock acquisition — same discipline as the
        // `start_locked` copy-out above).
        let prefs = self.spawn_prefs.lock().unwrap().clone();
        let fd_stale = prefs.full_duplex != *self.full_duplex_active.lock().unwrap();
        let stt_stale = prefs.stt_provider != *self.stt_provider_active.lock().unwrap();
        if !fd_stale && !stt_stale {
            return;
        }
        self.restart_child();
    }

    /// The provider a preference RESOLVES to right now — what the warm child will
    /// actually report. "cuda"/"auto" only become CUDA once the GPU runtime is
    /// present (else the helper falls back to CPU), so resolving against presence
    /// keeps `set_provider` from restart-looping while the runtime downloads.
    fn resolve_provider(which: &str) -> ds_config::RealizedProvider {
        use ds_config::RealizedProvider;
        if which.eq_ignore_ascii_case("coreml") {
            return RealizedProvider::CoreMl;
        }
        // `ane` AND `auto` resolve to the FluidAudio Core ML / ANE backend on macOS (the
        // shared ladder's top rung) — but only when its shim dylib is actually present (set
        // by the app); otherwise the helper falls back to the ONNX CPU path, so resolve to
        // CPU to match what the child will report and avoid a needless restart.
        #[cfg(target_os = "macos")]
        if which.eq_ignore_ascii_case("ane") || which.eq_ignore_ascii_case("auto") {
            let have_dylib = std::env::var_os("SMKOKORO_DYLIB_PATH")
                .map(|p| std::path::Path::new(&p).exists())
                .unwrap_or(false);
            return if have_dylib {
                RealizedProvider::CoreMlAne
            } else {
                RealizedProvider::Cpu
            };
        }
        #[cfg(all(
            any(target_os = "windows", target_os = "linux"),
            target_arch = "x86_64"
        ))]
        {
            if ds_config::provider_pref_wants_gpu(which) && ds_model::is_cuda_runtime_present() {
                return RealizedProvider::Cuda;
            }
        }
        RealizedProvider::Cpu
    }

    /// True when a warm child is running.
    pub fn is_running(&self) -> bool {
        self.child.is_running()
    }

    /// The last warm-child start failure, if the most recent start attempt failed
    /// and TTS is still on (cleared on a successful start or when toggled off).
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    /// Set the last-start-failure message and, on a REAL change (a fresh error, or a
    /// different one), bump the status-push gate — `last_error` is surfaced per-engine
    /// in `model_status`, so without this a blocked `WaitModelStatus` never learns a
    /// start failed until an unrelated event happens to bump the gate (same bug class
    /// as the caps status dot going stale across an Accessibility grant; see
    /// [`crate::engine::Engine::set_caps_gate`]).
    fn set_error(&self, msg: impl Into<String>) {
        let msg = msg.into();
        let mut guard = self.last_error.lock().unwrap();
        if guard.as_deref() != Some(msg.as_str()) {
            *guard = Some(msg);
            drop(guard);
            if let Some(gate) = self.gate.get() {
                gate.bump();
            }
        }
    }
    /// Clear the last-start-failure message and, on a REAL change (an error WAS set),
    /// bump the status-push gate so a resolved error reflects live — the mirror of
    /// [`set_error`](Self::set_error).
    fn clear_error(&self) {
        let mut guard = self.last_error.lock().unwrap();
        if guard.take().is_some() {
            drop(guard);
            if let Some(gate) = self.gate.get() {
                gate.bump();
            }
        }
    }

    /// Apply the `tts_enabled` toggle: start the warm child (on) or kill it (off).
    /// Idempotent — re-applying the same state is a no-op.
    pub fn set_enabled(&self, on: bool) {
        if on {
            self.ensure_started();
        } else {
            self.stop_child();
        }
    }

    /// Start the warm child if it isn't already running. Used by voice preview so
    /// auditioning works even when TTS replies are toggled off (the Settings
    /// window is actively driving it). No-op when already running.
    pub fn ensure_started(&self) {
        if !self.is_running() {
            self.start();
        }
    }

    /// Spawn `ds-helper --serve` and wait for its `READY` line (model warm).
    /// On any failure the manager stays "not running" and the hooks fall back to
    /// the cold one-shot path.
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

        // CHEAP, is_file()-only Kokoro presence gate, resolved per-backend from `prefs` (NOT a
        // fresh VoiceConfig re-read — start_locked has no cfg; boot/reload keep `spawn_prefs`
        // current, see the provider-freshness fix in boot.rs/engine.rs). Skips the spawn on a
        // fresh install / provider switch instead of paying the guaranteed-fail transient
        // ("kokoro model not downloaded" / "smk_init failed") — `reload_models` (unchanged)
        // performs the sole, successful start once the background fetch lands.
        //
        // Kokoro is gated UNCONDITIONALLY (not role-gated on whether TTS is even selected):
        // `ds-helper --serve` calls `load_backend()` (Kokoro) unconditionally whenever it runs at
        // all (oneshot::load_backend — no DONTSPEAK_TTS_PRELOAD-style role gate exists there, a
        // separate pre-existing gap, not fixed here), and a missing/mismatched Kokoro asset set
        // is FATAL to the WHOLE child (serve.rs's `_exit(1)`), mirroring `status.rs`'s own
        // `kokoro_present` computation, which is ALSO computed unconditionally. Parakeet is
        // deliberately NOT gated here: its preload is genuinely conditional
        // (DONTSPEAK_STT_PRELOAD) and a failed preload is non-fatal (STTLOADERR, no `_exit`) —
        // the child boots fine either way, and the existing per-model
        // `stt_load_error`/ModelSlot machinery already reports it correctly. Gating on Parakeet
        // too would incorrectly block a healthy Kokoro from starting whenever only Parakeet's
        // download is still pending.
        let kokoro_apple_native =
            Self::resolve_provider(&prefs.provider) == ds_config::RealizedProvider::CoreMlAne;
        let kokoro_ready = if kokoro_apple_native {
            ds_model::coreml_repo::is_coreml_set_present(&ds_model::coreml_repo::KOKORO_COREML_SET)
        } else {
            crate::config_gate::kokoro_onnx_files_present()
        };
        if !kokoro_ready {
            // Mirrors every OTHER early-return below (spawn error, missing stdio, ERR line):
            // set_error() so `warm_child_heal_action` sees error=true and resolves to
            // `HealAction::Nothing` — a Caps-Lock-triggered `restart_if_crashed` must NOT retry
            // this doomed spawn on every tap; only the download-completion hook retries it, once,
            // when the fetch actually lands. Safe for the status UI: `combined_error` only
            // surfaces a model's `last_error` while that SAME model reads `present` — false here
            // by construction — so the Kokoro row shows "Missing" (offer Download), never a
            // stale "Failed".
            self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
            log(&format!(
                "TTS/STT warm child start skipped — Kokoro model not yet present on disk \
                 (provider={}); the background download will restart it automatically once it \
                 finishes",
                prefs.provider
            ));
            return;
        }

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Helper stderr → a log file (full-duplex status, capture levels,
            // barge-debug, errors) so the warm child is diagnosable; was discarded.
            .stderr(helper_stderr());
        // The daemon→helper env contract, resolved from the spawn prefs:
        //   • DONTSPEAK_PROVIDER      — Kokoro TTS execution provider ("cpu"|"cuda"|…).
        //   • DONTSPEAK_STT_PROVIDER  — local STT backend the child serves ("cpu"|"ane"|…).
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
                log(&format!(
                    "WARN: TTS warm child spawn failed ({}): {e}",
                    self.bin.display()
                ));
                return;
            }
        };
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(BufReader::new);
        let (Some(stdin), Some(mut stdout)) = (stdin, stdout) else {
            let _ = child.kill();
            let _ = child.wait();
            self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
            log("WARN: TTS warm child missing stdio pipes");
            return;
        };

        // Wait for READY (model loaded) or ERR (fatal). Bounded by the child
        // closing stdout on failure; load takes a few seconds the first time.
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line) {
                Ok(0) => {
                    let _ = child.wait();
                    self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
                    log("WARN: TTS warm child closed before READY");
                    return;
                }
                Ok(_) => {
                    let l = line.trim();
                    if l == proto::READY {
                        break;
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
                        *self.stt_realized.lock().unwrap() = p.trim().to_string();
                        continue;
                    }
                    if let Some(p) = l.strip_prefix(proto::PROVIDER_PREFIX) {
                        *self.provider.lock().unwrap() = p.trim().to_string();
                        continue;
                    }
                    if let Some(msg) = l.strip_prefix(proto::ERR) {
                        let _ = child.kill();
                        let _ = child.wait();
                        self.set_error(msg.trim());
                        log(&format!("WARN: TTS warm child failed to load:{msg}"));
                        return;
                    }
                    // ignore any other chatter before READY
                }
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.set_error(ds_i18n::t("status.engine.reason.tts_failed"));
                    log(&format!(
                        "WARN: TTS warm child read error before READY: {e}"
                    ));
                    return;
                }
            }
        }

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
            // The status push-gate, so a post-READY STTLOADED pushes LIVE instead of waiting
            // for the next poll.
            let gate = self.gate.get().cloned();
            std::thread::spawn(move || {
                reader_loop(
                    stdout,
                    ReaderSlots {
                        speak: speak_slot,
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
        // Kokoro is eager-loaded by the helper before READY. STT (Parakeet) now preloads in
        // PARALLEL and reports its own STTLOADED (possibly BEFORE this READY), so we must NOT
        // reset stt_model here — it's initialized before the wait loop and set by the STT
        // signal handlers.
        self.tts_model
            .transition(ModelState::Loaded, self.gate.get().map(|g| g.as_ref()));
        // Re-apply the CURRENT global-mute state to this freshly (re)spawned child. Every
        // start (provider switch, post-download restart, crash-heal via `restart_if_crashed`)
        // installs a brand-new child that inits UNMUTED — without this push, speech would
        // play audibly at full volume right after the switch while the UI still shows
        // "muted" (mirrors the `mute` op `set_muted` sends to an already-running child on
        // a live toggle).
        let _ = self.write_request(if self.is_muted() {
            r#"{"op":"mute","text":"on"}"#
        } else {
            r#"{"op":"mute","text":"off"}"#
        });
        log("TTS warm Kokoro child READY");
    }

    /// Reset BOTH models to `Idle` (the process is gone → both models go with it). Each
    /// [`ModelSlot::transition`] call is already change-gated (a no-op, no bump, if that
    /// model was already `Idle`), so a blocked `WaitModelStatus` still sees a real
    /// transition immediately instead of at some unrelated later status change (the
    /// caps-dot bug class; see `set_caps_gate` in engine.rs) — without the old manual
    /// "only bump if at least one was loaded" bookkeeping this used to need. Shared by
    /// `stop_child` and `mark_dead_locked`.
    fn clear_loaded_flags(&self) {
        let gate = self.gate.get().map(|g| g.as_ref());
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
        // The realized STT provider goes with the dead child. Reset it so a restart whose STT
        // preload FAILS (emits no `STT_PROVIDER`) can't leave a stale token — e.g. the old child's
        // "CUDA" — to be read before the new child reports. (The status row is gated on
        // `stt_loaded` too, but keep the slot strictly fresh.)
        *self.stt_realized.lock().unwrap() = "CPU".to_string();
        // `reap` has already released the slot's lock when it returns, so the
        // kill/wait below run OUTSIDE any lock.
        if let Some(mut child) = self.child.reap() {
            let _ = child.kill();
            let _ = child.wait();
            log("TTS warm Kokoro child stopped (model freed)");
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
            log(&format!(
                "WARN: TTS warm child (pid {pid}) reaped by mark_dead: {}",
                describe_exit(status)
            ));
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
            log(
                "TTS: stale child-death signal from a superseded child ignored \
                 (already restarted)",
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
    /// OTHER engine keeps it warm — universal: `"tts"` → Kokoro, `"stt"` → Parakeet.
    /// The helper lazily reloads on next use. Fire-and-forget; no-op when the helper
    /// isn't running (nothing to free) or the engine is unknown.
    pub fn unload_engine(&self, engine: &str) {
        if engine != "tts" && engine != "stt" {
            return;
        }
        if self
            .write_request(&format!(r#"{{"op":"unload","engine":"{engine}"}}"#))
            .is_ok()
        {
            // `ModelSlot::transition` is itself change-gated (mirrors `mark_loaded`'s push on
            // the "true" direction) — otherwise an ordinary TTS/STT engine switch would leave
            // a blocked `WaitModelStatus` showing a stale "Running" dot for up to the poll
            // window, while `reconcile_helper_models`'s UNCONDITIONAL ~20s-tick call for any
            // engine that isn't currently wanted would wake every connected client every tick
            // forever even when nothing changed — reintroducing the poll-churn regression this
            // whole gating scheme exists to fix. Transitioning straight to `Idle` also clears
            // any stale "failed to load" state — a deliberately unloaded model has none anymore.
            let gate = self.gate.get().map(|g| g.as_ref());
            match engine {
                "tts" => self.tts_model.transition(ModelState::Idle, gate),
                "stt" => self.stt_model.transition(ModelState::Idle, gate),
                _ => {}
            }
            log(&format!("helper: requested unload of {engine} model"));
        }
    }

    /// Tell the warm helper to eagerly (pre)load a model so it's resident the moment
    /// its engine is selected — the symmetric counterpart to [`unload_engine`], so
    /// "loaded" reflects residency before first use (Parakeet is otherwise lazy).
    /// Fire-and-forget; no-op when the helper isn't running or the engine is unknown.
    pub fn load_engine(&self, engine: &str) {
        if engine != "tts" && engine != "stt" {
            return;
        }
        if self
            .write_request(&format!(r#"{{"op":"load","engine":"{engine}"}}"#))
            .is_ok()
        {
            // Neither engine lights optimistically: TTS waits for the helper's `TTSLOADED`
            // confirmation (after `load_backend`) exactly as STT waits for `STTLOADED` (after
            // preload + graph warmup), so the dot stays "warming" until the model is truly
            // resident — never greening on the mere `load` request.
            log(&format!("helper: requested preload of {engine} model"));
        }
    }

    /// Write one JSON request line to the child's stdin. Err if not running.
    fn write_request(&self, json: &str) -> std::io::Result<()> {
        let mut guard = self.stdin.lock().unwrap();
        let stdin = guard
            .as_mut()
            .ok_or_else(|| std::io::Error::other("TTS child not running"))?;
        stdin.write_all(json.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    /// Speak `text` through the warm child and block until it finishes (or is
    /// cancelled — the child reports `DONE` for both). Err ⇒ the engine could not
    /// speak (no child / IO error), so the caller falls back to the cold path.
    pub fn speak(&self, text: &str, voice: &str, rate: f32) -> std::io::Result<()> {
        self.play("speak", text, voice, rate)
    }

    /// Speak `text` via the macOS System engine (`say`) and block until it
    /// finishes (or is killed by `stop`). System TTS keeps no warm model — it
    /// spawns per request. The OS voice (System Settings) is used; `rate` maps to
    /// `say -r <words/min>`. Barge-in kills the tracked child.
    #[cfg(target_os = "macos")]
    pub fn speak_system(&self, text: &str, voice: &str, rate: f32) -> std::io::Result<()> {
        // Single speaker: stop any Kokoro playback and any prior `say` first.
        self.stop();
        // Shared say-command builder (canonical flags + wpm mapping). A non-empty
        // voice selects a specific `say` voice (the FULL displayed name, incl. any
        // quality suffix); empty means the OS default voice. We do NOT use the
        // pidfile here — this path owns the child via `say_child` directly.
        let voice = (!voice.trim().is_empty()).then_some(voice);
        let mut cmd = ds_tts::system::say_command(voice, rate);
        let child = cmd.arg(text).spawn()?;
        // Hand the child to the shared slot so stop() can kill it, then poll for
        // completion holding the lock only briefly (so a concurrent stop can win).
        *self.say_child.lock().unwrap() = Some(child);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let mut g = self.say_child.lock().unwrap();
            match g.as_mut() {
                Some(c) => match c.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        *g = None;
                        break;
                    }
                    Ok(None) => {}
                },
                None => break, // stop() killed/took it (barge-in)
            }
        }
        Ok(())
    }

    /// Windows: speak via the OS synthesizer (PowerShell `System.Speech.Synthesis`),
    /// the same builder the library `SystemTts` uses. Mirrors the macOS path: single
    /// speaker (stop any in-flight speech first), own the spawned child through the
    /// `say_child` slot so a barge-in/stop can kill it, then poll for completion.
    /// A non-empty `voice` selects a specific installed voice (full display name);
    /// empty = the OS default voice.
    #[cfg(target_os = "windows")]
    pub fn speak_system(&self, text: &str, voice: &str, rate: f32) -> std::io::Result<()> {
        self.stop();
        let voice = (!voice.trim().is_empty()).then_some(voice);
        let mut cmd = ds_tts::system::say_command(voice, rate, text);
        let child = cmd.spawn()?;
        // Hand the child to the shared slot so stop() can kill it, then poll for
        // completion holding the lock only briefly (so a concurrent stop can win).
        *self.say_child.lock().unwrap() = Some(child);
        loop {
            std::thread::sleep(std::time::Duration::from_millis(40));
            let mut g = self.say_child.lock().unwrap();
            match g.as_mut() {
                Some(c) => match c.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        *g = None;
                        break;
                    }
                    Ok(None) => {}
                },
                None => break, // stop() killed/took it (barge-in)
            }
        }
        Ok(())
    }

    /// Other platforms (Linux): the System path isn't wired up yet. Returns Unsupported
    /// so callers fall back / record last_error. TODO (Linux): route through
    /// ds_tts::SystemTts (spd-say/espeak), and/or have the engine selector fall back
    /// to Kokoro when SystemTts::available() is false, so this never reaches the user.
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn speak_system(&self, _text: &str, _voice: &str, _rate: f32) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "dontspeakd System (say) TTS is not yet wired up on this platform",
        ))
    }

    fn play(&self, op: &str, text: &str, voice: &str, rate: f32) -> std::io::Result<()> {
        // Snapshot the child's generation for THIS request (one acquisition also serves
        // as the is-running gate) — if the reader only wakes us (fatal) after a
        // concurrent restart has ALREADY installed a new child, this lets us tell "our
        // child died" apart from "a stale EOF from an old, superseded child".
        let Some(my_gen) = self.child.running_gen() else {
            return Err(std::io::Error::other("TTS child not running"));
        };
        // Fresh request: reset the speak slot so it reflects THIS speak only.
        {
            let (m, _cv) = &*self.speak_slot;
            *m.lock().unwrap() = SpeakSlot::default();
        }

        let req = serde_json::json!({"op": op, "voice": voice, "rate": rate, "text": text});
        if let Err(e) = self.write_request(&req.to_string()) {
            self.mark_dead();
            self.stats.record_failure();
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
        while !s.done {
            s = cv.wait(s).unwrap();
        }
        let err = s.err.take();
        let fatal = s.fatal;
        drop(s);
        if let Some(e) = err {
            // EOF/read-error ⇒ the child died: reap it so the next speak restarts — but
            // only if it's STILL the child we sent this request to (see
            // `mark_dead_if_current`). A soft `ERR` line (child alive) just fails this
            // one utterance either way.
            if fatal {
                self.mark_dead_if_current(my_gen);
                self.stats.record_failure();
            }
            return Err(std::io::Error::other(e));
        }
        Ok(())
    }

    /// Run an STT (listen) session on the warm helper: stream `PARTIAL` text to
    /// `on_partial`, return the FINAL transcript. The helper opens the mic and
    /// re-transcribes periodically; end it with `stop()` (from a second caller).
    /// Starts the helper if it isn't running. Holds the stdout reader for the
    /// session (speak/listen are mutually exclusive). Err ⇒ the helper is gone.
    pub fn listen(&self, on_partial: &mut dyn FnMut(&str)) -> std::io::Result<String> {
        self.ensure_started();
        if !self.is_running() {
            return Err(std::io::Error::other("STT helper not running"));
        }
        // Fresh session: drop any stale events / dead flag from a prior listen.
        {
            let (m, _cv) = &*self.listen_slot;
            let mut s = m.lock().unwrap();
            s.events.clear();
            s.dead = false;
        }
        if let Err(e) = self.write_request(r#"{"op":"listen"}"#) {
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
            let evt = {
                let mut s = m.lock().unwrap();
                loop {
                    if let Some(e) = s.events.pop_front() {
                        break Some(e);
                    }
                    if s.dead {
                        break None;
                    }
                    s = cv.wait(s).unwrap();
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

    /// Barge-in: cancel any in-flight playback. Fire-and-forget (no stdout read),
    /// so it can run while a `speak` is blocked awaiting its `DONE`. Stops BOTH the
    /// Kokoro warm child's playback and any in-flight System `say`. Only the macOS/Windows
    /// `speak_system` path calls this (Linux has no System engine), so it's gated to those
    /// targets to stay dead-code-clean.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn stop(&self) {
        let _ = self.write_request(r#"{"op":"stop"}"#);
        if let Some(mut c) = self.say_child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Whether global mute is on.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    /// Set global mute. Records it AND pushes the `mute` op to the warm child so the change
    /// is live (the child silences playback without stopping — the queue keeps draining).
    /// Idempotent.
    pub fn set_muted(&self, on: bool) {
        let changed = self.muted.swap(on, Ordering::Relaxed) != on;
        let _ = self.write_request(if on {
            r#"{"op":"mute","text":"on"}"#
        } else {
            r#"{"op":"mute","text":"off"}"#
        });
        // Push the mute transition to a blocked `WaitModelStatus` (the flag is part of
        // `model_status`). Only on a real change so an idempotent re-set wakes no one.
        if changed && let Some(gate) = self.gate.get() {
            gate.bump();
        }
    }

    /// Like [`stop`](Self::stop) but asks the warm helper to FADE the rodio player
    /// out over a short window before stopping, so a user-facing barge (clear-on-submit,
    /// window close, newest-reply preempt, the caps long-press reset, and the mic
    /// record-barge) tapers off instead of clicking. The system `say` path can't fade,
    /// so it's killed outright exactly as in `stop`.
    pub fn stop_fade(&self) {
        let _ = self.write_request(r#"{"op":"stopfade"}"#);
        if let Some(mut c) = self.say_child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    /// Play a one-shot EARCON (`path` = an absolute sound file) on the warm child's audio
    /// output — fire-and-forget, OUTSIDE the TTS queue, so a turn-end ding is mixed over any
    /// in-flight speech rather than queued behind it. No-op when the child isn't running.
    /// The engine has already gated on `earcon_enabled` + mute and resolved the path.
    pub fn cue(&self, path: &str) {
        let _ = self.write_request(&serde_json::json!({ "op": "cue", "text": path }).to_string());
    }

    /// End an in-flight `listen` WITHOUT cancelling a concurrent `speak` (the
    /// `lstop` op). In full-duplex coexist a dictation and a reply run at once, so
    /// the STT path must end its listen alone; in half-duplex `lstop` ends the
    /// serve-loop listen just like `stop`. Fire-and-forget over stdin.
    pub fn stop_listen(&self) {
        let _ = self.write_request(r#"{"op":"lstop"}"#);
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
        if let Err(e) = self.write_request(&format!(r#"{{"op":"diarize","seconds":{seconds}}}"#)) {
            self.mark_dead();
            return Err(e);
        }
        let (m, cv) = &*self.diarize_slot;
        let mut s = m.lock().unwrap();
        loop {
            if s.done || s.dead {
                break;
            }
            s = cv.wait(s).unwrap();
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
        if let Err(e) = self.write_request(&format!(r#"{{"op":"enroll","seconds":{seconds}}}"#)) {
            self.mark_dead();
            return Err(e);
        }
        let (m, cv) = &*self.enroll_slot;
        let mut s = m.lock().unwrap();
        loop {
            if s.done || s.dead {
                break;
            }
            s = cv.wait(s).unwrap();
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

#[cfg(test)]
mod coexist_it {
    use super::*;

    /// Live coexist smoke test for the stdout DEMUX: speak WHILE a listen runs and
    /// assert both terminate cleanly (the speak gets its `DONE`, the listen its
    /// `LDONE`) without serializing. Needs the built `ds-helper`, the
    /// Kokoro+Parakeet models, and mic permission for the test runner — so it is
    /// `#[ignore]`d. Run it explicitly (it plays audio):
    ///   cargo test -p dontspeakd coexist_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn coexist_smoke() {
        let bin =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/ds-helper");
        let mgr = Arc::new(TtsManager::new(
            bin,
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                std::env::temp_dir().join("ds-stats-coexist-test.json"),
            )),
        ));
        mgr.set_full_duplex_pref(true);
        mgr.ensure_started();
        assert!(
            mgr.is_running(),
            "helper failed to start: {:?}",
            mgr.last_error()
        );

        // A listen on a background thread drains the listen slot while we speak.
        let lmgr = mgr.clone();
        let listen = std::thread::spawn(move || lmgr.listen(&mut |p| eprintln!("[partial] {p}")));
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Speak WHILE the listen runs — the whole point of coexist. If the demux is
        // broken (one stdout reader), this blocks forever or steals the listen's lines.
        let t0 = std::time::Instant::now();
        let r = mgr.speak(
            "Testing coexistence. I am speaking while you dictate. This is the end.",
            "af_sarah",
            1.0,
        );
        eprintln!("[speak] returned {r:?} after {:?}", t0.elapsed());
        assert!(r.is_ok(), "speak failed: {r:?}");

        // End the listen and collect the final transcript.
        std::thread::sleep(std::time::Duration::from_millis(500));
        mgr.stop_listen();
        let final_text = listen.join().expect("listen thread panicked");
        eprintln!("[final] {final_text:?}");
        assert!(final_text.is_ok(), "listen failed: {final_text:?}");

        mgr.set_enabled(false);
    }
}

#[cfg(test)]
mod child_env_tests {
    use super::{SpawnPrefs, child_env};

    #[test]
    fn providers_always_set_conditionals_cleared_when_off() {
        // Both providers are ALWAYS `Some` (always overwrite any ambient value); the two
        // conditional flags are `Some("1")` when on and `None` when off — and `None` drives
        // `env_remove` in `start`, so an inherited `DONTSPEAK_FULL_DUPLEX=1` / `_STT_PRELOAD=1`
        // can't leak past the config-resolved intent.
        let on = child_env(&SpawnPrefs {
            provider: "cuda".into(),
            stt_provider: "ane".into(),
            full_duplex: true,
            stt_preload: true,
        });
        assert_eq!(on[0], ("DONTSPEAK_PROVIDER", Some("cuda".into())));
        assert_eq!(on[1], ("DONTSPEAK_STT_PROVIDER", Some("ane".into())));
        assert_eq!(on[2], ("DONTSPEAK_FULL_DUPLEX", Some("1".into())));
        assert_eq!(on[3], ("DONTSPEAK_STT_PRELOAD", Some("1".into())));

        let off = child_env(&SpawnPrefs {
            provider: "cpu".into(),
            stt_provider: "cpu".into(),
            full_duplex: false,
            stt_preload: false,
        });
        assert_eq!(off[0], ("DONTSPEAK_PROVIDER", Some("cpu".into())));
        assert_eq!(off[1], ("DONTSPEAK_STT_PROVIDER", Some("cpu".into())));
        assert_eq!(off[2], ("DONTSPEAK_FULL_DUPLEX", None));
        assert_eq!(off[3], ("DONTSPEAK_STT_PRELOAD", None));
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
        tts.unload_engine("tts");
        let seq1 = gate.seq();
        assert_ne!(seq1, 0, "a real loaded→unloaded transition bumps the gate");
        assert!(!tts.tts_model.is_loaded());

        // Repeat on an already-unloaded engine: no real transition, must NOT bump again.
        tts.unload_engine("tts");
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

    #[test]
    fn resolve_provider_is_deterministic_for_explicit_non_gpu_tokens() {
        // "cpu"/"coreml" never branch on host state (unlike "cuda"/"auto", which read
        // `ds_model::is_cuda_runtime_present()` against the real model_dir() on disk — those
        // two are deliberately NOT covered here, see the tts.rs coverage plan).
        assert_eq!(
            TtsManager::resolve_provider("cpu"),
            ds_config::RealizedProvider::Cpu
        );
        assert_eq!(
            TtsManager::resolve_provider("coreml"),
            ds_config::RealizedProvider::CoreMl
        );
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

        // set_provider's own persisted-value half of its contract (the early-return
        // is covered elsewhere; this confirms the write happens before that check).
        assert!(!tts.set_provider("cpu"));
        assert_eq!(tts.spawn_prefs.lock().unwrap().provider, "cpu");
    }

    /// Points `DONTSPEAK_MODEL_DIR` at a fresh EMPTY tempdir and clears
    /// `ORT_DYLIB_PATH`/`SMKOKORO_DYLIB_PATH` — forces the new Kokoro-presence gate's ONNX
    /// branch deterministically on every OS and guarantees "not present". Caller must hold
    /// `crate::config_gate::ENV_LOCK` and restore the returned previous values.
    fn clear_kokoro_env() -> (
        tempfile::TempDir,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
        Option<std::ffi::OsString>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let prev_model_dir = std::env::var_os("DONTSPEAK_MODEL_DIR");
        let prev_ort = std::env::var_os("ORT_DYLIB_PATH");
        let prev_smk = std::env::var_os("SMKOKORO_DYLIB_PATH");
        // SAFETY: test-only env mutation; the caller holds `config_gate::ENV_LOCK` (see
        // this fn's doc) and restores the returned previous values.
        unsafe {
            std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path());
            std::env::remove_var("ORT_DYLIB_PATH");
            std::env::remove_var("SMKOKORO_DYLIB_PATH");
        }
        (tmp, prev_model_dir, prev_ort, prev_smk)
    }

    fn restore_kokoro_env(
        prev_model_dir: Option<std::ffi::OsString>,
        prev_ort: Option<std::ffi::OsString>,
        prev_smk: Option<std::ffi::OsString>,
    ) {
        // SAFETY: restore the prior values (or clear them) so later tests see the real
        // env again; the caller still holds `config_gate::ENV_LOCK`.
        unsafe {
            match prev_model_dir {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
            match prev_ort {
                Some(v) => std::env::set_var("ORT_DYLIB_PATH", v),
                None => std::env::remove_var("ORT_DYLIB_PATH"),
            }
            match prev_smk {
                Some(v) => std::env::set_var("SMKOKORO_DYLIB_PATH", v),
                None => std::env::remove_var("SMKOKORO_DYLIB_PATH"),
            }
        }
    }

    #[test]
    fn start_locked_skips_the_spawn_when_kokoro_is_not_present() {
        // The new cheap presence gate: on a fresh install / provider switch, before the
        // Kokoro model has been downloaded, `start_locked` must skip the spawn entirely
        // rather than pay the guaranteed-fail "kokoro model not downloaded" transient.
        let _guard = crate::config_gate::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (_tmp, prev_model_dir, prev_ort, prev_smk) = clear_kokoro_env();

        let (tts, gate) = mk();
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

        restore_kokoro_env(prev_model_dir, prev_ort, prev_smk);
    }

    #[test]
    fn start_against_a_nonexistent_binary_sets_last_error_and_bumps_the_gate() {
        // Exercises start_locked's real spawn-failure branch (Command::spawn erroring on
        // a path that doesn't exist) — no mock, no real ds-helper binary needed. With the
        // presence gate now wired in, this must route through fixture files that read
        // "present" (else it would exercise the new skip path instead of ever reaching
        // Command::spawn, on any host whose real ambient model cache happens to be empty
        // OR already populated).
        let _guard = crate::config_gate::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (tmp, prev_model_dir, prev_ort, prev_smk) = clear_kokoro_env();
        std::fs::write(tmp.path().join(ds_model::KOKORO_ONNX_FILE), b"dummy").unwrap();
        std::fs::write(tmp.path().join(ds_model::KOKORO_VOICES_FILE), b"dummy").unwrap();
        let dylib = tmp.path().join("dummy-onnxruntime.dylib");
        std::fs::write(&dylib, b"dummy").unwrap();
        // SAFETY: test-only env mutation, serialized by ENV_LOCK (held above), restored
        // below via restore_kokoro_env.
        unsafe {
            std::env::set_var("ORT_DYLIB_PATH", &dylib);
        }

        let (tts, gate) = mk();
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

        restore_kokoro_env(prev_model_dir, prev_ort, prev_smk);
    }
}
