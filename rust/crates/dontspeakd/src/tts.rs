//! TtsManager — the engine's warm Kokoro owner (Phase 2).
//!
//! The engine supervises ONE long-lived `ds-helper --serve` child that holds
//! the ~325 MB model warm, so no reply pays the cold model-load cost. Enabling TTS
//! spawns the child; disabling KILLS it (freeing the model with no ONNX-teardown
//! crash, since a killed process runs no destructors). Speak/preview/stop are
//! mediated over the child's stdio with the protocol documented in
//! `ds_helper.rs`.
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
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;

use crate::log;
use crate::status::StatusGate;

/// Where the warm child's stderr is sent (the app has no console, so its diagnostics —
/// STTSTATS/rtf, RMS, gain, model EP — must go to a file). Routed through the shared
/// `open_aux_log` so it sits BESIDE the engine's unified log in the one per-OS logs dir AND is
/// size-rotated like it (never a second location, never unbounded). Null only if it can't open.
fn helper_stderr() -> Stdio {
    ds_config::Paths::resolve()
        .and_then(|p| ds_config::open_aux_log(&p, "ds-helper.log"))
        .map(Stdio::from)
        .unwrap_or_else(Stdio::null)
}

/// Parse the optional `<file_done> <file_total> <file_index> <file_count>` payload of a
/// `DOWNLOADING tts/stt …` line. `""` (the bare signal) → `None`; index/count default to 0 if a
/// shorter (legacy `<done> <total>`) line arrives.
fn parse_dl(rest: &str) -> Option<(u64, u64, u64, u64)> {
    let mut it = rest.split_whitespace();
    let fd = it.next()?.parse().ok()?;
    let ft = it.next()?.parse().ok()?;
    let idx = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let cnt = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Some((fd, ft, idx, cnt))
}

/// `(file_done, file_total, file_index, file_count)` of an in-flight Core ML download — the
/// dot shows "<index>/<count> · <pct>%" from it. All-zero when idle.
type DlProgress = (u64, u64, u64, u64);

/// THE one model-lifecycle line handler, generic over the engine `kind` ("tts" / "stt" / …) so
/// EVERY engine's download → warm → ready transition is driven by the SAME code (no per-engine
/// copy-paste), and `start()`'s pre-READY wait loop + the post-READY reader thread stay in
/// lockstep (a parallel preload's signals can land on either side of READY). Handles the
/// "in progress" lines:
///   `DOWNLOADING <kind> <fd> <ft> <idx> <cnt>` → downloading + per-file progress;
///   `WARMING <kind>`                           → loading/warming (clears downloading →
///                                                 "Starting…").
/// The terminal "loaded" line (`READY` for TTS, `STTLOADED` for STT) is the caller's, via
/// [`mark_loaded`] — it's engine-specific (READY also breaks the wait loop). Returns true if
/// `line` belonged to `kind`. The live status PUSH is built IN: a handled line bumps `gate`, so
/// EVERY engine's % ticks live in EVERY path (wait loop AND reader) — the push can't be
/// forgotten at a call site (that omission was the "Parakeet % sticks then jumps" bug).
fn apply_dl_progress(
    line: &str,
    kind: &str,
    downloading: &AtomicBool,
    dl: &Mutex<DlProgress>,
    gate: Option<&StatusGate>,
) -> bool {
    if let Some(rest) = line
        .strip_prefix("DOWNLOADING ")
        .and_then(|r| r.strip_prefix(kind))
    {
        downloading.store(true, Ordering::Relaxed);
        if let Some(dt) = parse_dl(rest) {
            *dl.lock().unwrap() = dt;
        }
    } else if line.strip_prefix("WARMING ") == Some(kind) {
        downloading.store(false, Ordering::Relaxed);
        *dl.lock().unwrap() = (0, 0, 0, 0);
    } else {
        return false;
    }
    if let Some(g) = gate {
        g.bump();
    }
    true
}

/// Mark an engine's model RESIDENT + WARM (the dot greens): clear downloading + its progress,
/// set loaded, and PUSH (bump the gate). Shared by the TTS `READY` and STT `STTLOADED` terminals
/// so "green = warm" — and the push that surfaces it — live in ONE place.
fn mark_loaded(
    downloading: &AtomicBool,
    dl: &Mutex<DlProgress>,
    loaded: &AtomicBool,
    gate: Option<&StatusGate>,
) {
    downloading.store(false, Ordering::Relaxed);
    *dl.lock().unwrap() = (0, 0, 0, 0);
    loaded.store(true, Ordering::Relaxed);
    if let Some(g) = gate {
        g.bump();
    }
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

pub struct TtsManager {
    /// Path to the `ds-helper` helper binary.
    bin: PathBuf,
    /// Serializes warm-child LIFECYCLE transitions — `start` / `stop_child` /
    /// `mark_dead`. Without it, a crash-driven `mark_dead` from a concurrent
    /// play+listen pair (both wake fatal on the same EOF) could race a restart and
    /// `join` the WRONG reader. The OUTERMOST lock: always taken before
    /// `child`/`stdin`/`reader`. Brief, never held across a slot `Condvar` wait.
    lifecycle: Mutex<()>,
    /// The live Kokoro `--serve` child (None when not warm).
    child: Mutex<Option<Child>>,
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
    /// The user's provider PREFERENCE ("auto"|"cpu"|"cuda"|"coreml"|"ane") —
    /// drives the `DONTSPEAK_PROVIDER` env when (re)starting the warm child.
    provider_pref: Mutex<String>,
    /// Whether the next warm child should run in full-duplex AEC mode — the engine
    /// sets this to `full_duplex && stt provider == cpu`. Drives the
    /// `DONTSPEAK_FULL_DUPLEX` env when (re)starting the child.
    full_duplex_pref: Mutex<bool>,
    /// The full-duplex mode the CURRENTLY running child was started with, so a
    /// changed `full_duplex_pref` can trigger exactly one restart (mirrors how
    /// `provider` tracks the running provider for `set_provider`).
    full_duplex_active: Mutex<bool>,
    /// The local STT backend the next warm child should use — the resolved provider token
    /// ("cpu"|"cuda"|"ane"|"system"), from `helper_stt_provider`. Drives the
    /// `DONTSPEAK_STT_PROVIDER` env when (re)starting.
    stt_provider_pref: Mutex<String>,
    /// The STT engine the CURRENTLY running child was started with, so a changed
    /// `stt_provider_pref` triggers exactly one restart (mirrors `full_duplex_active`).
    stt_provider_active: Mutex<String>,
    /// Whether STT (Parakeet) should be PRELOADED in the warm child — `helper_uses_stt(cfg)`,
    /// i.e. STT is the built-in engine. `stt_provider_pref` is NOT a usable on/off signal
    /// (it resolves to "cpu" even for Off/ClaudeCode), so this is tracked separately and
    /// drives `DONTSPEAK_STT_PRELOAD`, which gates the helper's parallel STT-preload thread.
    stt_wanted: Mutex<bool>,
    /// Which models are CURRENTLY resident in the warm helper. Kokoro is eager (true
    /// once the child is READY); Parakeet is lazy (true after the first `listen`). An
    /// `unload` clears the matching flag; the helper stopping clears both. Surfaced in
    /// `model_status` because the memory number is too noisy (ort retains freed arena).
    tts_loaded: AtomicBool,
    /// `Arc` so the stdout reader thread can flip it true on the helper's `STTLOADED`
    /// confirmation (emitted after preload + the graph WARMUP) — the dot only greens when
    /// the model is truly resident AND warm, not optimistically on the load request.
    stt_loaded: Arc<AtomicBool>,
    /// True while the helper is DOWNLOADING the Kokoro Core ML model BEFORE its warm load
    /// (the helper emits `DOWNLOADING tts` when the model is absent; cleared on READY). Drives
    /// the "downloading" dot on a clean install so it never reads a premature "starting"/green.
    tts_downloading: AtomicBool,
    /// `Arc` reader-thread-set twin for Parakeet STT — set on `DOWNLOADING stt`, cleared on
    /// `STTLOADED`.
    stt_downloading: Arc<AtomicBool>,
    /// Latest `(file_done, file_total, file_index, file_count)` for the apple-native Core ML
    /// download the WARM CHILD does itself, reported via `DOWNLOADING tts/stt <fd> <ft> <idx>
    /// <cnt>` — surfaced as "<idx>/<cnt> · <this file's %>" on the engine row. `stt_dl` is an
    /// `Arc` (the reader thread sets it). Reset to all-zero on READY / STTLOADED.
    tts_dl: Mutex<(u64, u64, u64, u64)>,
    stt_dl: Arc<Mutex<(u64, u64, u64, u64)>>,
    /// Global MUTE: when true the warm child plays silence (queue still drains; only the audio
    /// is zeroed). Toggled by a Caps-tap (dictation off) and the tray checkbox. Read by the
    /// status snapshot; pushed to the child via the `mute` op.
    muted: AtomicBool,
    /// The shared status-push gate, installed once at boot via [`set_status_gate`]. A
    /// mute toggle bumps it so a blocked `WaitModelStatus` wakes immediately (the muted
    /// flag is part of `model_status`). `OnceLock`-empty in tests / before wiring, where
    /// `set_muted` simply skips the bump.
    gate: OnceLock<Arc<StatusGate>>,
}

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
            child: Mutex::new(None),
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
            provider_pref: Mutex::new("auto".to_string()),
            full_duplex_pref: Mutex::new(false),
            full_duplex_active: Mutex::new(false),
            stt_provider_pref: Mutex::new("ane".to_string()),
            stt_wanted: Mutex::new(false),
            stt_provider_active: Mutex::new(String::new()),
            tts_loaded: AtomicBool::new(false),
            stt_loaded: Arc::new(AtomicBool::new(false)),
            tts_downloading: AtomicBool::new(false),
            stt_downloading: Arc::new(AtomicBool::new(false)),
            tts_dl: Mutex::new((0, 0, 0, 0)),
            stt_dl: Arc::new(Mutex::new((0, 0, 0, 0))),
            muted: AtomicBool::new(false),
            gate: OnceLock::new(),
        }
    }

    /// Install the shared status-push gate (called once at boot). Lets [`set_muted`]
    /// bump it so a mute change pushes to a blocked `WaitModelStatus` immediately.
    pub fn set_status_gate(&self, gate: Arc<StatusGate>) {
        let _ = self.gate.set(gate);
    }

    /// Whether the running warm child is in full-duplex AEC mode. Callers use it to
    /// bypass the half-duplex `mic_active()` gates — under VPIO the input device is
    /// always live, so `mic_active()` is permanently true and useless as a gate.
    pub fn full_duplex_active(&self) -> bool {
        *self.full_duplex_active.lock().unwrap()
    }

    /// Is the Kokoro (TTS) model currently resident in the warm helper?
    pub fn tts_loaded(&self) -> bool {
        self.tts_loaded.load(Ordering::Relaxed)
    }
    /// Is the Parakeet (STT) model currently resident in the warm helper?
    pub fn stt_loaded(&self) -> bool {
        self.stt_loaded.load(Ordering::Relaxed)
    }
    /// Is the Kokoro (TTS) Core ML model currently DOWNLOADING (clean-install first fetch)?
    pub fn tts_downloading(&self) -> bool {
        self.tts_downloading.load(Ordering::Relaxed)
    }
    /// Is the Parakeet (STT) Core ML model currently DOWNLOADING?
    pub fn stt_downloading(&self) -> bool {
        self.stt_downloading.load(Ordering::Relaxed)
    }
    /// Apple-native Kokoro (TTS) CURRENT-FILE download fraction 0..1 (0 when unknown/idle).
    pub fn tts_dl_progress(&self) -> f64 {
        let (d, t, _, _) = *self.tts_dl.lock().unwrap();
        if t > 0 {
            (d as f64 / t as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
    /// (file_index, file_count) of the in-flight apple-native TTS download — 0/0 when idle.
    pub fn tts_dl_files(&self) -> (u64, u64) {
        let (_, _, i, c) = *self.tts_dl.lock().unwrap();
        (i, c)
    }
    /// Apple-native Parakeet (STT) CURRENT-FILE download fraction 0..1 (0 when unknown/idle).
    pub fn stt_dl_progress(&self) -> f64 {
        let (d, t, _, _) = *self.stt_dl.lock().unwrap();
        if t > 0 {
            (d as f64 / t as f64).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
    /// (file_index, file_count) of the in-flight apple-native STT download — 0/0 when idle.
    pub fn stt_dl_files(&self) -> (u64, u64) {
        let (_, _, i, c) = *self.stt_dl.lock().unwrap();
        (i, c)
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
        *self.provider_pref.lock().unwrap() = which.to_string();
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
    /// we restart unconditionally — gated only on the child running (else the next start loads
    /// whatever is present). Returns whether a restart happened.
    pub(crate) fn reload_models(&self) -> bool {
        if !self.is_running() {
            return false; // stopped → the next start loads whatever is now present
        }
        self.restart_child();
        true
    }

    /// Set whether the warm child should run in full-duplex AEC mode (the engine
    /// passes `full_duplex && Parakeet STT`, see `full_duplex_wanted`). Stores the preference only; the
    /// next (re)start uses it. Pair with [`restart_if_full_duplex_stale`](Self::restart_if_full_duplex_stale)
    /// to apply a change to an already-running child.
    pub fn set_full_duplex_pref(&self, on: bool) {
        *self.full_duplex_pref.lock().unwrap() = on;
    }

    /// Set which local STT backend the warm child should use — the resolved provider token
    /// ("cpu"|"cuda"|"ane"|"system").
    /// Stores the preference only; [`restart_if_full_duplex_stale`](Self::restart_if_full_duplex_stale)
    /// applies a change to an already-running child.
    pub fn set_stt_provider_pref(&self, engine: &str) {
        *self.stt_provider_pref.lock().unwrap() = engine.to_string();
    }

    /// Set whether STT should be preloaded in the warm child (= `helper_uses_stt(cfg)`).
    /// Applied on the next (re)start via the `DONTSPEAK_STT_PRELOAD` env.
    pub fn set_stt_wanted(&self, wanted: bool) {
        *self.stt_wanted.lock().unwrap() = wanted;
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
        let fd_stale =
            *self.full_duplex_pref.lock().unwrap() != *self.full_duplex_active.lock().unwrap();
        let stt_stale =
            *self.stt_provider_pref.lock().unwrap() != *self.stt_provider_active.lock().unwrap();
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
            if ds_config::provider_pref_wants_gpu(which) && ds_model::cuda_runtime_present() {
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

    fn set_error(&self, msg: impl Into<String>) {
        *self.last_error.lock().unwrap() = Some(msg.into());
    }
    fn clear_error(&self) {
        *self.last_error.lock().unwrap() = None;
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
        // Re-check under the lifecycle lock: another thread may have started (or a
        // crashing one may still be tearing down) between the caller's
        // `is_running()` gate and here. Idempotent — never spawn a second child.
        if self.is_running() {
            return;
        }
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--serve")
            .env(
                "DONTSPEAK_PROVIDER",
                self.provider_pref.lock().unwrap().clone(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Helper stderr → a log file (full-duplex status, capture levels,
            // barge-debug, errors) so the warm child is diagnosable; was discarded.
            .stderr(helper_stderr());
        // Full-duplex AEC (macOS VPIO, Parakeet path only): the helper opens the
        // echo-cancelled duplex unit when this env is set. Unset ⇒ half-duplex.
        let full_duplex = *self.full_duplex_pref.lock().unwrap();
        if full_duplex {
            cmd.env("DONTSPEAK_FULL_DUPLEX", "1");
        }
        // Which local STT engine the child should serve ("cpu"|"ane").
        let stt_provider = self.stt_provider_pref.lock().unwrap().clone();
        cmd.env("DONTSPEAK_STT_PROVIDER", &stt_provider);
        // Whether the child should PRELOAD STT in parallel with the TTS load (gates its
        // STT-preload thread). Only when STT is the built-in engine — `stt_provider` alone
        // can't tell, since it resolves to "cpu" even for Off/ClaudeCode.
        if *self.stt_wanted.lock().unwrap() {
            cmd.env("DONTSPEAK_STT_PRELOAD", "1");
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
                    // ONE generic handler drives EVERY engine's download → warm transition (TTS
                    // warms before READY; STT preloads in PARALLEL so its lines can land on either
                    // side of READY — the reader uses the SAME calls). Their terminals differ:
                    // TTS = READY (the loop break above), STT = STTLOADED (below).
                    let gate = self.gate.get().map(|g| g.as_ref());
                    if apply_dl_progress(l, "tts", &self.tts_downloading, &self.tts_dl, gate)
                        || apply_dl_progress(l, "stt", &self.stt_downloading, &self.stt_dl, gate)
                    {
                        continue; // the push is built into apply_dl_progress now
                    }
                    if l == "STTLOADED" {
                        mark_loaded(&self.stt_downloading, &self.stt_dl, &self.stt_loaded, gate);
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
        *self.child.lock().unwrap() = Some(child);
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
            let stt_loaded = self.stt_loaded.clone();
            let stt_downloading = self.stt_downloading.clone();
            let stt_dl = self.stt_dl.clone();
            // STT preloads on a PARALLEL thread, so its `STT_PROVIDER` line often lands AFTER READY
            // (and always for a lazy `load stt`) — i.e. in THIS persistent reader, not start()'s
            // pre-READY wait loop. Clone the realized-provider slot in so the reader can capture it;
            // without this the STT status row stays "CPU" while STT actually ran on the GPU.
            let stt_realized = self.stt_realized.clone();
            // The status push-gate, so STT download progress in the reader pushes LIVE (like the
            // TTS path in start()'s wait loop) instead of waiting for the poll — otherwise the
            // Parakeet % sticks between polls then jumps when a file completes.
            let gate = self.gate.get().cloned();
            std::thread::spawn(move || {
                Self::reader_loop(
                    stdout,
                    speak_slot,
                    listen_slot,
                    diarize_slot,
                    enroll_slot,
                    stats,
                    stt_stats,
                    lifetime,
                    stt_loaded,
                    stt_downloading,
                    stt_dl,
                    stt_realized,
                    gate,
                );
            })
        };
        *self.reader.lock().unwrap() = Some(handle);
        // Record what this child was started with, so a later pref change restarts.
        *self.full_duplex_active.lock().unwrap() = full_duplex;
        *self.stt_provider_active.lock().unwrap() = stt_provider;
        // Kokoro is eager-loaded by the helper before READY. STT (Parakeet) now preloads in
        // PARALLEL and reports its own STTLOADED (possibly BEFORE this READY), so we must NOT
        // reset stt_loaded here — it's initialized before the wait loop and set by the STT
        // signal handlers. READY means the TTS download (if any) is done.
        mark_loaded(
            &self.tts_downloading,
            &self.tts_dl,
            &self.tts_loaded,
            self.gate.get().map(|g| g.as_ref()),
        );
        log("TTS warm Kokoro child READY");
    }

    /// Kill + reap the warm child, freeing the model. Safe to call when stopped.
    fn stop_child(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        // Toggled off ⇒ not a failure; clear any stale start error.
        self.clear_error();
        // Drop stdin first so the child sees EOF, then hard-kill to be sure.
        *self.stdin.lock().unwrap() = None;
        // The process is gone → both models go with it. Clear the downloading flags too, so a
        // child killed mid-fetch doesn't freeze the dot at "downloading" across the next start.
        self.tts_loaded.store(false, Ordering::Relaxed);
        self.stt_loaded.store(false, Ordering::Relaxed);
        self.tts_downloading.store(false, Ordering::Relaxed);
        self.stt_downloading.store(false, Ordering::Relaxed);
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

    /// Mark the child as dead after an IO error so the next speak restarts it.
    fn mark_dead(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        *self.stdin.lock().unwrap() = None;
        // A dead child holds no models and is mid-nothing — clear every residency/progress
        // flag so the dot doesn't show a stale "running"/"downloading" until the next start.
        self.tts_loaded.store(false, Ordering::Relaxed);
        self.stt_loaded.store(false, Ordering::Relaxed);
        self.tts_downloading.store(false, Ordering::Relaxed);
        self.stt_downloading.store(false, Ordering::Relaxed);
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.join_reader();
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
    #[allow(clippy::too_many_arguments)]
    fn reader_loop(
        mut stdout: BufReader<ChildStdout>,
        speak_slot: Arc<(Mutex<SpeakSlot>, Condvar)>,
        listen_slot: Arc<(Mutex<ListenSlot>, Condvar)>,
        diarize_slot: Arc<(Mutex<DiarizeSlot>, Condvar)>,
        enroll_slot: Arc<(Mutex<EnrollSlot>, Condvar)>,
        stats: Arc<crate::stats::TtsStats>,
        stt_stats: Arc<crate::stats::SttStats>,
        lifetime: Arc<crate::stats::LifetimeSeconds>,
        stt_loaded: Arc<AtomicBool>,
        stt_downloading: Arc<AtomicBool>,
        stt_dl: Arc<Mutex<(u64, u64, u64, u64)>>,
        stt_realized: Arc<Mutex<String>>,
        gate: Option<Arc<StatusGate>>,
    ) {
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
                    // STT lifecycle — the SAME generic handler `start()`'s wait loop uses, so the
                    // pre-/post-READY paths can't drift (STT preloads in parallel → lines land on
                    // either side of READY). `DOWNLOADING stt`/`WARMING stt` here; STTLOADED next.
                    } else if apply_dl_progress(
                        l,
                        "stt",
                        &stt_downloading,
                        &stt_dl,
                        gate.as_deref(),
                    ) {
                        // The live push is built into apply_dl_progress (same as the wait loop).
                    } else if l == "STTLOADED" {
                        mark_loaded(&stt_downloading, &stt_dl, &stt_loaded, gate.as_deref());
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
            match engine {
                "tts" => self.tts_loaded.store(false, Ordering::Relaxed),
                "stt" => self.stt_loaded.store(false, Ordering::Relaxed),
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
            // TTS lights optimistically (no warmup pass). STT does NOT — it waits for the
            // helper's `STTLOADED` confirmation (after preload + graph warmup), so the dot
            // shows "warming" until the model is truly resident AND warm.
            if engine == "tts" {
                self.tts_loaded.store(true, Ordering::Relaxed);
            }
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
        // The helper lazily (re)loads Kokoro to serve this — it's resident now.
        self.tts_loaded.store(true, Ordering::Relaxed);

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
            // EOF/read-error ⇒ the child died: reap it so the next speak restarts.
            // A soft `ERR` line (child alive) just fails this one utterance.
            if fatal {
                self.mark_dead();
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
        // The helper lazily loads Parakeet on first listen — it's resident now.
        self.stt_loaded.store(true, Ordering::Relaxed);

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
    use super::{apply_dl_progress, mark_loaded};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn apply_dl_progress_is_kind_scoped_and_parses_per_file() {
        let dling = AtomicBool::new(false);
        let dl = Mutex::new((0u64, 0, 0, 0));
        // A "stt" line is IGNORED by the "tts" handler (kind scoping — no cross-talk).
        assert!(!apply_dl_progress(
            "DOWNLOADING stt 1 2 3 4",
            "tts",
            &dling,
            &dl,
            None
        ));
        assert!(!dling.load(Ordering::Relaxed));
        // The matching kind sets downloading + the per-file (done, total, index, count).
        assert!(apply_dl_progress(
            "DOWNLOADING tts 10 100 3 22",
            "tts",
            &dling,
            &dl,
            None
        ));
        assert!(dling.load(Ordering::Relaxed));
        assert_eq!(*dl.lock().unwrap(), (10, 100, 3, 22));
        // WARMING <kind> clears downloading + progress (→ the dot reads "Starting…").
        assert!(apply_dl_progress("WARMING tts", "tts", &dling, &dl, None));
        assert!(!dling.load(Ordering::Relaxed));
        assert_eq!(*dl.lock().unwrap(), (0, 0, 0, 0));
        // The bare signal (no payload) still flips downloading.
        assert!(apply_dl_progress(
            "DOWNLOADING tts",
            "tts",
            &dling,
            &dl,
            None
        ));
        assert!(dling.load(Ordering::Relaxed));
        // A non-lifecycle line is not consumed (so PROVIDER/ERR/READY fall through).
        assert!(!apply_dl_progress(
            "PROVIDER CoreML",
            "tts",
            &dling,
            &dl,
            None
        ));
        assert!(!apply_dl_progress("READY", "tts", &dling, &dl, None));
    }

    #[test]
    fn apply_dl_progress_drives_stt_identically() {
        let dling = AtomicBool::new(false);
        let dl = Mutex::new((0u64, 0, 0, 0));
        assert!(apply_dl_progress(
            "DOWNLOADING stt 5 50 1 4",
            "stt",
            &dling,
            &dl,
            None
        ));
        assert_eq!(*dl.lock().unwrap(), (5, 50, 1, 4));
        assert!(apply_dl_progress("WARMING stt", "stt", &dling, &dl, None));
        assert!(!dling.load(Ordering::Relaxed));
        // The "tts" twin is ignored by the "stt" handler — same generic code, kind-scoped.
        assert!(!apply_dl_progress("WARMING tts", "stt", &dling, &dl, None));
    }

    #[test]
    fn mark_loaded_greens_and_clears_progress() {
        let dling = AtomicBool::new(true);
        let dl = Mutex::new((10u64, 100, 2, 5));
        let loaded = AtomicBool::new(false);
        mark_loaded(&dling, &dl, &loaded, None);
        assert!(!dling.load(Ordering::Relaxed));
        assert_eq!(*dl.lock().unwrap(), (0, 0, 0, 0));
        assert!(loaded.load(Ordering::Relaxed));
    }
}
