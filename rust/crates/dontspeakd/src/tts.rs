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
//! STATS/ERR/BARGE) and a [`ListenSlot`] (LISTENING/PARTIAL/FINAL/STTSTATS/
//! STTERR/LDONE). A `speak` waits on the speak slot while a `listen` drains the
//! listen slot AT THE SAME TIME — neither holds stdout, so they run concurrently
//! (dictate while the voice talks). `stop` only takes the brief `stdin` lock, so
//! barge-in still works while a speak is mid-flight.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::log;
use crate::model_slot::{ModelSlot, ModelState};
use crate::status::StatusGate;

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
/// library abort, an unhandled panic, or a startup failure before `ds_config::log_cached` can
/// even initialize.
fn helper_stderr() -> Stdio {
    ds_config::Paths::resolve()
        .and_then(|p| ds_config::open_aux_log(&p, "ds-helper.log"))
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

/// Render a `try_wait`'d exit status for a log line ("exit status: 0" / "signal: 9
/// (SIGKILL)" via `ExitStatus`'s own `Display`, or a fixed fallback when the status
/// couldn't be obtained). Shared ONLY for this one formatting detail — each caller
/// (`mark_dead_locked`'s reap, the reader thread's live unexpected-EOF detection)
/// keeps its own distinct surrounding message, so which one fired stays traceable
/// in the log — that distinction is itself diagnostic (crash reaped lazily on the
/// next speak vs. caught live the instant the pipe closed).
fn describe_exit(status: Option<std::process::ExitStatus>) -> String {
    status
        .map(|s| s.to_string())
        .unwrap_or_else(|| "exit status unavailable".to_string())
}

/// What a `speak` waits for: the persistent reader thread sets `done` on the
/// child's `DONE` (or `ERR`/EOF, with `err`). `fatal` distinguishes a child that
/// DIED (EOF/read error ⇒ reap + restart) from a soft `ERR` line (child alive).
#[derive(Default)]
struct SpeakSlot {
    done: bool,
    err: Option<String>,
    fatal: bool,
}

/// One demuxed line of a `listen` session (the reader routes the child's
/// LISTENING/PARTIAL/FINAL/STTERR/LDONE lines here).
#[cfg_attr(test, derive(Debug, PartialEq))]
enum ListenEvt {
    Partial(String),
    Final(String),
    Err(String),
    Done,
}

/// What a `listen` drains: the reader pushes [`ListenEvt`]s; `dead` marks the
/// child gone so a waiting listen unblocks.
#[derive(Default)]
struct ListenSlot {
    events: std::collections::VecDeque<ListenEvt>,
    dead: bool,
}

/// What a one-shot `diarize` waits for: the reader fills `result` from the child's
/// `DIAR <json>` (Ok) or `DIARERR <msg>` (Err), then sets `done` on `DDONE`. `dead`
/// marks the child gone mid-diarize so the waiter unblocks. Simpler than a listen —
/// diarize is record-then-return, not streamed.
#[derive(Default)]
struct DiarizeSlot {
    result: Option<Result<String, String>>,
    done: bool,
    dead: bool,
}

/// What a one-shot `enroll` waits for: the reader fills `result` from the child's
/// `EMB <json-floats>` (Ok) or `ENROLLERR <msg>` (Err), then sets `done` on `EDONE`.
/// Same shape as [`DiarizeSlot`].
#[derive(Default)]
struct EnrollSlot {
    result: Option<Result<String, String>>,
    done: bool,
    dead: bool,
}

/// The four demux slots [`TtsManager::reader_loop`] routes the child's lines into.
/// Bundled into one clone because every caller (production and all four tests)
/// always supplies the whole set together — never a subset independently.
struct ReaderSlots {
    speak: Arc<(Mutex<SpeakSlot>, Condvar)>,
    listen: Arc<(Mutex<ListenSlot>, Condvar)>,
    diarize: Arc<(Mutex<DiarizeSlot>, Condvar)>,
    enroll: Arc<(Mutex<EnrollSlot>, Condvar)>,
}

/// The three stats/lifetime sinks [`TtsManager::reader_loop`] feeds from the
/// child's `STATS`/`STTSTATS` lines. Bundled alongside [`ReaderSlots`] for the
/// same reason — always passed together, never partially.
struct ReaderStats {
    tts: Arc<crate::stats::TtsStats>,
    stt: Arc<crate::stats::SttStats>,
    lifetime: Arc<crate::stats::LifetimeSeconds>,
}

/// The model-residency state [`TtsManager::reader_loop`] flips on
/// `TTSLOADED`/`STTLOADED`/unexpected EOF, plus the shared child handle it peeks
/// (never kills) to log the real exit status. Bundled so the reader doesn't
/// thread a fixed-order run of positional args that share a type (three
/// `Arc<AtomicBool>`s among them) — an easy mis-ordering footgun.
struct ReaderModelState {
    tts_model: Arc<ModelSlot>,
    stt_model: Arc<ModelSlot>,
    stt_realized: Arc<Mutex<String>>,
    gate: Option<Arc<StatusGate>>,
    expected_eof: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
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
    /// The live Kokoro `--serve` child (None when not warm). `Arc` so the persistent
    /// reader thread can share it too — its unexpected-EOF handler `try_wait`s it
    /// (peek only, never taking/killing) to log the real exit status/signal instead
    /// of just "died, reason unknown"; the actual reap still happens later via
    /// `mark_dead`/`restart_if_crashed`.
    child: Arc<Mutex<Option<Child>>>,
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
    /// True while a DELIBERATE teardown (`stop_child` / `mark_dead`) is killing the child,
    /// so the reader can tell that EOF apart from a post-READY CRASH (AV false-positive on
    /// freshly written dylibs, OOM, GPU driver) — only the crash is reported and unloads
    /// the models from the reader; the deliberate paths own their flags and logging.
    /// Reset by each successful `start` when it installs the new reader.
    expected_eof: Arc<AtomicBool>,
    /// Bumped by every successful (re)start right when the new child is installed — the
    /// child's "incarnation number". `play`/`listen`/`diarize`/`enroll` capture it before
    /// sending a request; if the reader's EOF only wakes them up AFTER a concurrent
    /// restart has already bumped this, the death they're reacting to belongs to an OLD,
    /// already-superseded child — see [`mark_dead_if_current`](Self::mark_dead_if_current).
    child_gen: AtomicU64,
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
            child: Arc::new(Mutex::new(None)),
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
            expected_eof: Arc::new(AtomicBool::new(false)),
            child_gen: AtomicU64::new(0),
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
    /// until a manual restart. The sole caller (the download-completion hook) only reaches here
    /// for a target whose engine is wanted, so starting is correct. Reuses the shared start path.
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
        let (present, exited) = {
            let mut child = self.child.lock().unwrap();
            match child.as_mut() {
                // `try_wait` Err ⇒ treat as exited: the handle is unusable either way.
                Some(c) => (true, !matches!(c.try_wait(), Ok(None))),
                None => (false, false),
            }
        };
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
        self.child.lock().unwrap().is_some()
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
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Helper stderr → a log file (full-duplex status, capture levels,
            // barge-debug, errors) so the warm child is diagnosable; was discarded.
            .stderr(helper_stderr());
        // The daemon→helper env contract, resolved from the spawn prefs. Copy the whole
        // struct out from under its lock first (never hold a guard across the apply loop):
        //   • DONTSPEAK_PROVIDER      — Kokoro TTS execution provider ("cpu"|"cuda"|…).
        //   • DONTSPEAK_STT_PROVIDER  — local STT backend the child serves ("cpu"|"ane"|…).
        //   • DONTSPEAK_FULL_DUPLEX   — AEC duplex mode (Parakeet+Kokoro only); off ⇒ half-duplex.
        //   • DONTSPEAK_STT_PRELOAD   — preload STT in parallel with the TTS load; only when STT
        //                               is the built-in engine (`stt_provider` alone can't tell —
        //                               it resolves to "cpu" even for Off/ClaudeCode).
        // Applied as ONE set-or-remove pass so every OFF flag is explicitly CLEARED — an
        // inherited ambient value can't override the config-resolved intent. See [`child_env`].
        let prefs = self.spawn_prefs.lock().unwrap().clone();
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
                    if l == "READY" {
                        break;
                    }
                    // STT preloads in PARALLEL, so its terminal can land on either side of
                    // READY — this pre-READY wait loop and the post-READY reader both route
                    // STTLOADED through the SAME `ModelSlot::transition`. (The helper's WARMING
                    // trace lines fall through to the ignore arm: model downloads run in the
                    // engine's download manager, so there is no per-child fetch state here.)
                    let gate = self.gate.get().map(|g| g.as_ref());
                    if l == "STTLOADED" {
                        self.stt_model.transition(ModelState::Loaded, gate);
                        continue;
                    }
                    // Symmetric with STTLOADED: a mid-session `load tts` confirms residency here
                    // (though it normally lands post-READY, in the persistent reader below).
                    if l == "TTSLOADED" {
                        self.tts_model.transition(ModelState::Loaded, gate);
                        continue;
                    }
                    // STT preloads in PARALLEL, so a failed preload can also report here
                    // (before READY) rather than only in the post-READY persistent reader —
                    // see `set_stt_load_error`'s doc.
                    if let Some(msg) = l.strip_prefix("STTLOADERR ") {
                        self.set_stt_load_error(msg.trim());
                        continue;
                    }
                    if let Some(msg) = l.strip_prefix("TTSLOADERR ") {
                        self.set_tts_load_error(msg.trim());
                        continue;
                    }
                    if let Some(p) = l.strip_prefix("STT_PROVIDER ") {
                        *self.stt_realized.lock().unwrap() = p.trim().to_string();
                        continue;
                    }
                    if let Some(p) = l.strip_prefix("PROVIDER ") {
                        *self.provider.lock().unwrap() = p.trim().to_string();
                        continue;
                    }
                    if let Some(msg) = l.strip_prefix("ERR") {
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
        {
            // Bump the generation WHILE still holding the `child` lock: anyone who next
            // observes this child via `is_running()`/`child.lock()` is then guaranteed
            // (by the mutex's own happens-before edge) to see the new generation too —
            // see `mark_dead_if_current`.
            let mut child_guard = self.child.lock().unwrap();
            *child_guard = Some(child);
            self.child_gen.fetch_add(1, Ordering::Relaxed);
        }
        *self.stdin.lock().unwrap() = Some(stdin);
        // Spawn the persistent demux reader: it owns stdout and routes the child's
        // lines into the speak/listen slots, so a speak and a listen can be in
        // flight at once (full-duplex coexist). It exits on EOF (child killed).
        // The new child is healthy: from here an EOF is a CRASH unless a deliberate
        // teardown (`stop_child`/`mark_dead`) re-marks it expected before killing.
        self.expected_eof.store(false, Ordering::Relaxed);
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
            let expected_eof = self.expected_eof.clone();
            // So the reader's unexpected-EOF handler can try_wait() the real exit
            // status/signal (peek only — the actual reap stays with mark_dead/
            // restart_if_crashed, so no double-teardown race).
            let child_handle = self.child.clone();
            // STT preloads on a PARALLEL thread, so its `STT_PROVIDER` line often lands AFTER READY
            // (and always for a lazy `load stt`) — i.e. in THIS persistent reader, not start()'s
            // pre-READY wait loop. Clone the realized-provider slot in so the reader can capture it;
            // without this the STT status row stays "CPU" while STT actually ran on the GPU.
            let stt_realized = self.stt_realized.clone();
            // The status push-gate, so a post-READY STTLOADED pushes LIVE instead of waiting
            // for the next poll.
            let gate = self.gate.get().cloned();
            std::thread::spawn(move || {
                Self::reader_loop(
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
                        expected_eof,
                        child: child_handle,
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
        self.expected_eof.store(true, Ordering::Relaxed);
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
        if let Some(mut child) = self.child.lock().unwrap().take() {
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
        self.expected_eof.store(true, Ordering::Relaxed);
        *self.stdin.lock().unwrap() = None;
        // A dead child holds no models — clear the residency flags so the dot doesn't
        // show a stale "running" until the next start (this comment used to claim that
        // already, without actually doing it — the exact bug class fixed in
        // set_caps_gate, engine.rs).
        self.clear_loaded_flags();
        if let Some(mut child) = self.child.lock().unwrap().take() {
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
        if self.child_gen.load(Ordering::Relaxed) != expected_gen {
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

    /// The persistent stdout reader: owns the warm child's stdout and demuxes each
    /// line into the speak/listen slots, so a `speak` and a `listen` can be served
    /// concurrently. Returns on EOF / read error (child gone), signalling both
    /// slots so any waiter unblocks.
    fn reader_loop(
        // `impl BufRead` (not `BufReader<ChildStdout>`) so the EOF handling is unit-testable
        // with a canned byte slice — production passes the child's buffered stdout.
        mut stdout: impl BufRead,
        slots: ReaderSlots,
        stats: ReaderStats,
        model: ReaderModelState,
    ) {
        let ReaderSlots {
            speak: speak_slot,
            listen: listen_slot,
            diarize: diarize_slot,
            enroll: enroll_slot,
        } = slots;
        let ReaderStats {
            tts: stats,
            stt: stt_stats,
            lifetime,
        } = stats;
        let ReaderModelState {
            tts_model,
            stt_model,
            stt_realized,
            gate,
            expected_eof,
            child,
        } = model;
        let push_listen = |evt: ListenEvt| {
            let (m, cv) = &*listen_slot;
            m.lock().unwrap().events.push_back(evt);
            cv.notify_all();
        };
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    // Child gone: unblock a waiting speak (fatal) and a waiting listen.
                    let (m, cv) = &*speak_slot;
                    let mut s = m.lock().unwrap();
                    s.done = true;
                    s.fatal = true;
                    if s.err.is_none() {
                        s.err = Some("TTS child closed".into());
                    }
                    cv.notify_all();
                    drop(s);
                    let (lm, lcv) = &*listen_slot;
                    lm.lock().unwrap().dead = true;
                    lcv.notify_all();
                    let (dm, dcv) = &*diarize_slot;
                    dm.lock().unwrap().dead = true;
                    dcv.notify_all();
                    let (em, ecv) = &*enroll_slot;
                    em.lock().unwrap().dead = true;
                    ecv.notify_all();
                    // An EOF nobody marked expected = the child DIED post-READY (AV
                    // false-positive on freshly written dylibs, OOM, GPU driver). Such
                    // deaths used to be invisible — no log line, and the stale "loaded"
                    // flags stayed green until some later write failed. Unload both models
                    // NOW (the status dots go amber immediately) and say so; the worker's
                    // `restart_if_crashed` revives the child on the next speak. Deliberate
                    // teardowns (`stop_child`/`mark_dead`) own their flags and logging.
                    if !expected_eof.load(Ordering::Relaxed) {
                        // `ModelSlot::transition` to `Idle` clears any per-model "failed to
                        // load" state too (mirrors every other teardown path —
                        // `start_locked`'s fresh-install, `clear_loaded_flags`,
                        // `unload_engine`): a crashed child's stale error must not keep
                        // showing after the process is gone, or it lingers until the next
                        // successful `start_locked`. Each call is independently change-gated,
                        // so this replaces the old unconditional gate bump below it too — a
                        // real transition on either model already wakes a blocked waiter.
                        tts_model.transition(ModelState::Idle, gate.as_deref());
                        stt_model.transition(ModelState::Idle, gate.as_deref());
                        // Debug aid: try_wait() the real exit status/signal at the MOMENT
                        // of detection — peek only (never kill/take), so the later
                        // mark_dead/restart_if_crashed still owns the actual reap. Without
                        // this the cause (SIGKILL/SIGSEGV/OOM/clean exit) was only ever
                        // learned lazily, whenever the next speak/listen happened to
                        // trigger restart_if_crashed — which may be minutes later, or
                        // never before the app itself restarts.
                        let status = child
                            .lock()
                            .unwrap()
                            .as_mut()
                            .and_then(|c| c.try_wait().ok().flatten());
                        log(&format!(
                            "WARN: TTS warm child exited unexpectedly ({}) — models \
                             unloaded; the next speak restarts it",
                            describe_exit(status)
                        ));
                    }
                    return;
                }
                Ok(_) => {
                    let l = line.trim();
                    // ── speak terminals ──────────────────────────────────────────
                    if l == "DONE" {
                        let (m, cv) = &*speak_slot;
                        m.lock().unwrap().done = true;
                        cv.notify_all();
                    } else if let Some(rest) = l.strip_prefix("STATS ") {
                        // Persist the per-utterance playback timing to the activity log (it
                        // otherwise only fed the in-app stats view, so a clipped/short reply left
                        // no trace — the gap that made the tail-clip bug hard to diagnose). DEBUG
                        // level: off by default, one concise line per speak when DONTSPEAK_DEBUG
                        // is on, size-rotated like the rest.
                        crate::logging::debug(&format!("TTS speak {rest}"));
                        if let Some(secs) = stats.record_stats_line(rest) {
                            lifetime.add_tts(secs);
                        }
                    } else if let Some(msg) = l.strip_prefix("ERR") {
                        let (m, cv) = &*speak_slot;
                        let mut s = m.lock().unwrap();
                        s.err = Some(format!("TTS child error:{msg}"));
                        s.done = true; // soft error: child stays alive
                        cv.notify_all();
                    // ── listen events ────────────────────────────────────────────
                    } else if l == "LDONE" {
                        push_listen(ListenEvt::Done);
                    } else if let Some(rest) = l.strip_prefix("PARTIAL ") {
                        push_listen(ListenEvt::Partial(rest.to_string()));
                    } else if l == "FINAL" {
                        push_listen(ListenEvt::Final(String::new()));
                    } else if let Some(rest) = l.strip_prefix("FINAL ") {
                        push_listen(ListenEvt::Final(rest.to_string()));
                    } else if let Some(rest) = l.strip_prefix("STTSTATS ") {
                        // Per-listen transcription timing → the activity log, the speech-IN
                        // mirror of the `TTS speak` line above (so a slow dictation leaves a
                        // trace, not just an in-app stats bump). DEBUG: off by default, one
                        // concise line per listen when DONTSPEAK_DEBUG is on.
                        crate::logging::debug(&format!("STT listen {rest}"));
                        if let Some(secs) = stt_stats.record_stt_line(rest) {
                            lifetime.add_stt(secs);
                        }
                    } else if let Some(rest) = l.strip_prefix("STTERR ") {
                        push_listen(ListenEvt::Err(rest.to_string()));
                    // STT lifecycle — the SAME `ModelSlot::transition` `start()`'s wait loop
                    // uses, so the pre-/post-READY paths can't drift (STT preloads in parallel →
                    // its terminal lands on either side of READY).
                    } else if l == "TTSLOADED" {
                        // The Kokoro analogue of STTLOADED: the helper confirms the model is
                        // resident after a `load tts`, so the dot greens only now — not on the
                        // optimistic request. (The COMMON path for a mid-session TTS (re)select.)
                        // One write does what used to be two kept in lockstep (mark loaded +
                        // clear any stale load error) — see `ModelSlot::transition`.
                        tts_model.transition(ModelState::Loaded, gate.as_deref());
                    } else if l == "STTLOADED" {
                        stt_model.transition(ModelState::Loaded, gate.as_deref());
                    } else if let Some(msg) = l.strip_prefix("STTLOADERR ") {
                        // A mid-session `load stt`/preload failure (e.g. a transient AV-scan
                        // file-not-found on an already-downloaded model) — surfaced per-model so
                        // `model_status`'s `parakeet` row can show it without touching `kokoro`.
                        // Change-gated: the exact same failure can repeat identically several
                        // times in a row and must not spam `StatusGate` each time.
                        stt_model.transition(
                            ModelState::Failed(msg.trim().to_string()),
                            gate.as_deref(),
                        );
                    } else if let Some(msg) = l.strip_prefix("TTSLOADERR ") {
                        tts_model.transition(
                            ModelState::Failed(msg.trim().to_string()),
                            gate.as_deref(),
                        );
                    } else if let Some(p) = l.strip_prefix("STT_PROVIDER ") {
                        // The REALIZED STT EP (mirrors the pre-READY parse in start()). Post-READY is
                        // the COMMON path — the parallel preload usually reports after READY — so
                        // this is what keeps the STT status row honest on a GPU box.
                        *stt_realized.lock().unwrap() = p.trim().to_string();
                    // ── diarize events ───────────────────────────────────────────
                    } else if let Some(rest) = l.strip_prefix("DIAR ") {
                        diarize_slot.0.lock().unwrap().result = Some(Ok(rest.to_string()));
                    } else if let Some(rest) = l.strip_prefix("DIARERR ") {
                        diarize_slot.0.lock().unwrap().result = Some(Err(rest.to_string()));
                    } else if l == "DDONE" {
                        let (m, cv) = &*diarize_slot;
                        m.lock().unwrap().done = true;
                        cv.notify_all();
                    // ── enroll events ────────────────────────────────────────────
                    } else if let Some(rest) = l.strip_prefix("EMB ") {
                        enroll_slot.0.lock().unwrap().result = Some(Ok(rest.to_string()));
                    } else if let Some(rest) = l.strip_prefix("ENROLLERR ") {
                        enroll_slot.0.lock().unwrap().result = Some(Err(rest.to_string()));
                    } else if l == "EDONE" {
                        let (m, cv) = &*enroll_slot;
                        m.lock().unwrap().done = true;
                        cv.notify_all();
                    }
                    // else: LISTENING / PROVIDER / other chatter — ignore
                }
            }
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
        if !self.is_running() {
            return Err(std::io::Error::other("TTS child not running"));
        }
        // Snapshot the child's generation for THIS request — if the reader only wakes us
        // (fatal) after a concurrent restart has ALREADY installed a new child, this lets
        // us tell "our child died" apart from "a stale EOF from an old, superseded child".
        let my_gen = self.child_gen.load(Ordering::Relaxed);
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
mod dl_lifecycle_tests {
    use super::describe_exit;

    #[test]
    fn describe_exit_falls_back_when_no_status_was_obtained() {
        assert_eq!(describe_exit(None), "exit status unavailable");
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
mod reader_eof_tests {
    use super::*;

    /// A fresh [`ModelSlot`] already transitioned to `Loaded` — the post-READY state in
    /// which a crash used to leave the old raw flags stale. No gate: these tests assert
    /// on `is_loaded()`/`error()` outcomes, not bump counts.
    fn loaded_slot() -> Arc<ModelSlot> {
        let slot = Arc::new(ModelSlot::new());
        slot.transition(ModelState::Loaded, None);
        slot
    }

    /// Drive `reader_loop` over a canned child stdout (ending in EOF, like a real death)
    /// and return `(tts_loaded, stt_loaded, speak_fatal)` afterwards. Both models start
    /// `Loaded` — see [`loaded_slot`].
    fn run_reader(stdout: &[u8], expected_eof: bool) -> (bool, bool, bool) {
        let dir = tempfile::tempdir().unwrap();
        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        let tts_model = loaded_slot();
        let stt_model = loaded_slot();
        TtsManager::reader_loop(
            stdout,
            ReaderSlots {
                speak: speak_slot.clone(),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-eof-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                gate: None,
                expected_eof: Arc::new(AtomicBool::new(expected_eof)),
                child: Arc::new(Mutex::new(None)),
            },
        );
        let fatal = speak_slot.0.lock().unwrap().fatal;
        (tts_model.is_loaded(), stt_model.is_loaded(), fatal)
    }

    #[test]
    fn unexpected_eof_unloads_both_models() {
        // A post-READY child DEATH (no teardown marked the EOF expected): the reader must
        // drop BOTH loaded flags so the status dots go amber immediately. Previously the
        // stale green survived until some later write failed — and with the flags then
        // cleared by `mark_dead`, the worker's not-ready guard dropped every speak, so the
        // crash wedged TTS+STT in "Starting" until an app restart.
        let (tts, stt, fatal) = run_reader(b"", false);
        assert!(!tts && !stt, "an unexpected EOF must unload both models");
        assert!(fatal, "a waiting speak must be unblocked as fatal");
    }

    #[test]
    #[cfg(unix)]
    fn unexpected_eof_reads_the_real_exit_status_through_the_shared_child_handle() {
        // End-to-end through the ACTUAL wiring `start()` uses (not just a canned byte slice
        // with no child behind it): a real spawned process, sharing the same
        // `Arc<Mutex<Option<Child>>>` production hands the reader thread. Proves try_wait()
        // sees a genuine ExitStatus through the new `child` param — this is what lets a log
        // line say WHY the child died instead of "reason unknown".
        let dir = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("true")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn `true`");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let _ = child.wait(); // let it actually exit before the reader sees stdout EOF
        let child_handle = Arc::new(Mutex::new(Some(child)));

        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        TtsManager::reader_loop(
            stdout,
            ReaderSlots {
                speak: speak_slot,
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-real-child-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: loaded_slot(),
                stt_model: loaded_slot(),
                stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                gate: None,
                expected_eof: Arc::new(AtomicBool::new(false)), // unexpected — the crash-detection path
                child: child_handle.clone(),
            },
        );

        // The peek must not have consumed/broken the handle — a real teardown (mark_dead)
        // still needs to try_wait()/kill() it afterwards without erroring.
        let status = child_handle.lock().unwrap().as_mut().unwrap().try_wait();
        assert!(
            matches!(status, Ok(Some(_))),
            "exit status still readable after the reader's peek: {status:?}"
        );
    }

    #[test]
    fn deliberate_stop_eof_leaves_the_flags_to_the_stopper() {
        // `stop_child`/`mark_dead` set `expected_eof` before killing: they own the flag
        // clearing and the logging, so the reader must NOT double-report their EOF as a
        // crash (a restart would otherwise log a spurious "exited unexpectedly" WARN).
        let (tts, stt, fatal) = run_reader(b"", true);
        assert!(
            tts && stt,
            "a deliberate stop's EOF must not touch the flags"
        );
        assert!(fatal, "waiters still unblock on any EOF");
    }

    #[test]
    fn post_ready_lines_still_route_before_eof() {
        // The genericized reader (`impl BufRead`) must keep demuxing real lines: an
        // STTLOADED before the crash re-greens STT, then the unexpected EOF clears both.
        let (tts, stt, _) = run_reader(b"STTLOADED\nDONE\n", false);
        assert!(
            !tts && !stt,
            "EOF handling runs after the lines are demuxed"
        );
    }

    /// Like `run_reader` but with EXPLICIT initial loaded states — for asserting a line
    /// FLIPS a model to `Loaded` (a load terminal greening a not-yet-resident model), not
    /// just that a crash clears one. `expected_eof=true` so the trailing EOF leaves the
    /// state exactly as the demuxed line set it (the deliberate-stop path).
    fn run_reader_init(tts0: bool, stt0: bool, stdout: &[u8]) -> (bool, bool) {
        let dir = tempfile::tempdir().unwrap();
        let mk = |loaded: bool| {
            let slot = Arc::new(ModelSlot::new());
            if loaded {
                slot.transition(ModelState::Loaded, None);
            }
            slot
        };
        let tts_model = mk(tts0);
        let stt_model = mk(stt0);
        TtsManager::reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-load-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                gate: None,
                expected_eof: Arc::new(AtomicBool::new(true)),
                child: Arc::new(Mutex::new(None)),
            },
        );
        (tts_model.is_loaded(), stt_model.is_loaded())
    }

    #[test]
    fn ttsloaded_greens_tts_only() {
        // The Kokoro analogue of STTLOADED: a mid-session `load tts` confirms residency, so
        // the reader greens `tts_loaded` ONLY on this line — never on the optimistic request
        // (the old premature-green). Start both flags FALSE (fresh (re)load) and confirm the
        // TTS terminal flips TTS and leaves STT alone.
        let (tts, stt) = run_reader_init(false, false, b"TTSLOADED\n");
        assert!(tts, "TTSLOADED must mark the TTS model resident");
        assert!(!stt, "TTSLOADED must not touch the STT flag");
    }

    #[test]
    fn sttloaded_greens_stt_only() {
        // Symmetric guard for the STT terminal, so the two load paths can't silently swap
        // (a regression that would green dictation while narration is still warming).
        let (tts, stt) = run_reader_init(false, false, b"STTLOADED\n");
        assert!(stt, "STTLOADED must mark the STT model resident");
        assert!(!tts, "STTLOADED must not touch the TTS flag");
    }

    /// Like `run_reader_init`, but starting both models `Idle` and additionally returning
    /// their drained `error()` — for the `STTLOADERR`/`TTSLOADERR` coverage below.
    fn run_reader_init_errs(stdout: &[u8]) -> (bool, bool, Option<String>, Option<String>) {
        let dir = tempfile::tempdir().unwrap();
        let tts_model = Arc::new(ModelSlot::new());
        let stt_model = Arc::new(ModelSlot::new());
        TtsManager::reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-loaderr-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: tts_model.clone(),
                stt_model: stt_model.clone(),
                stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                gate: None,
                expected_eof: Arc::new(AtomicBool::new(true)),
                child: Arc::new(Mutex::new(None)),
            },
        );
        (
            tts_model.is_loaded(),
            stt_model.is_loaded(),
            stt_model.error(),
            tts_model.error(),
        )
    }

    #[test]
    fn sttloaderr_sets_the_error_without_touching_stt_loaded() {
        let (_, stt_loaded, stt_err, tts_err) = run_reader_init_errs(b"STTLOADERR boom\n");
        assert_eq!(stt_err.as_deref(), Some("boom"));
        assert_eq!(tts_err, None, "STTLOADERR must not touch tts_load_error");
        assert!(
            !stt_loaded,
            "a load FAILURE must not mark the model resident"
        );
    }

    #[test]
    fn ttsloaderr_sets_the_error_without_touching_tts_loaded() {
        let (tts_loaded, _, stt_err, tts_err) = run_reader_init_errs(b"TTSLOADERR boom\n");
        assert_eq!(tts_err.as_deref(), Some("boom"));
        assert_eq!(stt_err, None, "TTSLOADERR must not touch stt_load_error");
        assert!(
            !tts_loaded,
            "a load FAILURE must not mark the model resident"
        );
    }

    #[test]
    fn sttloaded_after_sttloaderr_clears_the_error() {
        // The AV-scan-retry scenario this whole channel exists for: a transient failure
        // followed by a successful (re)load must clear the stale error, not leave it stuck
        // showing "failed" forever alongside a now-healthy green dot.
        let (_, stt_loaded, stt_err, _) =
            run_reader_init_errs(b"STTLOADERR transient boom\nSTTLOADED\n");
        assert!(
            stt_loaded,
            "STTLOADED after the retry must mark it resident"
        );
        assert_eq!(
            stt_err, None,
            "a subsequent STTLOADED must clear the earlier STTLOADERR"
        );
    }

    /// A `Read` that yields `data` once, then BLOCKS (never reports EOF) until `close` is
    /// flipped — used to test a reader_loop line WITHOUT the trailing implicit EOF that a
    /// finite canned byte slice (as `run_reader` uses) always ends in. That EOF unconditionally
    /// sets `speak_slot.fatal = true`, which would clobber the very distinction the soft-ERR
    /// test below exists to observe (`ERR` sets `err`+`done` WITHOUT `fatal`). Wrapped in a
    /// `BufReader` (like production's real `ChildStdout`) to satisfy `reader_loop`'s bound.
    struct BlockThenClose {
        data: Vec<u8>,
        pos: usize,
        close: Arc<AtomicBool>,
    }

    impl std::io::Read for BlockThenClose {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos < self.data.len() {
                let n = buf.len().min(self.data.len() - self.pos);
                buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            // Exhausted: block until told to close, THEN report EOF — never spontaneously.
            while !self.close.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(0)
        }
    }

    #[test]
    fn soft_err_line_sets_err_without_marking_fatal() {
        // The soft-ERR arm (child stays alive) sets `err`+`done` but leaves `fatal` false —
        // distinct from the EOF/read-error arm, which sets BOTH. Never previously exercised:
        // see `BlockThenClose` for why a plain finite byte slice can't observe this directly.
        let dir = tempfile::tempdir().unwrap();
        let close = Arc::new(AtomicBool::new(false));
        let stdout = BufReader::new(BlockThenClose {
            data: b"ERR bad phoneme\n".to_vec(),
            pos: 0,
            close: close.clone(),
        });
        let speak_slot = Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new()));
        let tts_model = loaded_slot();
        let stt_model = loaded_slot();
        let reader_speak = speak_slot.clone();
        let reader_tts = tts_model.clone();
        let reader_stt = stt_model.clone();
        let handle = std::thread::spawn(move || {
            TtsManager::reader_loop(
                stdout,
                ReaderSlots {
                    speak: reader_speak,
                    listen: Arc::new((Mutex::new(ListenSlot::default()), Condvar::new())),
                    diarize: Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new())),
                    enroll: Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new())),
                },
                ReaderStats {
                    tts: Arc::new(crate::stats::TtsStats::new()),
                    stt: Arc::new(crate::stats::SttStats::new()),
                    lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                        dir.path().join("ds-stats-reader-soft-err-test.json"),
                    )),
                },
                ReaderModelState {
                    tts_model: reader_tts,
                    stt_model: reader_stt,
                    stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                    gate: None,
                    expected_eof: Arc::new(AtomicBool::new(true)),
                    child: Arc::new(Mutex::new(None)),
                },
            );
        });

        // Wait for the soft-ERR line to land (the reader sets `done` on it, same as DONE) —
        // at this point the reader is still blocked inside `BlockThenClose::read`, so the
        // trailing EOF (and its `fatal = true`) has NOT happened yet.
        let (m, cv) = &*speak_slot;
        let mut s = m.lock().unwrap();
        while !s.done {
            s = cv.wait(s).unwrap();
        }
        let err = s.err.clone();
        let fatal = s.fatal;
        drop(s);
        let loaded_ok = tts_model.is_loaded() && stt_model.is_loaded();

        // Let the reader exit cleanly (EOF) and join it BEFORE asserting, so a future
        // regression here (which is exactly what these assertions exist to catch) can't
        // also leak the blocked reader thread spinning in `BlockThenClose::read` for the
        // rest of the test process's life.
        close.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert_eq!(err.as_deref(), Some("TTS child error: bad phoneme"));
        assert!(
            !fatal,
            "a soft ERR (child stays alive) must not mark the speak fatal"
        );
        assert!(loaded_ok, "a soft ERR must not touch the loaded flags");
    }

    /// Drained results from a `run_reader_slots` call: the `listen_slot` events (in order),
    /// then the terminal `diarize_slot` (result, done) and `enroll_slot` (result, done).
    type ReaderSlotsResult = (
        Vec<ListenEvt>,
        Option<Result<String, String>>,
        bool,
        Option<Result<String, String>>,
        bool,
    );

    /// Drive `reader_loop` over a canned child stdout and return the drained `listen_slot`
    /// events (in order) plus the terminal `diarize_slot`/`enroll_slot` results — the
    /// Listen/Diarize/Enroll demux arms, previously exercised by no test (only
    /// `tts_loaded`/`stt_loaded`/`speak_slot.fatal` were ever asserted on in this module).
    fn run_reader_slots(stdout: &[u8]) -> ReaderSlotsResult {
        let dir = tempfile::tempdir().unwrap();
        let listen_slot = Arc::new((Mutex::new(ListenSlot::default()), Condvar::new()));
        let diarize_slot = Arc::new((Mutex::new(DiarizeSlot::default()), Condvar::new()));
        let enroll_slot = Arc::new((Mutex::new(EnrollSlot::default()), Condvar::new()));
        TtsManager::reader_loop(
            stdout,
            ReaderSlots {
                speak: Arc::new((Mutex::new(SpeakSlot::default()), Condvar::new())),
                listen: listen_slot.clone(),
                diarize: diarize_slot.clone(),
                enroll: enroll_slot.clone(),
            },
            ReaderStats {
                tts: Arc::new(crate::stats::TtsStats::new()),
                stt: Arc::new(crate::stats::SttStats::new()),
                lifetime: Arc::new(crate::stats::LifetimeSeconds::load(
                    dir.path().join("ds-stats-reader-slots-test.json"),
                )),
            },
            ReaderModelState {
                tts_model: loaded_slot(),
                stt_model: loaded_slot(),
                stt_realized: Arc::new(Mutex::new("CPU".to_string())),
                gate: None,
                expected_eof: Arc::new(AtomicBool::new(true)),
                child: Arc::new(Mutex::new(None)),
            },
        );
        let events: Vec<ListenEvt> = listen_slot.0.lock().unwrap().events.drain(..).collect();
        let diarize = diarize_slot.0.lock().unwrap();
        let enroll = enroll_slot.0.lock().unwrap();
        (
            events,
            diarize.result.clone(),
            diarize.done,
            enroll.result.clone(),
            enroll.done,
        )
    }

    #[test]
    fn listen_demux_orders_partial_final_done() {
        let (events, ..) = run_reader_slots(b"PARTIAL hi\nFINAL done\nLDONE\n");
        assert_eq!(
            events,
            vec![
                ListenEvt::Partial("hi".to_string()),
                ListenEvt::Final("done".to_string()),
                ListenEvt::Done,
            ]
        );
    }

    #[test]
    fn listen_demux_routes_sttterr() {
        let (events, ..) = run_reader_slots(b"STTERR mic denied\nLDONE\n");
        assert_eq!(
            events,
            vec![ListenEvt::Err("mic denied".to_string()), ListenEvt::Done]
        );
    }

    #[test]
    fn diarize_demux_routes_ok_result_and_done() {
        let (_, result, done, ..) = run_reader_slots(b"DIAR {\"segments\":[]}\nDDONE\n");
        assert_eq!(result, Some(Ok("{\"segments\":[]}".to_string())));
        assert!(done);
    }

    #[test]
    fn diarize_demux_routes_err_result_and_done() {
        let (_, result, done, ..) = run_reader_slots(b"DIARERR boom\nDDONE\n");
        assert_eq!(result, Some(Err("boom".to_string())));
        assert!(done);
    }

    #[test]
    fn enroll_demux_routes_ok_result_and_done() {
        let (_, _, _, result, done) = run_reader_slots(b"EMB [0.1,0.2]\nEDONE\n");
        assert_eq!(result, Some(Ok("[0.1,0.2]".to_string())));
        assert!(done);
    }

    #[test]
    fn enroll_demux_routes_err_result_and_done() {
        let (_, _, _, result, done) = run_reader_slots(b"ENROLLERR boom\nEDONE\n");
        assert_eq!(result, Some(Err("boom".to_string())));
        assert!(done);
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

    #[test]
    fn start_against_a_nonexistent_binary_sets_last_error_and_bumps_the_gate() {
        // Exercises start_locked's real spawn-failure branch (Command::spawn erroring on
        // a path that doesn't exist) — no mock, no real ds-helper binary needed.
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
    }
}
