//! Engine-owned TTS queue — the single serializer for all speech.
//!
//! Producers (the `Speak` / `SpeakNarration` RPC handlers) enqueue whole blocks onto ONE
//! bounded plain FIFO — there is no "reply vs narration" kind; what gets spoken is decided
//! upstream by the `narrate` setting. ONE worker thread plays them in order on the
//! WARM child (`TtsManager`), so there is no per-block model reload. The warm child stays
//! DUMB: ordering, the mic feedback gate, and the barge/pause/resume policy all live here.
//!
//! Playback granularity: each block is sent to the warm child as ONE `tts.speak`. The
//! child runs the shared Rust text-to-phoneme frontend once, transactionally prepares the
//! same bounded phoneme batches through ONNX or Core ML, then begins playback — so there
//! is no per-block reload or backend-specific pronunciation path here.
//!
//! Focus gate (cross-platform, only when the `pause_in_background` config is set):
//! the engine poll thread publishes whether a terminal is frontmost via
//! `set_terminal_front`; the worker HOLDS the whole queue (nothing dropped) while no
//! terminal is frontmost — so narration pauses when you tab to a browser and resumes
//! when you return. Self-arming: the gate only engages after a terminal has been seen
//! frontmost once, so an unrecognized terminal emulator degrades to always-play
//! rather than going mute.
//!
//! Record barge (mic goes active, HALF-DUPLEX only): the whole queue PAUSES —
//! every item is kept, nothing is dropped — and resumes once the mic
//! frees. The interrupted item is re-spoken from its top (a block plays continuously,
//! so there is no mid-block offset to resume from). Full-duplex never pauses: the
//! AEC mic stays open and you dictate over the reply.
//!
//! A hard barge (StopSpeech / long-press reset) still CLEARS the whole queue.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ds_config::{Paths, VoiceConfig};

use crate::status::StatusGate;
use crate::tts::TtsManager;

/// Map a session id (`None` = the default/global session) to its override-map key.
fn vkey(session: &Option<String>) -> String {
    session.clone().unwrap_or_default()
}

/// RAII guard that resets the `healing` flag on drop, even if the closure panics.
struct HealingGuard(Arc<AtomicBool>);
impl Drop for HealingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// `None` = no claimed item; `Some(None)` = an untagged/global item; `Some(Some(id))` = a
/// session item. The nested option is required because global speech is still real in-flight
/// work and must remain distinguishable from an idle worker during scoped cancellation.
struct PlayingGuard<'a>(&'a Mutex<Option<Option<String>>>);
impl Drop for PlayingGuard<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = None;
    }
}

struct InFlightGuard<'a>(&'a AtomicBool);
impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Rotates greeting templates so consecutive opens don't repeat the same line.
static GREET_ROTATION: AtomicUsize = AtomicUsize::new(0);

/// Hard cap on `pool_assignments`: a client that never sends `SessionEnd` (a crashed
/// hook, or one hammering `GreetSession`/`Speak` with fresh, client-supplied session
/// strings) must not grow this map for the engine process's lifetime. On overflow the
/// whole map is cleared rather than growing further — `pick_pool_voice` already
/// tolerates a session's pick going stale and being re-derived (see
/// `stale_assignment_is_dropped_when_voice_leaves_the_pool`), so this is safe: a live
/// terminal just gets a (possibly different) pool voice on its NEXT reply.
const POOL_ASSIGNMENTS_MAX: usize = 128;

/// One item may carry at most 10 KiB of UTF-8 text, regardless of whether it arrived through
/// IPC or the in-process Codex subscriber. Keeping the limit here prevents a producer from
/// bypassing it by avoiding the wire protocol.
pub(crate) const MAX_SPEAK_BYTES: usize = 10 * 1024;

/// Bound pending work so a stalled focus/microphone gate cannot retain unlimited narration.
/// The current item is tracked separately as `in_flight`, so total accepted work can be this
/// many queued items plus one being processed.
const MAX_PENDING_ITEMS: usize = 128;
const MAX_PENDING_BYTES: usize = 1024 * 1024;
const MAX_SESSION_PENDING_ITEMS: usize = 32;
const MAX_SESSION_PENDING_BYTES: usize = 256 * 1024;

/// Short, non-obtrusive greeting templates; `{n}` = the voice's display name.
const GREETINGS: &[&str] = &[
    "{n} here — I'm with you today.",
    "Hey, it's {n}. Ready when you are.",
    "{n} speaking. Let's get into it.",
    "{n} here. Good to see you.",
    "{n} with you. Let's go.",
    "Hi, {n} here — what are we building?",
];

/// Name-less variants for when there's no voice to announce — the System engine on the
/// OS-default voice (`tts_system_voice` empty). Same rotation, just without `{n}`.
const GREETINGS_ANON: &[&str] = &[
    "I'm with you today.",
    "Ready when you are.",
    "Let's get into it.",
    "Good to see you.",
    "With you. Let's go.",
    "What are we building?",
];

/// Build the greeting line for the resolved voice `name`: the named template when there's a
/// name to announce, the name-less variant otherwise (System on the OS-default voice when the
/// name can't be resolved). The `name` comes from the ONE shared resolver
/// [`ds_tts::enumerate::voice_display_name`], so Kokoro/System on every OS agree.
fn greeting_line(name: Option<&str>, idx: usize) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => GREETINGS[idx % GREETINGS.len()].replace("{n}", n),
        None => GREETINGS_ANON[idx % GREETINGS_ANON.len()].to_string(),
    }
}

/// PURE pool pick: the voice `session` should get from `pool` given the CURRENT
/// `assignments`. If already assigned, return it. Else pick the first pool voice not
/// taken by another session (so terminals differ); when all are taken, round-robin by
/// assignment count. `pool` must be non-empty.
fn pick_pool_voice(
    assignments: &HashMap<String, String>,
    pool: &[String],
    session: &str,
) -> String {
    // Reuse this session's prior pick ONLY if it's still in the live pool. The pool is
    // `tts_built_in_voices`, which the user can change at runtime (set_config →
    // hot-reload) WITHOUT clearing `pool_assignments`; a cached pick that's no longer
    // configured must NOT linger, or the terminal keeps speaking a voice the user
    // dropped (e.g. greets/replies in the old default `af_sarah` after switching to
    // `af_nicole` — heard as "Sarah introduces herself as Nicole"). When the cached
    // pick is stale we fall through and re-pick from the current pool; `assign_pool_voice`
    // overwrites the stale map entry with this fresh pick, so the heal is permanent.
    if let Some(v) = assignments.get(session)
        && pool.iter().any(|p| p == v)
    {
        return v.clone();
    }
    pool.iter()
        // Skip voices already CLAIMED by another live session (a stale self-entry for
        // `session` isn't in `pool`, so it never blocks here) so terminals stay distinct.
        .find(|v| !assignments.iter().any(|(s, a)| s != session && a == *v))
        .cloned()
        .unwrap_or_else(|| pool[assignments.len() % pool.len()].clone())
}

/// Insert `session → voice` into `assignments`, then clear the whole map if it now
/// exceeds `cap`. Pulled out of `assign_pool_voice` so the "never grows past cap"
/// invariant is unit-testable without a live `TtsQueue`. PURE.
fn record_pool_assignment(
    assignments: &mut HashMap<String, String>,
    session: &str,
    voice: String,
    cap: usize,
) {
    assignments.insert(session.to_string(), voice);
    if assignments.len() > cap {
        assignments.clear();
    }
}

/// One queued unit of speech. The queue is a bounded plain FIFO with no "narration vs reply"
/// kind: whatever the narration layer successfully enqueues is played in order. Items differ
/// only by their optional per-call voice/rate and the session they belong to.
struct Item {
    text: String,
    voice: Option<String>,
    rate: Option<f32>,
    /// The Claude session this item belongs to (`None` = default/global), used to
    /// resolve the per-session voice override at play time AND to gate playback on
    /// the active terminal (the worker plays only the active session's items).
    session: Option<String>,
}

/// Which terminal (session) the worker is allowed to speak for. The portable focus
/// model: there is no per-tab focus API, so "active" is the terminal you last
/// submitted a prompt to (`explicit`, from the `UserPromptSubmit` hook), with a
/// recency fallback (`recent`) for the window before any prompt-hook reports in.
#[derive(Default)]
struct ActiveSel {
    /// The session you last submitted a prompt to (authoritative when set). `None`
    /// until the `MarkActive` RPC fires (e.g. hooks not re-wired yet).
    explicit: Option<String>,
    /// Session of the most-recently enqueued item — the recency fallback, used ONLY
    /// while `explicit` is `None`, so a multi-terminal setup with un-wired hooks
    /// plays the most-recent terminal rather than interleaving all of them.
    recent: Option<String>,
}

impl ActiveSel {
    /// The session the worker should currently speak for: the explicit prompt-target
    /// if known, else the most-recent producer, else `None` (= play FIFO).
    fn effective(&self) -> Option<String> {
        self.explicit.clone().or_else(|| self.recent.clone())
    }
}

/// Drop a single window's items, keeping every OTHER session's (and untagged global)
/// item in place — the per-window barge for `StopSpeech { session }`. Split out so the
/// "keep other terminals' queue" invariant is unit-testable without a live engine. An
/// item is this window's iff its `session` tag equals `target`. PURE.
fn prune_session(q: &mut VecDeque<Item>, target: &Option<String>) {
    q.retain(|it| &it.session != target);
}

/// The inverse of [`prune_session`]: keep ONLY `keep`'s items, dropping every other
/// session's AND every untagged/global item — the `input_clears` `other` scope. An
/// item is kept iff its `session` tag equals `keep`. PURE.
fn retain_only_session(q: &mut VecDeque<Item>, keep: &Option<String>) {
    q.retain(|it| &it.session == keep);
}

/// Index of the first item the worker may play given the active terminal. `None`
/// active → strict FIFO (back-compat single-terminal). `Some(s)` → PREFER the first
/// item tagged `s` OR untagged (`None` = global audio like the MCP `speak` tool); but
/// if the active terminal has NOTHING queued, fall back to plain FIFO so another
/// terminal's reply is never starved forever (the active `explicit` session persists
/// until the next MarkActive, so without this fallback a backgrounded window's reply
/// is held indefinitely — the cross-window "one window goes silent" bug). PURE.
fn select_pos(q: &VecDeque<Item>, active: &Option<String>) -> Option<usize> {
    match active {
        None => (!q.is_empty()).then_some(0),
        Some(_) => q
            .iter()
            .position(|it| it.session.is_none() || &it.session == active)
            // No item for the active terminal → don't starve the others: play FIFO.
            .or_else(|| (!q.is_empty()).then_some(0)),
    }
}

/// Why the queue is currently paused — lets a barge-watcher auto-resume tell its OWN
/// speculative pause apart from a real Caps-gesture pause it must never touch. The two
/// causes are guarded ASYMMETRICALLY on purpose: a `Dictation` pause/resume always
/// applies unconditionally (engine.rs's own gesture always wins), while a
/// `BargeSpeculative` pause/resume is a no-op whenever a `Dictation` pause is (or
/// would be) in effect — see `pause_with_cause` and `resume_if_barge_speculative`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseCause {
    /// A genuine user gesture: `engine.rs`'s `toggle_dictation` PauseVoice arm or
    /// `start_recording`. Never auto-cleared by anything except the matching
    /// `resume()` call from that SAME gesture's stop/ResumeVoice arm.
    Dictation,
    /// `barge.rs`'s own "foreign mic looks live" watcher. The ONLY cause
    /// `resume_if_barge_speculative` will ever auto-clear.
    BargeSpeculative,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PausedState {
    paused: bool,
    cause: Option<PauseCause>,
}

pub struct TtsQueue {
    items: Mutex<VecDeque<Item>>,
    cv: Condvar,
    /// Bumped on every barge/pause; the worker abandons its in-flight item when
    /// the generation moves past the one it dequeued under.
    generation: AtomicU64,
    /// True while paused for a record-barge (resume mode): the worker stops
    /// dequeuing until `resume()`. Tagged with WHY it's paused (`PauseCause`) so a
    /// barge-watcher auto-resume (`resume_if_barge_speculative`) can tell its own
    /// speculative pause apart from a real Caps/PTT `Dictation` pause it must never
    /// touch — and so the PAUSE side of that same guard (`pause_with_cause`) can
    /// refuse to relabel an already-`Dictation`-tagged pause as `BargeSpeculative`
    /// during the `start_recording` race window (`pause_for_record()` fires before
    /// `set_stt_active(true)`). Both sides guarded under ONE lock so the check-and-set
    /// is atomic.
    paused: Mutex<PausedState>,
    /// Per-interruption requeue intent, keyed by the generation value in effect the
    /// MOMENT each cancellation fired (the `fetch_add`'s PRE-bump value — exactly the
    /// `gen0` whatever item was in flight at that instant is running under). `true` = a
    /// record-barge pause (resume this item later via [`resume`](Self::resume)); `false` =
    /// a hard cancel (`clear` / `clear_session` / `cancel_for_submit` / `skip_current`
    /// — drop it). [`requeue_if_resuming`](Self::requeue_if_resuming) looks up ITS OWN
    /// `gen0` here instead of re-reading the CURRENT `paused` flag, so a later, unrelated
    /// record-barge pause can't resurrect an item an explicit `clear_session` already
    /// dropped (and a later resume/clear can't retroactively change the fate of an item
    /// a DIFFERENT, earlier bump already decided). Consumed (removed) on lookup.
    cancel_kind: Mutex<HashMap<u64, bool>>,
    /// True while the worker is actively playing an item (set around playback,
    /// cleared when it returns to waiting). Read-only signal for `Status`.
    tts_active: AtomicBool,
    /// True from dequeue until the item reaches a terminal outcome, except while focus is
    /// the gate holding it. Unlike `tts_active`, this includes readiness and mic-only holds,
    /// closing the half-duplex gap where an item had left the deque but the listener
    /// incorrectly considered the pipeline idle. Focus-held work deliberately reads idle so
    /// always-listening remains usable in another app.
    in_flight: AtomicBool,
    /// Whether a terminal is the frontmost app — published by the engine poll thread
    /// each tick (the worker thread can't call NSWorkspace; it's poll/main-thread
    /// affine). The worker HOLDS the queue while this is false, so narration pauses
    /// when you tab to a browser/other app and resumes when a terminal returns. Init
    /// true: fail-open, never silence before the first sample.
    terminal_front: AtomicBool,
    /// Latches true the first time a terminal IS seen frontmost. The focus gate only
    /// engages once this is set — so a user whose terminal emulator isn't in the shared
    /// terminal table (`ds_platform::KNOWN_TERMINALS`) (frontmost always reads false) is
    /// NEVER silenced; they degrade to today's always-play instead of going mute.
    terminal_seen: AtomicBool,
    /// Config `pause_in_background`: when true, the frontmost focus gate HOLDS the queue
    /// while no terminal is frontmost; when false it's disabled (speech plays regardless of
    /// which app is frontmost). Published by the engine poll thread each tick. Init false =
    /// the shipped default (keep speaking); the first poll tick applies the live config.
    pause_in_background: AtomicBool,
    /// Last config published by the engine reload path. Playback reads this in memory
    /// instead of reparsing config.toml for every queued item.
    config: Mutex<VoiceConfig>,
    /// AUTO voice assignments from the preferred-voices pool, keyed by Claude session id.
    /// Filled lazily — the first reply from a new session claims the next untaken pool
    /// voice, so each terminal speaks with a different voice. In-memory; cleared on engine
    /// restart. The voice itself is a persistent config setting (`tts_built_in_voices`);
    /// this map only records which pool entry each terminal claimed. Bounded
    /// (`POOL_ASSIGNMENTS_MAX`): a client that never sends `SessionEnd` must not grow this
    /// for the daemon's lifetime — see `record_pool_assignment`.
    pool_assignments: Mutex<HashMap<String, String>>,
    /// Which terminal the worker may currently speak for (see [`ActiveSel`]). Read by
    /// the worker at each dequeue; written by the `MarkActive` RPC handler (explicit)
    /// and by every enqueue (recent). One lock, always acquired INSIDE `items`.
    active: Mutex<ActiveSel>,
    /// The `session` tag of the item the worker is currently playing — meaningful
    /// ONLY while `tts_active` is true (set alongside it at playback start, cleared
    /// with it at the end). Lets [`clear_session`](Self::clear_session) decide whether
    /// a per-window stop must cancel the in-flight item (its session matches) or only
    /// prune that window's queued items, leaving another window's playback alone.
    playing_session: Mutex<Option<Option<String>>>,
    tts: Arc<TtsManager>,
    /// The shared status-push gate: a `tts_active` transition bumps it so a blocked
    /// `WaitModelStatus` sees playback start/stop immediately (the flag drives the
    /// menu-bar TTS dot in `model_status`). Routed through [`set_tts_active`].
    gate: Arc<StatusGate>,
    /// When a VOICE submit (Caps dictation / hands-free) last pressed Enter. The
    /// UserPromptSubmit hook fires for EVERY submit, so `MarkActive` consumes this to tell
    /// a voice submit's own auto-Enter apart from a real text submit (the `text`
    /// drop must not fire on a voice submit). See `note_voice_submit` / `take_recent_voice_submit`.
    last_voice_submit: Mutex<Option<Instant>>,
    /// Shared read handle to the single mic-in-use watcher (CoreAudio listener on macOS, poll
    /// thread elsewhere). The worker's focus-hold reads this CACHED state instead of querying
    /// the audio device every 120 ms while holding an item.
    mic: ds_platform::MicState,
    /// Single-flight for [`heal_crashed_child`](Self::heal_crashed_child): true while a
    /// heal thread is in flight, so tap-happy Caps presses during a multi-second heal
    /// don't pile up threads that would all block on the manager's lifecycle lock.
    healing: Arc<AtomicBool>,
}

impl TtsQueue {
    /// Create the queue and spawn its worker thread.
    pub fn start(
        tts: Arc<TtsManager>,
        paths: Paths,
        gate: Arc<StatusGate>,
        mic: ds_platform::MicState,
    ) -> Arc<Self> {
        let config = VoiceConfig::load(&paths);
        let q = Arc::new(Self {
            items: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            generation: AtomicU64::new(0),
            paused: Mutex::new(PausedState::default()),
            cancel_kind: Mutex::new(HashMap::new()),
            tts_active: AtomicBool::new(false),
            in_flight: AtomicBool::new(false),
            terminal_front: AtomicBool::new(true),
            terminal_seen: AtomicBool::new(false),
            pause_in_background: AtomicBool::new(false),
            config: Mutex::new(config),
            pool_assignments: Mutex::new(HashMap::new()),
            active: Mutex::new(ActiveSel::default()),
            last_voice_submit: Mutex::new(None),
            playing_session: Mutex::new(None),
            tts,
            gate,
            mic,
            healing: Arc::new(AtomicBool::new(false)),
        });
        let worker = q.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("ds-ttsq".into())
            .spawn(move || worker.run())
        {
            log::error!(target: "engine", "failed to spawn TTS queue worker: {e}");
        }
        q
    }

    /// Build a queue WITHOUT spawning its worker thread (unlike [`TtsQueue::start`]) — the
    /// "real-enough" double for tests in OTHER modules that need `ttsq: Some(..)` (the
    /// fields here are private, so `engine.rs`'s tests can't build one themselves). The
    /// `TtsManager` points at a nonexistent helper binary and is never started — the same
    /// "safe to call while stopped" stub `tts.rs`'s own `status_gate_tests::mk()` uses; a
    /// fresh manager reports `stt_loaded() == false`, so an engine holding this queue is
    /// exactly ON-but-NOT-READY for `built_in`. The `TempDir` behind `paths` is dropped on
    /// return — fine, nothing here ever reads those paths.
    #[cfg(test)]
    pub(crate) fn test_stub() -> Arc<Self> {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("ds-test-nonexistent-helper");
        Self::test_stub_with_helper(dir.path(), helper)
    }

    /// [`test_stub`](Self::test_stub) with an explicit helper binary — for the readiness-gate
    /// tests that need a REAL (fake-helper) child so `is_running()` turns true. `root` backs
    /// `paths` and the lifetime file; keep its TempDir alive in the test when the helper will
    /// actually be spawned (stderr-log override dirs live there too).
    #[cfg(test)]
    pub(crate) fn test_stub_with_helper(
        root: &std::path::Path,
        helper: std::path::PathBuf,
    ) -> Arc<Self> {
        let paths = Paths::rooted_at(root);
        let tts = Arc::new(TtsManager::new(
            helper,
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                root.join("ds-ttsq-test-lifetime.json"),
            )),
        ));
        // A real (briefly-lived) mic watcher: `MicState` has no other constructor. Dropped
        // immediately — the handle just freezes at its last (safe) reading.
        let mic = ds_platform::MicWatcher::spawn(|_| {}).handle();
        Arc::new(TtsQueue {
            items: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            generation: AtomicU64::new(0),
            paused: Mutex::new(PausedState::default()),
            cancel_kind: Mutex::new(HashMap::new()),
            tts_active: AtomicBool::new(false),
            in_flight: AtomicBool::new(false),
            terminal_front: AtomicBool::new(true),
            terminal_seen: AtomicBool::new(false),
            pause_in_background: AtomicBool::new(false),
            config: Mutex::new(VoiceConfig::load(&paths)),
            pool_assignments: Mutex::new(HashMap::new()),
            active: Mutex::new(ActiveSel::default()),
            last_voice_submit: Mutex::new(None),
            playing_session: Mutex::new(None),
            tts,
            gate: StatusGate::new(),
            mic,
            healing: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Force `tts_active` directly, bypassing the real playback pipeline — lets
    /// `engine.rs`'s own test module simulate "TTS is speaking" (to drive
    /// `Engine::handle_tap`'s defer-while-speaking path) on a [`test_stub`], which
    /// deliberately never spawns the worker that would flip this for real. Test-only.
    #[cfg(test)]
    pub(crate) fn set_active_for_test(&self, on: bool) {
        self.tts_active.store(on, Ordering::SeqCst);
    }

    /// Enqueue one bounded unit of speech onto the FIFO. Empty text is ignored. Callers
    /// (explicit `speak`, the greeting, and mid-turn narration) all land here and are played
    /// in order. `voice`/`rate` are optional per-call overrides (narration passes `None` for
    /// both → the session/config voice at play time).
    pub fn enqueue(
        &self,
        text: String,
        voice: Option<String>,
        rate: Option<f32>,
        session: Option<String>,
    ) -> Result<(), String> {
        if text.len() > MAX_SPEAK_BYTES {
            return Err(format!("text exceeds the {MAX_SPEAK_BYTES}-byte limit"));
        }
        if text.trim().is_empty() {
            return Ok(());
        }
        let mut q = self.items.lock().unwrap();
        if q.len() >= MAX_PENDING_ITEMS {
            return Err(format!(
                "speech queue is full ({MAX_PENDING_ITEMS} pending items)"
            ));
        }
        let pending_bytes = q
            .iter()
            .try_fold(0usize, |total, item| total.checked_add(item.text.len()));
        if !matches!(
            pending_bytes.and_then(|n| n.checked_add(text.len())),
            Some(total) if total <= MAX_PENDING_BYTES
        ) {
            return Err(format!(
                "speech queue is full ({MAX_PENDING_BYTES} pending text bytes)"
            ));
        }
        let mut session_items = 0usize;
        let mut session_bytes = 0usize;
        for item in q.iter().filter(|item| item.session == session) {
            session_items += 1;
            session_bytes = session_bytes.saturating_add(item.text.len());
        }
        if session_items >= MAX_SESSION_PENDING_ITEMS {
            return Err(format!(
                "session speech queue is full ({MAX_SESSION_PENDING_ITEMS} pending items)"
            ));
        }
        if !matches!(
            session_bytes.checked_add(text.len()),
            Some(total) if total <= MAX_SESSION_PENDING_BYTES
        ) {
            return Err(format!(
                "session speech queue is full ({MAX_SESSION_PENDING_BYTES} pending text bytes)"
            ));
        }
        self.note_recent(&session);
        q.push_back(Item {
            text,
            voice,
            rate,
            session,
        });
        self.cv.notify_one();
        Ok(())
    }

    /// Claim a selected item while the caller holds `items`. The generation snapshot and
    /// playback-session publication must stay inside that lock so global and per-session clears
    /// either remove the queued item first or cancel the fully published in-flight item.
    fn claim_item(&self, q: &mut VecDeque<Item>, pos: usize) -> (Item, u64) {
        let gen0 = self.generation.load(Ordering::SeqCst);
        let item = q.remove(pos).expect("select_pos returns a valid index");
        self.in_flight.store(true, Ordering::SeqCst);
        *self.playing_session.lock().unwrap() = Some(item.session.clone());
        (item, gen0)
    }

    /// Record the session of the most-recently enqueued item (the recency fallback
    /// for active-terminal selection). MUST be called with the `items` lock held, so
    /// the lock order stays `items` → `active` everywhere.
    fn note_recent(&self, session: &Option<String>) {
        self.active.lock().unwrap().recent = session.clone();
    }

    /// Set global mute on the warm child (delegates to the [`TtsManager`]). Silences playback
    /// without stopping it — the queue keeps draining.
    pub fn set_muted(&self, on: bool) {
        self.tts.set_muted(on);
    }

    /// The 4-statement "hard cancel the in-flight item" sequence shared by every barge
    /// that unconditionally cancels: reflect the stop immediately (`Status` never sees
    /// stale `tts_active=true` mid-unwind), abandon the worker's current item via a
    /// generation bump, record it as a drop (never requeue, regardless of `paused`),
    /// and fade the helper's audio out (not an instant cut). Does NOT touch `items` or
    /// `paused` — callers prune/retain the queue themselves first. `skip_current` does
    /// NOT use this: it deliberately skips the `set_tts_active` toggle (see its own doc).
    fn hard_cancel_in_flight(&self) {
        self.set_tts_active(false);
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        self.record_cancel_kind(gen0, false);
        self.tts.stop_fade();
    }

    /// Global hard barge (caps long-press reset / `StopSpeech{None}`): drop everything
    /// pending, cancel whatever is playing, and clear any pause. The audio is faded out
    /// over the short window (not an instant cut) so even this "stop everything" gesture
    /// tapers instead of clicking.
    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
        *self.paused.lock().unwrap() = PausedState::default();
        self.hard_cancel_in_flight();
        self.cv.notify_one();
    }

    /// Skip the CURRENTLY-playing item and advance to the NEXT queued one — the caps
    /// DOUBLE-TAP gesture. Unlike [`clear`](Self::clear) (the long-press "stop everything"),
    /// the rest of the queue is KEPT: bumping the generation makes the worker abandon its
    /// in-flight item, then it dequeues the next and plays it (or goes idle if none remain).
    /// A no-op when nothing is playing (the engine only calls this while `is_tts_active`).
    pub fn skip_current(&self) {
        // NB: do NOT clear `items` and do NOT touch `paused`/`tts_active` — the worker
        // re-asserts `tts_active` when it dequeues the next item (or clears it if the queue
        // is now empty). Fade the current audio out (no click), then wake the worker.
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        // A skip drops the CURRENT item on purpose (it's not coming back) — never requeue it.
        self.record_cancel_kind(gen0, false);
        self.tts.stop_fade();
        self.cv.notify_one();
    }

    /// Per-window barge (a `StopSpeech { session }` from one terminal — its new-reply
    /// preempt or its SessionEnd close): drop only THIS session's queued items and
    /// cancel the in-flight item ONLY if it belongs to this session. Another window's
    /// queue and playback are untouched — the fix for the old global `clear()` that, in
    /// the multi-window/one-voice-per-window model, silenced every terminal at once.
    /// `StopSpeech { session: None }` still routes to [`clear`](Self::clear).
    pub fn clear_session(&self, session: Option<String>) {
        // Keep pruning and the in-flight identity snapshot under the same `items →
        // playing_session` lock order as worker selection. The worker therefore cannot remove
        // a target item between these operations and escape the session-scoped cancellation.
        let cancel_current = {
            let mut items = self.items.lock().unwrap();
            prune_session(&mut items, &session);
            self.playing_session.lock().unwrap().as_ref() == Some(&session)
        };
        if cancel_current {
            // Per-window barge: fade the in-flight item out (short window) so a clear-on-
            // submit / window close / newest-reply preempt tapers off instead of clicking.
            // Every user-facing barge fades now (global + record-barge included); only the
            // helper's internal block-to-block preempt stays an instant cut.
            self.hard_cancel_in_flight();
        }
        // Wake the worker so a held item for this (now-pruned) session re-evaluates,
        // and so the active terminal's next item starts promptly after a cancel.
        self.cv.notify_one();
    }

    /// Snapshot of "who is active" right now — the SAME resolution
    /// [`cancel_for_submit`](Self::cancel_for_submit) needs. Exposed so a caller applying
    /// MULTIPLE `input_clears` scopes for the SAME submit resolves it ONCE up front and
    /// passes the result along, rather than each scope re-reading `active` independently
    /// (which could observe a DIFFERENT session if a concurrent submit from another
    /// terminal's `MarkActive` lands in between two separate calls).
    pub fn active_session(&self) -> Option<String> {
        self.active.lock().unwrap().effective()
    }

    /// Apply `input_clears` for ONE submit, against a single already-resolved `target`
    /// session (see [`active_session`](Self::active_session)) — atomic w.r.t. which
    /// session is "current", so the `current` and `other` scopes can never disagree
    /// about who that is even when both are configured. A no-op when `target` is `None`
    /// (never guess-wipe or guess-spare audio when the caller doesn't know who's
    /// submitting) or when neither scope is requested.
    ///
    /// `current` prunes `target`'s own queued items and cancels its in-flight item
    /// UNCONDITIONALLY — the worker only ever plays the active session's (or untagged)
    /// audio, so `target` IS what the helper is emitting; gating on `tts_active`/
    /// `playing_session` would let a submit that landed in a record-barge transition
    /// (flags briefly stale) leak several blocks before stopping. `other` keeps ONLY
    /// `target`'s queued items — dropping every other session's AND untagged/global
    /// audio — and cancels the in-flight item ONLY when it does NOT belong to `target`
    /// (the worker can legitimately already be playing `target`'s own item, which must
    /// be left alone). Running both in one call composes correctly: `current` first
    /// drops `target`'s own items, then `other` retains only `target`'s items (now
    /// none), so the combination empties the queue entirely, as a user configuring both
    /// scopes would expect.
    pub fn cancel_for_submit(
        &self,
        target: Option<String>,
        cancel_current: bool,
        cancel_other: bool,
    ) {
        if !cancel_current && !cancel_other {
            return;
        }
        let Some(target) = target else {
            return;
        };
        if cancel_current {
            prune_session(&mut self.items.lock().unwrap(), &Some(target.clone()));
            self.hard_cancel_in_flight();
        }
        if cancel_other {
            // Couple queue pruning to the in-flight identity snapshot. Reading the session
            // before taking `items` allowed the worker to claim another session in between and
            // start it after this submit returned.
            let playing_is_other = {
                let mut items = self.items.lock().unwrap();
                let playing_is_other = self
                    .playing_session
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|session| session.as_deref() != Some(target.as_str()));
                retain_only_session(&mut items, &Some(target));
                playing_is_other
            };
            if playing_is_other {
                self.hard_cancel_in_flight();
            }
        }
        self.cv.notify_one();
    }

    /// Mark that a VOICE submit just pressed Enter (Caps dictation / hands-free). Called on
    /// every voice submit regardless of `input_clears`, so the `MarkActive` path can
    /// de-dup the voice submit's own auto-Enter from a genuinely separate text submit.
    pub fn note_voice_submit(&self) {
        *self.last_voice_submit.lock().unwrap() = Some(Instant::now());
    }

    /// Consume the voice-submit mark: true iff a voice submit happened in the last ~3s — i.e.
    /// the UserPromptSubmit hook now firing is that voice submit's echo, NOT a text submit.
    pub fn take_recent_voice_submit(&self) -> bool {
        let mut g = self.last_voice_submit.lock().unwrap();
        let recent = voice_submit_recent(*g, Instant::now());
        if recent {
            *g = None;
        }
        recent
    }

    /// Shared body of `pause_for_record` / `pause_for_suspected_barge`: mark the queue
    /// paused under `cause`, abandon the in-flight item into resume mode (generation
    /// bump + resume intent), and fade the audio out. GUARD: a `BargeSpeculative`
    /// request is a no-op — paused state and cause both left exactly as they were — if
    /// the queue is ALREADY paused for `Dictation`. That closes the round-4 race:
    /// `start_recording` calls `pause_for_record()` (tags `Dictation`) roughly 40 lines
    /// before it calls `set_stt_active(true)`; in that window a genuinely-foreign mic
    /// edge can fire the barge watcher, and it must not relabel (or touch at all) a
    /// pause that's already correctly held for the real reason — the queue is already
    /// paused, so the watcher's own pause action would be redundant even if it weren't
    /// actively harmful (a later `resume_if_barge_speculative()` would otherwise
    /// wrongly clear it). A `Dictation` request (`cause == Dictation`) is NEVER
    /// guarded — engine.rs's own gesture always wins and applies unconditionally, same
    /// as before this change.
    fn pause_with_cause(&self, cause: PauseCause) {
        {
            let mut st = self.paused.lock().unwrap();
            if cause == PauseCause::BargeSpeculative && st.cause == Some(PauseCause::Dictation) {
                return;
            }
            st.paused = true;
            st.cause = Some(cause);
        }
        // Nothing is audibly playing while paused for the record-barge; the kept
        // reply resumes on `resume()` (which re-enters the worker and re-sets it).
        self.set_tts_active(false);
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        // A record-barge pause means RESUME later — pin that intent to this specific
        // transition (see `record_cancel_kind`) so a later, unrelated hard cancel of a
        // DIFFERENT item can't be misread as this pause's outcome, or vice versa.
        self.record_cancel_kind(gen0, true);
        // Fade out (short) rather than hard-cut when you press caps to dictate, so the
        // voice tapers as recording starts. ~60 ms keeps mic bleed minimal in half-duplex
        // (full-duplex stands this watcher down entirely, so it never reaches here).
        self.tts.stop_fade();
    }

    /// Record barge (a genuine Caps/PTT gesture — `engine.rs`'s `toggle_dictation`
    /// PauseVoice arm and `start_recording`, UNCHANGED call sites/behavior): pause and
    /// tag `Dictation`. Always applies, never guarded. Keeps the ENTIRE queue
    /// (narration and reply); the worker re-enqueues the interrupted item on its
    /// generation bump, so `resume()` continues the whole queue from where the mic
    /// interrupted it.
    pub fn pause_for_record(&self) {
        self.pause_with_cause(PauseCause::Dictation);
    }

    /// Speculative record barge (`barge.rs`'s own foreign-mic watcher, NEW entry point
    /// replacing its direct `pause_for_record()` call): pause and tag `BargeSpeculative`
    /// — UNLESS the queue is already paused for a real `Dictation` gesture, in which case
    /// this is a no-op (see `pause_with_cause`'s guard doc for why).
    pub fn pause_for_suspected_barge(&self) {
        self.pause_with_cause(PauseCause::BargeSpeculative);
    }

    /// Whether the warm child is running in full-duplex AEC mode (delegates to the
    /// `TtsManager`). The mic-barge watcher reads it to stand down: in full-duplex
    /// the input device is always live (VPIO), so the `is_mic_active()` edge is useless,
    /// and the user cancels the voice via the Caps long-press instead.
    pub fn is_full_duplex(&self) -> bool {
        self.tts.is_full_duplex_active()
    }

    /// One-shot speaker diarization on the warm helper (delegates to the `TtsManager`):
    /// record `seconds` of mic, then return the `{"segments":[…]}` JSON. Blocks the
    /// caller until the helper's terminal marker. Mutually exclusive with speak/listen.
    pub(crate) fn diarize(&self, seconds: u64) -> std::io::Result<String> {
        self.tts.diarize(seconds)
    }

    /// One-shot voiceprint enrollment on the warm helper (delegates to the `TtsManager`):
    /// record `seconds` of mic, then return the extracted embedding for the engine to
    /// persist under a name. Blocks the caller until the helper's terminal marker.
    pub(crate) fn enroll(&self, seconds: u64) -> std::io::Result<Vec<f32>> {
        self.tts.enroll(seconds)
    }

    /// Mark the active terminal — the session you last submitted a prompt to
    /// (`UserPromptSubmit` hook → `MarkActive`). The worker then speaks only this
    /// session's items (plus untagged global audio) and HOLDS the rest until they
    /// become active. Takes the `items` lock around the update so the worker — which
    /// releases that lock only inside `cv.wait` — can never miss the wake (no lost
    /// wakeup); lock order stays `items` → `active`.
    pub fn set_active_session(&self, session: Option<String>) {
        let _q = self.items.lock().unwrap();
        self.active.lock().unwrap().explicit = session;
        self.cv.notify_one();
    }

    /// Publish whether a terminal is the frontmost app (engine poll thread → worker).
    /// Latches `terminal_seen` the first time a terminal is seen, so the focus gate
    /// self-disables for unrecognized terminal emulators (frontmost never true → the
    /// queue is never silenced). Cheap; called every poll tick.
    pub fn set_terminal_front(&self, front: bool) {
        if front {
            self.terminal_seen.store(true, Ordering::SeqCst);
        }
        self.terminal_front.store(front, Ordering::SeqCst);
    }

    /// Publish the `pause_in_background` config (engine poll thread → worker). When
    /// false, the worker's focus gate is disabled — speech plays regardless of which app
    /// is frontmost. Cheap; called every poll tick alongside `set_terminal_front`.
    pub fn set_pause_in_background(&self, pause: bool) {
        self.pause_in_background.store(pause, Ordering::SeqCst);
    }

    pub(crate) fn set_config(&self, config: VoiceConfig) {
        *self.config.lock().unwrap() = config;
    }

    /// Mic freed via a genuine Caps/PTT gesture (`engine.rs`'s `toggle_dictation`
    /// ResumeVoice arm and `stop_recording`, UNCHANGED call sites): unconditionally lift
    /// the pause regardless of cause — this IS the matching counterpart of whichever
    /// gesture paused it. No-op when not paused.
    pub fn resume(&self) {
        let notify = {
            let mut st = self.paused.lock().unwrap();
            if st.paused {
                *st = PausedState::default();
                true
            } else {
                false
            }
        };
        if notify {
            let _items = self.items.lock().unwrap();
            self.cv.notify_one();
        }
    }

    /// `barge.rs`'s own resume (NEW entry point replacing its direct `resume()` call):
    /// clears the pause ONLY when it's tagged `BargeSpeculative` — a `Dictation`-tagged
    /// pause (or no pause) is left completely untouched, no matter when this is called.
    /// This is the round-3 half of the fix (approved); `pause_for_suspected_barge`'s
    /// guard above is the round-4 half that makes it actually safe, by ensuring the tag
    /// can never have been wrongly stomped in the first place.
    pub fn resume_if_barge_speculative(&self) {
        let notify = {
            let mut st = self.paused.lock().unwrap();
            if st.cause == Some(PauseCause::BargeSpeculative) {
                *st = PausedState::default();
                true
            } else {
                false
            }
        };
        if notify {
            let _items = self.items.lock().unwrap();
            self.cv.notify_one();
        }
    }

    /// Read-only playback snapshot: `(tts_active, queued, paused, muted)`.
    /// `queued` counts items still waiting in the deque (excludes the one being played);
    /// `muted` is the global mute (output plays silently while set).
    pub fn snapshot(&self) -> (bool, usize, bool, bool) {
        let queued = self.items.lock().unwrap().len();
        (
            self.tts_active.load(Ordering::SeqCst),
            queued,
            self.paused.lock().unwrap().paused,
            self.tts.is_muted(),
        )
    }

    /// Cheap, lock-free read of the live playback flag — true while audio is
    /// actually playing. For the model-status JSON's `running.tts_active` (polled
    /// often to drive the menu-bar icon), so it must NOT take the `items` lock the
    /// way `snapshot()` does.
    pub fn is_tts_active(&self) -> bool {
        self.tts_active.load(Ordering::SeqCst)
    }

    /// Set the live playback flag and, on a real transition, bump the status-push gate
    /// so a blocked `WaitModelStatus` sees playback start/stop immediately. The single
    /// writer for `tts_active` — every barge/dequeue routes through here so the push
    /// fires exactly once per change (no spurious bump when it's already in that state).
    fn set_tts_active(&self, on: bool) {
        if self.tts_active.swap(on, Ordering::SeqCst) != on {
            self.gate.bump();
        }
    }

    /// Cheap read of the live pause flag (cause-agnostic) — used by the worker's
    /// dequeue loop, by `requeue_if_resuming`'s defensive fallback, and by `engine.rs`'s
    /// refused-start regression test (hence `pub(crate)`).
    pub(crate) fn is_paused(&self) -> bool {
        self.paused.lock().unwrap().paused
    }

    /// Record the requeue intent for the generation transition a cancellation JUST made:
    /// `gen0` is the PRE-bump value (`fetch_add`'s return), i.e. exactly the `gen0` whatever
    /// item was in flight at that instant is running under — so
    /// [`requeue_if_resuming`](Self::requeue_if_resuming) can later look up THIS bump's
    /// intent by that same key, instead of re-reading the CURRENT (possibly since-changed)
    /// `paused` flag. See the `cancel_kind` field doc for why that distinction matters.
    /// Bounded defensively: bumps are rare user-triggered events, but if a burst of them is
    /// never claimed (nothing was playing to consume them), don't grow forever.
    fn record_cancel_kind(&self, gen0: u64, requeue: bool) {
        let mut m = self.cancel_kind.lock().unwrap();
        m.insert(gen0, requeue);
        if m.len() > 32
            && let Some(oldest) = m.keys().copied().min()
        {
            m.remove(&oldest);
        }
    }

    /// Whether the queue should close always-listening's microphone now.
    ///
    /// A focus hold deliberately reads idle so dictation can continue while the user is in
    /// another app. Otherwise the `items` lock couples this read to the worker's dequeue +
    /// `in_flight` store: sampling the flag before taking the lock left a false-idle
    /// interleaving even though the writer published it under that same lock.
    pub fn is_busy(&self) -> bool {
        if self.worker_focus_hold() && !self.tts_active.load(Ordering::SeqCst) {
            return false;
        }
        let items = self.items.lock().unwrap();
        self.tts_active.load(Ordering::SeqCst)
            || self.in_flight.load(Ordering::SeqCst)
            || !items.is_empty()
    }

    /// Whether the warm helper's Parakeet (STT) model is resident + warm — the dictation
    /// start-guard reads this through the queue (it owns the `TtsManager`).
    pub fn stt_loaded(&self) -> bool {
        self.tts.is_stt_loaded()
    }

    /// Non-blocking crash heal, shared by TWO callers that must never ride a blocking
    /// start (a start holds the manager's `lifecycle` lock for seconds — up to the READY
    /// handshake bound — while the model loads):
    ///
    /// * the DICTATION side: a Caps tap that finds Parakeet not resident may be looking
    ///   at a warm child that CRASHED post-READY — and a user who only dictates never
    ///   queues the speak that would heal it, so dictation would stay refused until an
    ///   app restart. The tap lives on the input poll tick, which must not stall; the
    ///   refusing tap stays refused — the NEXT one finds the model warm.
    /// * the worker's readiness wait (`wait_until_ready_with_timeout`): while an item is
    ///   claimed the mic is closed and `stop_speech` polls the same loop, so the wait
    ///   must keep ticking instead of blocking inside a child restart (issue #59).
    ///
    /// Runs [`TtsManager::restart_if_crashed`] on a throwaway thread.
    pub fn heal_crashed_child(&self) {
        // Single-flight: taps during a heal-in-progress (a start blocks for seconds while
        // the model loads) must not pile up threads on the manager's lifecycle lock.
        if self
            .healing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let tts = self.tts.clone();
        let healing = self.healing.clone();
        std::thread::spawn(move || {
            let _guard = HealingGuard(healing);
            tts.restart_if_crashed();
        });
    }

    /// SessionEnd (window closed for good): per-window barge like [`clear_session`], then
    /// FORGET this session's preferred-pool assignment so `pool_assignments` doesn't
    /// accumulate one entry per distinct session for the daemon's lifetime (it was
    /// previously only reclaimed on engine restart). Called with `Some` session; the
    /// `None`/global case routes to [`clear`](Self::clear) at the IPC site (nothing
    /// session-scoped to forget).
    pub fn end_session(&self, session: Option<String>) {
        self.clear_session(session.clone());
        if let Some(s) = &session {
            self.pool_assignments.lock().unwrap().remove(s);
        }
    }

    /// Get-or-assign this session's voice from the preferred pool (delegates to
    /// [`pick_pool_voice`]), recording the pick so it's stable per session. `pool`
    /// must be non-empty (caller checks).
    fn assign_pool_voice(&self, session: &str, pool: &[String]) -> String {
        let mut map = self.pool_assignments.lock().unwrap();
        let voice = pick_pool_voice(&map, pool, session);
        record_pool_assignment(&mut map, session, voice.clone(), POOL_ASSIGNMENTS_MAX);
        voice
    }

    /// Resolve the `(engine, voice)` for `session` — the ONE place the greeting and the playback
    /// worker agree on "what speaks". The engine is the resolved `tts_engine` ladder rung; the
    /// voice is the System voice, or this terminal's CLAIMED Kokoro pool voice (locking the
    /// per-terminal assignment; the global/empty session and an empty pool fall back to
    /// `current_voice()`). `None` when TTS is off — no usable rung — so the caller skips/no-ops.
    fn resolve_engine_voice(
        &self,
        cfg: &VoiceConfig,
        session: &Option<String>,
    ) -> Option<(ds_config::TtsEngine, String)> {
        let engine = cfg.resolved_tts()?;
        let voice = match engine {
            ds_config::TtsEngine::System => cfg.tts_system_voice.clone(),
            ds_config::TtsEngine::Kokoro => {
                let pool = cfg.active_voices();
                let sess = vkey(session);
                if !pool.is_empty() && !sess.is_empty() {
                    self.assign_pool_voice(&sess, pool)
                } else {
                    cfg.current_voice()
                }
            }
        };
        Some((engine, voice))
    }

    /// Greet a freshly-opened terminal in its assigned pool / system voice (no-op unless
    /// `greet_on_open` is set and TTS is on). Claims the session's voice now via
    /// [`resolve_engine_voice`](Self::resolve_engine_voice), so the per-terminal assignment is
    /// locked in at open rather than on first reply.
    pub fn greet_session(&self, session: Option<String>) {
        let cfg = self.config.lock().unwrap().clone();
        if !cfg.greet_on_open {
            return;
        }
        // Resolve the active engine + voice via the SAME shared helper the worker uses, so the
        // greeting is NAMED by and SPOKEN in exactly the voice that will play (under System that
        // means the system voice, not a Kokoro id handed to `say`). `None` ⇒ TTS off ⇒ no greeting.
        let Some((engine, voice)) = self.resolve_engine_voice(&cfg, &session) else {
            return;
        };
        // Name the greeting via the ONE shared resolver (Kokoro id → "Sarah"; System → the
        // tidied `tts_system_voice`, or the OS-default voice's name). A None name (e.g. System
        // OS-default where it can't be read) falls back to a name-less greeting.
        let name = ds_tts::enumerate::voice_display_name(engine, &voice);
        let idx = GREET_ROTATION.fetch_add(1, Ordering::Relaxed);
        let text = greeting_line(name.as_deref(), idx);
        if let Err(e) = self.enqueue(text, Some(voice), None, session) {
            log::warn!(target: "ttsq", "greeting rejected: {e}");
        }
    }

    /// Snapshot the two hold gates together so the loop's hold and busy decisions cannot
    /// disagree because they sampled the microphone/focus state at different moments.
    fn worker_hold_state(&self) -> HoldState {
        hold_state(
            self.tts.is_full_duplex_active(),
            self.mic.is_active(),
            self.pause_in_background.load(Ordering::SeqCst),
            self.terminal_seen.load(Ordering::SeqCst),
            self.terminal_front.load(Ordering::SeqCst),
        )
    }

    fn worker_focus_hold(&self) -> bool {
        focus_holds(
            self.pause_in_background.load(Ordering::SeqCst),
            self.terminal_seen.load(Ordering::SeqCst),
            self.terminal_front.load(Ordering::SeqCst),
        )
    }

    fn run(self: Arc<Self>) {
        loop {
            // Wait for a PLAYABLE item (see [`select_pos`]) while not paused: items for
            // other terminals are held in place until their terminal becomes active.
            // Lock order: `items` then `active`.
            let (item, gen0) = {
                let mut q = self.items.lock().unwrap();
                loop {
                    if !self.is_paused() {
                        let active = self.active.lock().unwrap().effective();
                        if let Some(pos) = select_pos(&q, &active) {
                            break self.claim_item(&mut q, pos);
                        }
                    }
                    q = self.cv.wait(q).unwrap();
                }
            };
            let _in_flight = InFlightGuard(&self.in_flight);
            let _playing = PlayingGuard(&self.playing_session);

            let (engine, voice, rate) = match self.gate_item(&item, gen0) {
                GateOutcome::Play {
                    engine,
                    voice,
                    rate,
                } => (engine, voice, rate),
                GateOutcome::Requeue => {
                    self.requeue_if_resuming(item, gen0);
                    continue;
                }
                GateOutcome::Drop(reason) => {
                    log::warn!(target: "ttsq", "queued speak could not start: {reason}");
                    continue;
                }
            };

            self.set_tts_active(true);
            // Speak the whole block in ONE call (the warm child prepares it transactionally,
            // then plays it continuously) — uniformly for NARRATION and REPLY. If a record-barge
            // pause (generation bump) interrupts playback mid-way, re-enqueue the item so
            // `resume()` continues it. This is the SAME for both kinds: the old per-kind
            // split re-enqueued only replies, so an interrupted NARRATION was dropped —
            // tap-to-pause then tap-to-resume came back SILENT.
            if let Err(e) = self.speak_one(engine, &item.text, &voice, rate) {
                log::warn!(target: "ttsq", "queued speak failed: {e}");
            }
            if self.generation.load(Ordering::SeqCst) != gen0 {
                self.requeue_if_resuming(item, gen0);
            }
            self.set_tts_active(false);
        }
    }

    /// The combined pre-playback gate for one claimed item: hold, wait for readiness, then
    /// hold AGAIN if the wait outlasted a focus/mic change. One loop, so an item can never
    /// start playing under a hold that arrived during the (up to 60 s) readiness wait —
    /// `set_terminal_front` only stores atomics and never bumps the generation, so nothing
    /// else breaks that window.
    fn gate_item(&self, item: &Item, gen0: u64) -> GateOutcome {
        loop {
            // HOLD this item (any kind) while we must stay silent — resume when the
            // gate clears, dropping nothing. Two independent "hold, don't drop" gates:
            //   * mic live (HALF-DUPLEX only): never speak into a recording. Full-duplex
            //     skips this — the VPIO mic is always live (`is_mic_active()` permanently
            //     true), so the AEC lets us speak into the open mic (coexist: playback and
            //     dictation overlap; the voice stops only on an explicit `stop`/`stopfade`
            //     op, not an implicit talk-over barge).
            //   * no terminal frontmost: you've tabbed to a browser/other app — the
            //     cross-platform focus gate (applies in BOTH duplex modes). The worker
            //     reads the flag the poll thread publishes (NSWorkspace is poll/main-
            //     thread affine). Self-arming via `terminal_seen`, so an unrecognized
            //     terminal never goes mute. DISABLED when `pause_in_background` is false
            //     (config) — then speech plays regardless of which app is frontmost.
            // A generation bump (a pause edge or a hard StopSpeech/clear) breaks the
            // wait so it never sticks.
            while self.generation.load(Ordering::SeqCst) == gen0 {
                let hold = self.worker_hold_state();
                if !hold.any() {
                    break;
                }
                // Focus takes precedence while both gates are live. Reporting the mic hold as
                // busy here made always-listening alternate between opening its capture and
                // immediately closing/resetting it. Once focus returns, the focus gate clears,
                // the mic-only hold reports busy once, and closing capture lets playback start.
                self.in_flight.store(hold.reports_busy(), Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(120));
            }
            // Past the hold: the item is ours to play, so it is in flight again regardless of
            // which gate we waited on. (`InFlightGuard` still clears it on every exit path.)
            self.in_flight.store(true, Ordering::SeqCst);
            if self.generation.load(Ordering::SeqCst) != gen0 {
                return GateOutcome::Requeue;
            }

            let cfg = self.config.lock().unwrap().clone();
            // Engine + base voice come from config via the SAME shared helper the greeting
            // uses — System reads `tts_system_voice`; Kokoro claims this terminal's pool voice
            // (the global/empty session and an empty pool fall back to `current_voice()`). Off /
            // no usable rung ⇒ a blank voice (speak_one no-ops, value unused). A per-call
            // `item.voice` (e.g. the MCP `speak` voice arg) then overrides just the voice
            // string within the chosen engine.
            let (engine, base_voice) = match self.resolve_engine_voice(&cfg, &item.session) {
                Some((e, v)) => (Some(e), v),
                None => (None, String::new()),
            };
            let voice = item.voice.clone().unwrap_or(base_voice);
            let rate = item.rate.unwrap_or(cfg.tts_rate);

            // Never send Kokoro work before its model is ready. Accepted work is HELD during
            // an ordinary warm-up and remains busy; it is dropped only for an explicit cancel,
            // disabled engine, terminal load failure, or readiness timeout. The wait never
            // blocks on child lifecycle calls (healing is async, issue #59), so its deadline
            // is a real upper bound.
            if !crate::config_gate::tts_can_play(engine, self.tts.is_tts_loaded()) {
                match self.wait_until_ready(engine, gen0) {
                    ReadyOutcome::Ready => {}
                    ReadyOutcome::Cancelled => return GateOutcome::Requeue,
                    ReadyOutcome::Unavailable(reason) => return GateOutcome::Drop(reason),
                }
                // Regression guard (audit): focus/mic can flip while the readiness wait runs.
                // Re-enter the hold gate (re-resolving config, which may have changed too)
                // instead of playing into a background app the moment the model loads.
                if self.worker_hold_state().any() {
                    continue;
                }
            }
            return GateOutcome::Play {
                engine,
                voice,
                rate,
            };
        }
    }

    /// Play one queued block on the warm child using the resolved `engine` (config or a
    /// session override) and `voice`. `None` = TTS off — never speak (defensive; items
    /// shouldn't be enqueued when off).
    fn speak_one(
        &self,
        engine: Option<ds_config::TtsEngine>,
        text: &str,
        voice: &str,
        rate: f32,
    ) -> Result<(), String> {
        let result = match engine {
            None => return Err("TTS is disabled".to_string()),
            Some(ds_config::TtsEngine::System) => self.tts.speak_system(text, voice, rate),
            Some(ds_config::TtsEngine::Kokoro) => {
                self.tts.ensure_started();
                self.tts.speak(text, voice, rate)
            }
        };
        result.map_err(|e| e.to_string())
    }

    fn wait_until_ready(&self, engine: Option<ds_config::TtsEngine>, gen0: u64) -> ReadyOutcome {
        const READY_TIMEOUT: Duration = Duration::from_secs(60);
        self.wait_until_ready_with_timeout(engine, gen0, READY_TIMEOUT)
    }

    /// Timeout-injectable body of [`wait_until_ready`](Self::wait_until_ready) so tests can
    /// pin the deadline arm in milliseconds instead of the production 60 s.
    fn wait_until_ready_with_timeout(
        &self,
        engine: Option<ds_config::TtsEngine>,
        gen0: u64,
        timeout: Duration,
    ) -> ReadyOutcome {
        use ds_config::TtsEngine;

        match engine {
            None => return ReadyOutcome::Unavailable("TTS is disabled".to_string()),
            Some(TtsEngine::System) => return ReadyOutcome::Ready,
            Some(TtsEngine::Kokoro) => {}
        }

        // Complete mark_dead's "next speak heals" contract WITHOUT blocking: the heal
        // routes through `heal_crashed_child`'s single-flight background thread, never a
        // synchronous `restart_if_crashed` — a start can hold the manager's `lifecycle`
        // lock across a whole spawn+READY handshake (bounded since issue #59, but still
        // up to `READY_HANDSHAKE_TIMEOUT`), and riding it here kept the claimed item
        // `in_flight` (mic closed, `stop_speech` unheard) for the duration. This worker
        // instead keeps polling at the 50 ms tick, so cancel/ready/error all stay live.
        // `heal_kicked`/`load_requested` bound the side effects to one heal kick / one
        // `load` write per child incarnation, not one per tick.
        let deadline = Instant::now() + timeout;
        let mut heal_kicked = false;
        let mut load_requested = false;
        if self.tts.is_running() {
            if !self.tts.is_tts_loaded() {
                // A cached TTSLOADERR predating this retry is not terminal — the load fired
                // right below may resolve it (e.g. an AV scan briefly locking the model).
                // Clear it so the wait loop only fails on a FRESH error from this attempt;
                // a genuinely permanent failure re-emits TTSLOADERR and still fails fast.
                self.tts.clear_tts_load_error();
                self.tts.load_engine("tts"); // fire-and-forget stdin write — non-blocking
                load_requested = true;
            }
        } else {
            // A TTSLOADERR from a dead child is stale by definition — clear it BEFORE
            // kicking the async heal. The heal is no longer synchronous, so without this
            // clear the poll loop's error check would fire on its very first iteration
            // (microseconds from now, long before the heal thread finishes spawn+READY)
            // and return Unavailable with the dead child's stale error — dropping the
            // held item instead of healing and retrying. A genuinely permanent failure
            // re-emits from the fresh child. Pinned by
            // wait_until_ready_ignores_a_stale_load_error_when_retrying.
            self.tts.clear_tts_load_error();
            self.heal_crashed_child();
            heal_kicked = true;
        }
        loop {
            // ORDER LOAD-BEARING: cancel beats ready beats error — pinned by
            // wait_until_ready_cancellation_beats_a_manager_error. `last_error` is
            // deliberately NEVER cleared here: a failed start (including a timed-out
            // READY handshake) parks the manager with it set, and this error check
            // surfacing it is the intended give-up path.
            if self.generation.load(Ordering::SeqCst) != gen0 {
                return ReadyOutcome::Cancelled;
            }
            if self.tts.is_tts_loaded() {
                return ReadyOutcome::Ready;
            }
            if let Some(error) = self.tts.tts_load_error().or_else(|| self.tts.last_error()) {
                return ReadyOutcome::Unavailable(error);
            }
            if !self.tts.is_running() {
                if !heal_kicked {
                    // A mid-wait death: mirror the entry rule above — the stale-error
                    // clear must happen when the heal is KICKED (it completes
                    // asynchronously, so clearing "after" it has no defined moment).
                    self.tts.clear_tts_load_error();
                    self.heal_crashed_child();
                    heal_kicked = true;
                }
                load_requested = false; // a fresh child may need a fresh load request
            } else {
                heal_kicked = false; // running again; re-kick if it dies later
                if !load_requested {
                    // Same stale-error rule as the entry retry above.
                    self.tts.clear_tts_load_error();
                    self.tts.load_engine("tts");
                    load_requested = true;
                }
            }
            if Instant::now() >= deadline {
                return ReadyOutcome::Unavailable(
                    "timed out waiting for the Kokoro model to become ready".to_string(),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// On a cancel: if the SPECIFIC cancellation that interrupted THIS item (recorded
    /// against its own `gen0` in `cancel_kind`, at the moment that bump fired) was a
    /// record-barge pause, re-enqueue the interrupted item (narration OR reply) at the
    /// front so the worker resumes the whole queue from there. A block is one synth unit
    /// (the warm child prepares it before playback), so we re-speak it from the top rather than
    /// from a sentence offset. A hard cancel re-enqueues nothing — it dropped the item on
    /// purpose.
    ///
    /// Deliberately does NOT just re-read the CURRENT `paused` flag: by the time playback
    /// actually unwinds, an unrelated LATER event may have moved `paused` on — e.g. an
    /// explicit `cancel_for_submit` current-scope drop (intending no requeue) immediately followed
    /// by an unrelated record-barge `pause_for_record` (which sets `paused = true`) would
    /// make a live read see `paused == true` and wrongly resurrect an item the clear had
    /// already cancelled. Falls back to the live flag only if this bump's intent was never
    /// recorded (defensive — every generation-bumping site above records one).
    fn requeue_if_resuming(&self, item: Item, gen0: u64) {
        let requeue = self
            .cancel_kind
            .lock()
            .unwrap()
            .remove(&gen0)
            .unwrap_or_else(|| self.is_paused());
        if !should_requeue(requeue, &item.text) {
            return;
        }
        let mut q = self.items.lock().unwrap();
        q.push_front(Item {
            text: item.text,
            voice: item.voice,
            rate: item.rate,
            session: item.session,
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadyOutcome {
    Ready,
    Cancelled,
    Unavailable(String),
}

/// What [`TtsQueue::gate_item`] decided for a claimed item.
#[derive(Debug)]
enum GateOutcome {
    Play {
        engine: Option<ds_config::TtsEngine>,
        voice: String,
        rate: f32,
    },
    Requeue,
    Drop(String),
}

/// Whether an interrupted item (narration OR reply) should be RE-ENQUEUED to resume later.
/// Only when we were PAUSED for a record-barge (resume mode) — a hard clear/StopSpeech
/// leaves `paused == false` and re-enqueues nothing (it dropped on purpose). Empty text is
/// never requeued. Pure, so the "resume keeps the item, clear drops it" rule is unit-tested.
fn should_requeue(paused: bool, text: &str) -> bool {
    paused && !text.trim().is_empty()
}

/// The worker's two independent "hold, don't drop" gates.
///
/// - MIC LIVE (half-duplex only): never speak into a recording. Full-duplex skips this
///   — the VPIO mic is always live, so the AEC handles the overlap instead (coexist;
///   the voice stops only on an explicit `stop`/`stopfade` op, never a talk-over barge).
/// - FOCUS (both modes, only when `pause_in_background`): no terminal frontmost (you
///   tabbed to a browser) → hold. Self-arming via `terminal_seen`, so an unrecognized
///   terminal emulator (never seen frontmost) degrades to always-play, never mute.
///
/// PURE — the worker re-evaluates this state each tick while holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HoldState {
    mic: bool,
    focus: bool,
}

impl HoldState {
    fn any(self) -> bool {
        self.mic || self.focus
    }

    /// Only a mic-only hold closes always-listening. If focus also holds, closing the mic
    /// clears `mic` but not `focus`, so repeatedly mirroring `mic` into busy creates an
    /// open/close feedback loop instead of progress.
    fn reports_busy(self) -> bool {
        self.mic && !self.focus
    }
}

fn mic_holds(full_duplex: bool, mic_active: bool) -> bool {
    !full_duplex && mic_active
}

fn focus_holds(pause_in_background: bool, terminal_seen: bool, terminal_front: bool) -> bool {
    pause_in_background && terminal_seen && !terminal_front
}

fn hold_state(
    full_duplex: bool,
    mic_active: bool,
    pause_in_background: bool,
    terminal_seen: bool,
    terminal_front: bool,
) -> HoldState {
    HoldState {
        mic: mic_holds(full_duplex, mic_active),
        focus: focus_holds(pause_in_background, terminal_seen, terminal_front),
    }
}

/// How recently a voice submit must have happened for the next UserPromptSubmit hook to be
/// its echo (rather than a real text submit). The hook fires sub-second after the voice
/// submit's auto-Enter, so the window is generous.
const VOICE_SUBMIT_WINDOW: Duration = Duration::from_secs(3);

/// Pure predicate behind [`TtsQueue::take_recent_voice_submit`]: did a voice submit at `last`
/// happen within the de-dup window before `now`?
fn voice_submit_recent(last: Option<Instant>, now: Instant) -> bool {
    matches!(last, Some(t) if now.saturating_duration_since(t) < VOICE_SUBMIT_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_hold_mic_and_focus_gates() {
        // MIC gate (half-duplex): a live mic holds; full-duplex ignores the mic.
        assert!(
            hold_state(false, true, false, false, false).any(),
            "half-duplex + mic → hold"
        );
        assert!(
            !hold_state(true, true, false, false, false).any(),
            "full-duplex ignores mic"
        );
        // FOCUS gate: only when pause_in_background AND a terminal has been seen AND none
        // is frontmost. Self-arming: unseen terminal never holds (degrade to always-play).
        assert!(
            hold_state(false, false, true, true, false).any(),
            "bg pause + seen + not front → hold"
        );
        assert!(
            !hold_state(false, false, true, false, false).any(),
            "never-seen terminal → play"
        );
        assert!(
            !hold_state(false, false, true, true, true).any(),
            "terminal frontmost → play"
        );
        assert!(
            !hold_state(false, false, false, true, false).any(),
            "pause_in_background off → play"
        );
        // Nothing gating → play.
        assert!(!hold_state(false, false, false, false, false).any());
    }

    #[test]
    fn focus_takes_precedence_over_mic_for_busy_reporting() {
        let focus_only = hold_state(false, false, true, true, false);
        assert!(focus_only.any());
        assert!(!focus_only.reports_busy());

        // Always-listening opens the mic while focus holds. That must remain idle to the
        // listener; mirroring this mic edge into busy would close it and oscillate forever.
        let both = hold_state(false, true, true, true, false);
        assert!(both.any());
        assert!(!both.reports_busy());

        // Once focus returns, the mic-only hold closes capture exactly once so playback can run.
        let mic_only = hold_state(false, true, true, true, true);
        assert!(mic_only.reports_busy());
        assert!(!hold_state(true, true, false, false, false).any());
    }

    #[test]
    fn should_requeue_only_when_paused_and_nonempty() {
        // Resume mode (paused) keeps a non-empty item → re-enqueued to continue.
        assert!(should_requeue(true, "the held narration"));
        // A hard clear / StopSpeech leaves paused == false → dropped on purpose.
        assert!(!should_requeue(false, "the held narration"));
        // Empty / whitespace-only text is never requeued, even when paused.
        assert!(!should_requeue(true, ""));
        assert!(!should_requeue(true, "   \n\t "));
    }

    #[test]
    fn voice_submit_recent_window() {
        let base = Instant::now();
        // No voice submit → never a voice echo.
        assert!(!voice_submit_recent(None, base));
        // Within the window (0s, 2s) → it's the voice submit's echo.
        assert!(voice_submit_recent(Some(base), base));
        assert!(voice_submit_recent(
            Some(base),
            base + Duration::from_secs(2)
        ));
        // Past the window (4s) → a genuine text submit, not the echo.
        assert!(!voice_submit_recent(
            Some(base),
            base + Duration::from_secs(4)
        ));
    }

    fn pool() -> Vec<String> {
        vec!["af_sarah".into(), "am_adam".into(), "bf_emma".into()]
    }

    #[test]
    fn each_terminal_gets_the_next_untaken_voice() {
        // Three distinct sessions claim three distinct voices, in pool order.
        let p = pool();
        let mut a = HashMap::new();
        for (sess, want) in [("s1", "af_sarah"), ("s2", "am_adam"), ("s3", "bf_emma")] {
            let v = pick_pool_voice(&a, &p, sess);
            assert_eq!(v, want, "session {sess} should get the next untaken voice");
            a.insert(sess.to_string(), v);
        }
    }

    #[test]
    fn assignment_is_stable_per_session() {
        let p = pool();
        let mut a = HashMap::new();
        let first = pick_pool_voice(&a, &p, "s1");
        a.insert("s1".into(), first.clone());
        // A second lookup for the same session returns the SAME voice, regardless of others.
        a.insert("s2".into(), "am_adam".into());
        assert_eq!(pick_pool_voice(&a, &p, "s1"), first);
    }

    #[test]
    fn pool_round_robins_once_exhausted() {
        // More terminals than voices → wrap (reuse) by assignment count.
        let p = pool(); // len 3
        let mut a = HashMap::new();
        for (i, sess) in ["s1", "s2", "s3"].iter().enumerate() {
            a.insert(sess.to_string(), p[i].clone());
        }
        // All three taken → the 4th session wraps to pool[3 % 3] = pool[0].
        assert_eq!(pick_pool_voice(&a, &p, "s4"), "af_sarah");
    }

    #[test]
    fn stale_assignment_is_dropped_when_voice_leaves_the_pool() {
        // Regression: a session assigned under the OLD pool keeps speaking the old
        // voice after the user changes `tts_built_in_voices` (the assignment cache
        // survives a config hot-reload). The stale pick must be discarded and a voice
        // from the CURRENT pool chosen instead — otherwise the terminal keeps using a
        // voice the user dropped ("Sarah introduces herself as Nicole").
        let mut a = HashMap::new();
        a.insert("s1".to_string(), "af_sarah".to_string()); // picked under the old default
        let new_pool = vec!["af_nicole".to_string()]; // user switched to Nicole-only
        let v = pick_pool_voice(&a, &new_pool, "s1");
        assert_eq!(
            v, "af_nicole",
            "a voice no longer in the pool must not be reused"
        );
        // And once re-recorded, the fresh pick is stable.
        a.insert("s1".to_string(), v);
        assert_eq!(pick_pool_voice(&a, &new_pool, "s1"), "af_nicole");
    }

    #[test]
    fn stale_assignment_repick_avoids_voices_taken_by_other_live_sessions() {
        // When a stale session re-picks, it must still get a DISTINCT voice from the
        // sessions whose assignments are still valid under the new pool.
        let p = vec!["af_nicole".to_string(), "am_adam".to_string()];
        let mut a = HashMap::new();
        a.insert("s1".to_string(), "af_sarah".to_string()); // stale (old pool)
        a.insert("s2".to_string(), "af_nicole".to_string()); // valid, holds af_nicole
        // s1 re-picks: af_nicole is taken by s2, so s1 gets am_adam (not the stale af_sarah).
        assert_eq!(pick_pool_voice(&a, &p, "s1"), "am_adam");
    }

    #[test]
    fn pool_assignment_map_is_bounded_defensively() {
        // A client that never sends SessionEnd (crashed hook, or one hammering
        // GreetSession with fresh session ids) must not grow this map forever.
        let mut a = HashMap::new();
        for i in 0..3 {
            record_pool_assignment(&mut a, &format!("s{i}"), "af_sarah".into(), 3);
        }
        assert_eq!(a.len(), 3, "under the cap: every entry kept");
        // The 4th insert pushes len to 4 > cap(3) → the whole map is cleared, not grown.
        record_pool_assignment(&mut a, "s3", "af_sarah".into(), 3);
        assert_eq!(a.len(), 0, "over the cap: cleared, never grown past it");
    }

    /// Build a narration `Item` tagged with `session` (the only field `select_pos`
    /// inspects), for the selection truth-table tests.
    fn narr(session: Option<&str>) -> Item {
        Item {
            text: "x".into(),
            voice: None,
            rate: None,
            session: session.map(str::to_string),
        }
    }

    fn deque(sessions: &[Option<&str>]) -> VecDeque<Item> {
        sessions.iter().map(|s| narr(*s)).collect()
    }

    #[test]
    fn no_active_session_is_strict_fifo() {
        // None active (no prompt-hook yet) → always the front item, regardless of tags.
        let q = deque(&[Some("a"), Some("b")]);
        assert_eq!(select_pos(&q, &None), Some(0));
        assert_eq!(select_pos(&VecDeque::new(), &None), None);
    }

    #[test]
    fn active_session_picks_its_item_and_holds_others() {
        // Active = "b": PREFER b's item while b has one queued (a's wait behind it).
        let q = deque(&[Some("a"), Some("b"), Some("a")]);
        assert_eq!(select_pos(&q, &Some("b".into())), Some(1));
    }

    #[test]
    fn active_session_with_no_item_falls_back_to_fifo_not_starvation() {
        // The active terminal "b" has NOTHING queued → another terminal's reply must
        // still play (FIFO), never be held forever. (Regression: the old behavior
        // returned None here, silencing a backgrounded window indefinitely.)
        let q = deque(&[Some("a"), Some("a")]);
        assert_eq!(select_pos(&q, &Some("b".into())), Some(0));
        // Empty queue is still nothing to play.
        assert_eq!(select_pos(&VecDeque::new(), &Some("b".into())), None);
    }

    #[test]
    fn untagged_global_audio_plays_under_any_active() {
        // session == None (e.g. the MCP `speak` tool) isn't tied to a terminal → it
        // plays even when another terminal is active.
        let q = deque(&[Some("a"), None, Some("a")]);
        assert_eq!(select_pos(&q, &Some("b".into())), Some(1));
    }

    #[test]
    fn prune_session_drops_only_that_window() {
        // A per-window stop for "a" removes a's items, keeps b's and untagged global.
        let mut q = deque(&[Some("a"), Some("b"), None, Some("a")]);
        prune_session(&mut q, &Some("a".into()));
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(kept, vec![Some("b".into()), None]);
    }

    #[test]
    fn prune_session_none_drops_only_untagged_global() {
        // `Some(None)` target prunes untagged/global items, leaving tagged windows —
        // the GLOBAL hard barge goes through `clear()`, not this path.
        let mut q = deque(&[Some("a"), None, Some("b")]);
        prune_session(&mut q, &None);
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(kept, vec![Some("a".into()), Some("b".into())]);
    }

    #[test]
    fn retain_only_session_is_the_exact_inverse_of_prune_session() {
        // `input_clears = [other]`: keeping "a" drops b's item AND the untagged/global
        // one — unlike `prune_session`, untagged audio does NOT survive here.
        let mut q = deque(&[Some("a"), Some("b"), None, Some("a")]);
        retain_only_session(&mut q, &Some("a".into()));
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(kept, vec![Some("a".into()), Some("a".into())]);
    }

    #[test]
    fn retain_only_session_none_keeps_only_untagged_global() {
        let mut q = deque(&[Some("a"), None, Some("b")]);
        retain_only_session(&mut q, &None);
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(kept, vec![None]);
    }

    #[test]
    fn effective_prefers_explicit_then_recent() {
        let mut s = ActiveSel::default();
        assert_eq!(s.effective(), None); // nothing known → FIFO
        s.recent = Some("r".into());
        assert_eq!(s.effective(), Some("r".into())); // recency fallback
        s.explicit = Some("e".into());
        assert_eq!(s.effective(), Some("e".into())); // prompt-target wins
    }

    #[test]
    fn greeting_names_the_voice_and_rotates() {
        // Every template carries the resolved name…
        for i in 0..GREETINGS.len() {
            assert!(
                greeting_line(Some("Sarah"), i).contains("Sarah"),
                "template {i} names the voice"
            );
        }
        // …consecutive indices differ, and the index wraps the set.
        assert_ne!(
            greeting_line(Some("Sarah"), 0),
            greeting_line(Some("Sarah"), 1)
        );
        assert_eq!(
            greeting_line(Some("Sarah"), 0),
            greeting_line(Some("Sarah"), GREETINGS.len())
        );
    }

    #[test]
    fn greeting_falls_back_to_anon_without_a_name() {
        // A resolved name gets a NAMED line…
        assert!(greeting_line(Some("Hazel"), 0).contains("Hazel"));
        // …but no name (None or blank) gets a name-LESS line — no stray `{n}` placeholder or
        // leading separator.
        for i in 0..GREETINGS_ANON.len() {
            for g in [greeting_line(None, i), greeting_line(Some("  "), i)] {
                assert!(!g.contains("{n}"), "anon line {i} has no placeholder");
                assert!(!g.starts_with(['—', ' ']), "anon line {i} reads cleanly");
            }
        }
        assert_ne!(greeting_line(None, 0), greeting_line(None, 1)); // rotates
    }

    /// Build a `TtsQueue` WITHOUT spawning its worker thread — these tests exercise
    /// `record_cancel_kind`/`requeue_if_resuming` directly, so a live worker would be pure
    /// risk (an unrelated thread touching `items`/`cancel_kind`) for zero benefit. Thin
    /// alias for [`TtsQueue::test_stub`] (which moved out of this module so `engine.rs`'s
    /// tests can build one too — the fields are private to this file).
    fn mk_queue() -> Arc<TtsQueue> {
        TtsQueue::test_stub()
    }

    /// Build a throwaway narration `Item` with the given text (voice/rate/session unused by
    /// the `cancel_kind`/`requeue_if_resuming` tests below).
    fn item(text: &str) -> Item {
        Item {
            text: text.to_string(),
            voice: None,
            rate: None,
            session: None,
        }
    }

    #[test]
    fn requeue_if_resuming_puts_the_item_back_at_front_when_marked_for_resume() {
        // A record-barge pause records `true` against the generation the interrupted item was
        // running under; `requeue_if_resuming` must look THAT up (not the live `paused` flag)
        // and re-enqueue the item at the front, ahead of whatever was already queued.
        let q = mk_queue();
        q.items.lock().unwrap().push_back(item("already queued"));
        q.record_cancel_kind(7, true);
        q.requeue_if_resuming(item("interrupted"), 7);
        let items = q.items.lock().unwrap();
        assert_eq!(
            items.len(),
            2,
            "the interrupted item is re-enqueued, not dropped"
        );
        assert_eq!(
            items.front().map(|it| it.text.as_str()),
            Some("interrupted"),
            "it lands at the FRONT, ahead of the rest of the queue"
        );
    }

    #[test]
    fn requeue_if_resuming_drops_the_item_when_marked_a_hard_cancel() {
        // A hard cancel (clear/skip/clear_session/cancel_for_submit) records `false` — the
        // interrupted item must be dropped, not resurrected.
        let q = mk_queue();
        q.record_cancel_kind(9, false);
        q.requeue_if_resuming(item("dropped"), 9);
        assert!(
            q.items.lock().unwrap().is_empty(),
            "a hard-cancel-marked generation must not requeue its item"
        );
    }

    #[test]
    fn requeue_if_resuming_falls_back_to_the_live_paused_flag_when_unrecorded() {
        // Defensive fallback: if this generation's intent was never recorded (or was already
        // consumed), `requeue_if_resuming` reads the CURRENT `paused` flag instead.
        let q = mk_queue();
        q.paused.lock().unwrap().paused = true;
        q.requeue_if_resuming(item("resumed via live flag"), 123);
        assert_eq!(
            q.items.lock().unwrap().len(),
            1,
            "paused==true → falls back to requeue"
        );

        let q2 = mk_queue();
        q2.paused.lock().unwrap().paused = false;
        q2.requeue_if_resuming(item("dropped via live flag"), 456);
        assert!(
            q2.items.lock().unwrap().is_empty(),
            "paused==false → falls back to dropping"
        );
    }

    #[test]
    fn record_cancel_kind_consumes_the_entry_on_lookup() {
        // `requeue_if_resuming` looks the entry up via `remove` — a SECOND lookup of the same
        // generation must not see it again (it must fall back to the live `paused` flag).
        let q = mk_queue();
        q.record_cancel_kind(1, true);
        q.paused.lock().unwrap().paused = false; // live flag says "drop" this time
        q.requeue_if_resuming(item("first"), 1); // consumes the recorded `true`
        assert_eq!(
            q.items.lock().unwrap().len(),
            1,
            "first lookup honors the recorded intent"
        );

        q.requeue_if_resuming(item("second"), 1); // entry already consumed → falls back to `paused`
        assert_eq!(
            q.items.lock().unwrap().len(),
            1,
            "the SAME generation looked up again falls back to the live flag, not a stale reuse"
        );
    }

    #[test]
    fn record_cancel_kind_evicts_only_the_oldest_once_the_bound_is_crossed() {
        // 32 distinct generations fit comfortably under the defensive bound…
        let q = mk_queue();
        for gen0 in 0..32u64 {
            q.record_cancel_kind(gen0, true);
        }
        assert_eq!(
            q.cancel_kind.lock().unwrap().len(),
            32,
            "32 distinct entries fit under the bound"
        );
        // …but the 33rd distinct key evicts only the oldest intent.
        q.record_cancel_kind(32, true);
        let kinds = q.cancel_kind.lock().unwrap();
        assert_eq!(kinds.len(), 32);
        assert!(!kinds.contains_key(&0));
        assert_eq!(kinds.get(&32), Some(&true));
    }

    #[test]
    fn enqueue_drops_empty_text_and_counts_real_items() {
        let q = mk_queue();
        q.enqueue("".into(), None, None, None).unwrap();
        q.enqueue("   \n\t".into(), None, None, None).unwrap();
        assert_eq!(
            q.snapshot().1,
            0,
            "empty/whitespace-only text is dropped, not queued"
        );
        q.enqueue("hello there".into(), None, None, None).unwrap();
        assert_eq!(q.snapshot().1, 1, "a real text block is queued");
    }

    #[test]
    fn enqueue_rejects_oversize_text_without_changing_queue_or_recency() {
        let q = mk_queue();
        let err = q
            .enqueue(
                "x".repeat(MAX_SPEAK_BYTES + 1),
                None,
                None,
                Some("oversize".into()),
            )
            .unwrap_err();

        assert!(err.contains("byte limit"));
        assert!(q.items.lock().unwrap().is_empty());
        assert_eq!(q.active_session(), None);
    }

    #[test]
    fn enqueue_bounds_each_session_and_keeps_other_sessions_usable() {
        let q = mk_queue();
        for _ in 0..MAX_SESSION_PENDING_ITEMS {
            q.enqueue("x".into(), None, None, Some("full".into()))
                .unwrap();
        }

        let err = q
            .enqueue("overflow".into(), None, None, Some("full".into()))
            .unwrap_err();
        assert!(err.contains("session speech queue is full"));
        q.enqueue("still accepted".into(), None, None, Some("other".into()))
            .unwrap();
    }

    #[test]
    fn enqueue_bounds_global_pending_work() {
        let q = mk_queue();
        for i in 0..MAX_PENDING_ITEMS {
            q.enqueue("x".into(), None, None, Some(format!("session-{i}")))
                .unwrap();
        }

        let err = q
            .enqueue("overflow".into(), None, None, Some("last".into()))
            .unwrap_err();
        assert!(err.contains("speech queue is full"));
        assert_eq!(q.items.lock().unwrap().len(), MAX_PENDING_ITEMS);
    }

    #[test]
    fn enqueue_accepts_exact_byte_caps_and_rejects_one_more_byte() {
        let per_session = mk_queue();
        for _ in 0..25 {
            per_session
                .enqueue("x".repeat(MAX_SPEAK_BYTES), None, None, Some("same".into()))
                .unwrap();
        }
        per_session
            .enqueue("x".repeat(6144), None, None, Some("same".into()))
            .unwrap();
        assert!(
            per_session
                .enqueue("x".into(), None, None, Some("same".into()))
                .unwrap_err()
                .contains("pending text bytes")
        );

        let global = mk_queue();
        for i in 0..102 {
            global
                .enqueue(
                    "x".repeat(MAX_SPEAK_BYTES),
                    None,
                    None,
                    Some(format!("session-{i}")),
                )
                .unwrap();
        }
        global
            .enqueue("x".repeat(4096), None, None, Some("exact".into()))
            .unwrap();
        assert!(
            global
                .enqueue("x".into(), None, None, Some("over".into()))
                .unwrap_err()
                .contains("pending text bytes")
        );
    }

    #[test]
    fn clear_drops_everything_resets_paused_and_bumps_generation() {
        let q = mk_queue();
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("b"))]);
        q.paused.lock().unwrap().paused = true;
        let gen_before = q.generation.load(Ordering::SeqCst);

        // Also proves this doesn't panic calling `tts.stop_fade()` against the stub manager.
        q.clear();

        assert!(
            q.items.lock().unwrap().is_empty(),
            "clear drops every queued item"
        );
        {
            let st = q.paused.lock().unwrap();
            assert!(!st.paused, "clear resets the pause");
            assert!(st.cause.is_none(), "clear resets the cause too");
        }
        assert!(
            q.generation.load(Ordering::SeqCst) > gen_before,
            "clear bumps the generation"
        );

        // `clear` itself records the old generation's outcome as a hard drop — a late
        // requeue lookup under that stale gen0 must not resurrect anything.
        q.requeue_if_resuming(item("stale-from-before-clear"), gen_before);
        assert!(
            q.items.lock().unwrap().is_empty(),
            "an item from the old generation is never resurrected after clear"
        );
    }

    #[test]
    fn clear_session_prunes_only_that_sessions_queued_items() {
        let q = mk_queue();
        q.items.lock().unwrap().extend([
            narr(Some("a")),
            narr(Some("b")),
            narr(None),
            narr(Some("a")),
        ]);
        q.clear_session(Some("a".into()));
        let kept: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(kept, vec![Some("b".into()), None]);
    }

    #[test]
    fn clear_session_cancels_in_flight_only_when_playing_session_matches() {
        // Playing the TARGET session → the in-flight item is cancelled too.
        let q = mk_queue();
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing_session.lock().unwrap() = Some(Some("a".into()));
        let gen_before = q.generation.load(Ordering::SeqCst);
        q.clear_session(Some("a".into()));
        assert!(
            q.generation.load(Ordering::SeqCst) > gen_before,
            "matching playing session is cancelled"
        );
        assert!(!q.tts_active.load(Ordering::SeqCst));

        // Playing a DIFFERENT session → this per-window stop leaves it alone.
        let q2 = mk_queue();
        q2.tts_active.store(true, Ordering::SeqCst);
        *q2.playing_session.lock().unwrap() = Some(Some("b".into()));
        let gen_before2 = q2.generation.load(Ordering::SeqCst);
        q2.clear_session(Some("a".into()));
        assert_eq!(
            q2.generation.load(Ordering::SeqCst),
            gen_before2,
            "non-matching playback is left alone"
        );
        assert!(q2.tts_active.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_for_submit_is_a_noop_without_scopes_or_a_resolved_target() {
        let q = mk_queue();
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("b"))]);
        let gen_before = q.generation.load(Ordering::SeqCst);

        // Neither scope requested.
        q.cancel_for_submit(Some("a".into()), false, false);
        assert_eq!(q.items.lock().unwrap().len(), 2);
        assert_eq!(q.generation.load(Ordering::SeqCst), gen_before);

        // No resolved target, even with both scopes on.
        q.cancel_for_submit(None, true, true);
        assert_eq!(q.items.lock().unwrap().len(), 2);
        assert_eq!(q.generation.load(Ordering::SeqCst), gen_before);
    }

    #[test]
    fn cancel_for_submit_current_prunes_target_and_cancels_unconditionally() {
        let q = mk_queue();
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("b"))]);
        // Someone ELSE is playing ("b") — `current` must still cancel unconditionally.
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing_session.lock().unwrap() = Some(Some("b".into()));
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.cancel_for_submit(Some("a".into()), true, false);

        let kept: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(
            kept,
            vec![Some("b".into())],
            "current prunes only target's own queued items"
        );
        assert!(
            q.generation.load(Ordering::SeqCst) > gen_before,
            "current cancels the in-flight item unconditionally"
        );
        assert!(!q.tts_active.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_for_submit_other_keeps_only_target_and_cancels_only_when_playing_is_other() {
        // Playing a DIFFERENT session than the target → `other` cancels it.
        let q = mk_queue();
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("b")), narr(None)]);
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing_session.lock().unwrap() = Some(Some("other".into()));
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.cancel_for_submit(Some("a".into()), false, true);

        let kept: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(
            kept,
            vec![Some("a".into())],
            "other keeps only the target's queued items"
        );
        assert!(
            q.generation.load(Ordering::SeqCst) > gen_before,
            "playing a DIFFERENT session than target is cancelled"
        );

        // Playing the TARGET itself → `other` must leave it alone (the worker can
        // legitimately already be playing the target's own item).
        let q2 = mk_queue();
        q2.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("b"))]);
        q2.tts_active.store(true, Ordering::SeqCst);
        *q2.playing_session.lock().unwrap() = Some(Some("a".into()));
        let gen_before2 = q2.generation.load(Ordering::SeqCst);

        q2.cancel_for_submit(Some("a".into()), false, true);

        let kept2: Vec<_> = q2
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(kept2, vec![Some("a".into())]);
        assert_eq!(
            q2.generation.load(Ordering::SeqCst),
            gen_before2,
            "playing the target itself is left alone"
        );
        assert!(q2.tts_active.load(Ordering::SeqCst));
    }

    #[test]
    fn cancel_for_submit_both_scopes_compose_to_an_empty_queue() {
        // `current` first drops the target's own queued items, then `other` retains ONLY
        // the target's items (now none) — the combination empties the queue entirely.
        let q = mk_queue();
        q.items.lock().unwrap().extend([
            narr(Some("a")),
            narr(Some("b")),
            narr(None),
            narr(Some("a")),
        ]);
        q.cancel_for_submit(Some("a".into()), true, true);
        assert!(q.items.lock().unwrap().is_empty());
    }

    #[test]
    fn pause_for_record_pauses_bumps_generation_and_marks_resume_intent() {
        let q = mk_queue();
        q.tts_active.store(true, Ordering::SeqCst);
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.pause_for_record();

        {
            let st = q.paused.lock().unwrap();
            assert!(st.paused);
            assert_eq!(st.cause, Some(PauseCause::Dictation));
        }
        assert!(!q.tts_active.load(Ordering::SeqCst));
        assert!(q.generation.load(Ordering::SeqCst) > gen_before);
        assert_eq!(
            q.cancel_kind.lock().unwrap().get(&gen_before),
            Some(&true),
            "a record-barge pause records RESUME (true) against the pre-bump generation"
        );
    }

    #[test]
    fn resume_is_a_noop_when_not_paused_and_clears_pause_when_it_was() {
        let q = mk_queue();
        assert!(!q.paused.lock().unwrap().paused);
        q.resume(); // no-op
        assert!(!q.paused.lock().unwrap().paused);

        q.pause_for_record();
        assert!(q.paused.lock().unwrap().paused);
        q.resume();
        {
            let st = q.paused.lock().unwrap();
            assert!(!st.paused, "resume clears the pause");
            assert!(st.cause.is_none(), "resume clears the cause too");
        }
    }

    #[test]
    fn pause_for_suspected_barge_tags_barge_speculative_when_idle() {
        // Baseline positive case: with nothing paused, the barge watcher's own pause
        // applies normally and tags BargeSpeculative (mirrors the existing
        // pause_for_record_... test, but for the new entry point).
        let q = mk_queue();
        q.tts_active.store(true, Ordering::SeqCst);
        let gen_before = q.generation.load(Ordering::SeqCst);
        q.pause_for_suspected_barge();
        let st = q.paused.lock().unwrap();
        assert!(st.paused);
        assert_eq!(st.cause, Some(PauseCause::BargeSpeculative));
        drop(st);
        assert!(q.generation.load(Ordering::SeqCst) > gen_before);
    }

    #[test]
    fn pause_for_suspected_barge_never_overwrites_an_existing_dictation_cause() {
        // THE ROUND-4 REGRESSION GUARD: a real Caps/PTT pause is already in effect
        // (pause_for_record, tagged Dictation) when the barge watcher's speculative
        // pause fires — e.g. the start_recording race window where pause_for_record()
        // (line ~1279) runs before set_stt_active(true) (line ~1318) and a foreign mic
        // edge lands in between. pause_for_suspected_barge() must be a no-op: cause
        // must STILL read Dictation afterward, never relabeled BargeSpeculative.
        let q = mk_queue();
        q.pause_for_record();
        let gen_after_dictation_pause = q.generation.load(Ordering::SeqCst);

        q.pause_for_suspected_barge();

        let st = q.paused.lock().unwrap();
        assert!(st.paused, "still paused");
        assert_eq!(
            st.cause,
            Some(PauseCause::Dictation),
            "a real Dictation pause must never be relabeled BargeSpeculative"
        );
        drop(st);
        assert_eq!(
            q.generation.load(Ordering::SeqCst),
            gen_after_dictation_pause,
            "the redundant speculative pause must not bump the generation again"
        );

        // And the mirror-image resume-side guard (round 3) still holds on top of this:
        // the barge watcher's own resume must NOT clear a Dictation-tagged pause.
        q.resume_if_barge_speculative();
        assert!(
            q.paused.lock().unwrap().paused,
            "resume_if_barge_speculative must leave a Dictation pause untouched"
        );
    }

    #[test]
    fn resume_if_barge_speculative_never_clears_a_real_dictation_pause() {
        // Round 3's planned regression test (not yet in the tree — add it here): a
        // Dictation-tagged pause is untouched by the barge watcher's auto-resume no
        // matter when it's called, since nothing else re-tags it BargeSpeculative
        // (this test doesn't rely on the round-4 pause-side guard — it pauses via
        // pause_for_record directly, so it also acts as a standalone check that the
        // resume-side guard alone is correct).
        let q = mk_queue();
        q.pause_for_record();
        q.resume_if_barge_speculative();
        let st = q.paused.lock().unwrap();
        assert!(
            st.paused,
            "a Dictation pause is never auto-cleared by the barge watcher"
        );
        assert_eq!(st.cause, Some(PauseCause::Dictation));
    }

    #[test]
    fn resume_if_barge_speculative_clears_a_barge_speculative_pause() {
        // Positive case: the barge watcher's own pause IS cleared by its own resume.
        let q = mk_queue();
        q.pause_for_suspected_barge();
        q.resume_if_barge_speculative();
        let st = q.paused.lock().unwrap();
        assert!(!st.paused);
        assert_eq!(st.cause, None);
    }

    #[test]
    fn is_busy_reflects_active_playback_or_queued_items() {
        let q = mk_queue();
        assert!(!q.is_busy());

        q.tts_active.store(true, Ordering::SeqCst);
        assert!(q.is_busy(), "actively playing counts as busy");
        q.tts_active.store(false, Ordering::SeqCst);
        assert!(!q.is_busy());

        q.in_flight.store(true, Ordering::SeqCst);
        assert!(q.is_busy(), "dequeued work held for warm-up counts as busy");
        q.in_flight.store(false, Ordering::SeqCst);
        assert!(!q.is_busy());

        q.items.lock().unwrap().push_back(narr(Some("a")));
        assert!(q.is_busy(), "anything queued counts as busy");
    }

    /// Regression: sampling `in_flight` before taking `items` allowed the worker to replace the
    /// final queued item with in-flight state while the reader retained the stale `false` sample.
    #[test]
    fn is_busy_couples_the_dequeue_to_in_flight_transition() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        items.push_back(narr(Some("a")));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reader = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(reader.is_busy()).unwrap();
        });
        started_rx.recv().unwrap();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(200)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            "the busy reader must wait for the queue transition lock"
        );

        items.pop_front().expect("queued item");
        q.in_flight.store(true, Ordering::SeqCst);
        drop(items);

        assert!(
            done_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            "the reader must observe either queued or in-flight work, never an idle gap"
        );
        handle.join().unwrap();
    }

    #[test]
    fn global_clear_cancels_an_item_claimed_during_the_queue_transition() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        items.push_back(narr(Some("a")));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.clear();
        });
        started_rx.recv().unwrap();

        let (_item, selected_generation) = q.claim_item(&mut items, 0);
        drop(items);
        handle.join().unwrap();

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            selected_generation,
            "a clear waiting on the selection lock must cancel the claimed item"
        );
    }

    #[test]
    fn session_clear_cancels_an_item_claimed_during_the_queue_transition() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        items.push_back(narr(Some("a")));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.clear_session(Some("a".into()));
        });
        started_rx.recv().unwrap();

        let (_item, selected_generation) = q.claim_item(&mut items, 0);
        drop(items);
        handle.join().unwrap();

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            selected_generation,
            "a session clear must observe and cancel the claimed session"
        );
    }

    #[test]
    fn other_scope_cancels_a_foreign_item_claimed_during_pruning() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        items.push_back(narr(Some("other")));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.cancel_for_submit(Some("current".into()), false, true);
        });
        started_rx.recv().unwrap();

        let (_item, selected_generation) = q.claim_item(&mut items, 0);
        drop(items);
        handle.join().unwrap();

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            selected_generation,
            "the other-scope clear must cancel a foreign item claimed under its queue lock"
        );
    }

    #[test]
    fn global_session_identity_is_distinct_from_an_idle_worker() {
        // The idle half: with NO claimed item (`playing_session` = None), a global-session
        // clear must NOT hard-cancel. The nested representation distinguishes "idle" (None)
        // from "playing the untagged global session" (Some(None)); the flat Option<String>
        // it replaced could not — this assertion fails under a revert to it.
        let q = mk_queue();
        let before = q.generation.load(Ordering::SeqCst);
        q.clear_session(None);
        assert_eq!(
            q.generation.load(Ordering::SeqCst),
            before,
            "an idle worker must not be hard-cancelled by a global-session clear"
        );

        // The playing half: an untagged claimed item IS the global session — it must cancel.
        let mut items = q.items.lock().unwrap();
        items.push_back(narr(None));
        let (_item, selected_generation) = q.claim_item(&mut items, 0);
        drop(items);

        q.clear_session(None);

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            selected_generation,
            "an untagged in-flight item must remain cancellable by its global session"
        );
    }

    #[test]
    fn focus_hold_keeps_listener_open_even_with_multiple_pending_items() {
        let q = mk_queue();
        q.set_terminal_front(true); // arm the self-disabling focus gate
        q.set_pause_in_background(true);
        q.set_terminal_front(false);
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("a")), narr(Some("a"))]);
        q.in_flight.store(true, Ordering::SeqCst);

        assert!(q.worker_focus_hold());
        assert!(
            !q.is_busy(),
            "focus-held queued/in-flight work must not close always-listening"
        );

        q.tts_active.store(true, Ordering::SeqCst);
        assert!(q.is_busy(), "audible playback always gates the listener");
        q.tts_active.store(false, Ordering::SeqCst);
        q.set_terminal_front(true);
        assert!(q.is_busy(), "returning focus re-arms pending work as busy");
    }

    /// `wait_until_ready` fast paths (audit): a disabled engine is `Unavailable` and System is
    /// `Ready`, both without touching the warm helper.
    #[test]
    fn wait_until_ready_fast_paths_for_disabled_and_system_engines() {
        let q = mk_queue();
        assert_eq!(
            q.wait_until_ready(None, 0),
            ReadyOutcome::Unavailable("TTS is disabled".to_string())
        );
        assert_eq!(
            q.wait_until_ready(Some(ds_config::TtsEngine::System), 0),
            ReadyOutcome::Ready
        );
    }

    /// Pins the cancel-before-error ordering inside the readiness poll: a generation bump must
    /// come back `Cancelled` (so the worker can requeue a merely-paused item) even when a
    /// manager error is also present — `Unavailable` here would drop the item instead.
    #[test]
    fn wait_until_ready_cancellation_beats_a_manager_error() {
        let q = mk_queue();
        let dir = tempfile::tempdir().unwrap();
        q.tts.set_stderr_log_dir_for_test(dir.path().to_path_buf());
        q.tts.restart_if_crashed(); // nonexistent helper: the failed spawn records last_error
        assert!(
            q.tts.last_error().is_some(),
            "premise: the failed spawn must leave a manager error"
        );
        let stale = q.generation.load(Ordering::SeqCst).wrapping_add(1);
        assert_eq!(
            q.wait_until_ready(Some(ds_config::TtsEngine::Kokoro), stale),
            ReadyOutcome::Cancelled
        );
    }

    /// A manager-level error fails fast as `Unavailable` instead of consuming the deadline.
    #[test]
    fn wait_until_ready_reports_a_manager_error_without_consuming_the_deadline() {
        let q = mk_queue();
        let dir = tempfile::tempdir().unwrap();
        q.tts.set_stderr_log_dir_for_test(dir.path().to_path_buf());
        q.tts.restart_if_crashed();
        assert!(
            q.tts.last_error().is_some(),
            "premise: the failed spawn must leave a manager error"
        );
        let started = std::time::Instant::now();
        let outcome = q.wait_until_ready(
            Some(ds_config::TtsEngine::Kokoro),
            q.generation.load(Ordering::SeqCst),
        );
        assert!(
            matches!(outcome, ReadyOutcome::Unavailable(_)),
            "{outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "an error must short-circuit the wait"
        );
    }

    /// The deadline arm: a never-ready helper with no error and no cancel times out as
    /// `Unavailable` (timeout-injectable so the test waits milliseconds, not 60 s).
    #[test]
    fn wait_until_ready_times_out_at_the_deadline() {
        let q = mk_queue();
        q.tts.suppress_heal_for_test(); // no spawn attempt → no error short-circuit
        let outcome = q.wait_until_ready_with_timeout(
            Some(ds_config::TtsEngine::Kokoro),
            q.generation.load(Ordering::SeqCst),
            Duration::from_millis(200),
        );
        assert_eq!(
            outcome,
            ReadyOutcome::Unavailable(
                "timed out waiting for the Kokoro model to become ready".to_string()
            )
        );
    }

    /// Regression (audit): a cached TTSLOADERR from an EARLIER load attempt must not drop a
    /// held item while the retry that `wait_until_ready` itself fires is still in flight —
    /// only a fresh error from this attempt is terminal. Uses the fake-helper fixture (see
    /// `tts::wedge_recovery_tests`) so `is_running()` is genuinely true and the entry retry
    /// path (clear stale error → `load_engine`) actually runs.
    #[test]
    fn wait_until_ready_ignores_a_stale_load_error_when_retrying() {
        let bin = crate::tts::wedge_recovery_tests::fake_helper_bin();
        let dir = tempfile::tempdir().unwrap();
        let q = TtsQueue::test_stub_with_helper(dir.path(), bin);
        q.tts.set_stderr_log_dir_for_test(dir.path().to_path_buf());
        // A transient failure cached by a PRIOR load attempt (e.g. an AV scan holding the
        // model file) — the exact state that used to fail the wait instantly.
        q.tts
            .set_tts_load_error_for_test("transient: model file locked");

        // The fake helper never loads TTS itself; flip residency while the wait polls, as
        // a successful retried load would. Flipped REPEATEDLY (not once): the heal is
        // asynchronous now, and its `start_locked` tail resets `tts_model` to Idle
        // (`tts_preload` is false in this stub) at whatever moment the fixture finishes
        // spawning — a single early flip raced that reset and lost. Re-asserting each
        // tick mirrors the real helper, which re-confirms TTSLOADED for every retried
        // `load` request.
        let stop_flipping = Arc::new(AtomicBool::new(false));
        let flipper = Arc::clone(&q);
        let stop = Arc::clone(&stop_flipping);
        let flip = std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
                flipper.tts.set_tts_loaded_for_test();
            }
        });

        let outcome = q.wait_until_ready(
            Some(ds_config::TtsEngine::Kokoro),
            q.generation.load(Ordering::SeqCst),
        );
        stop_flipping.store(true, Ordering::SeqCst);
        flip.join().unwrap();
        q.tts.set_enabled(false); // stop the spawned fixture
        assert_eq!(
            outcome,
            ReadyOutcome::Ready,
            "a stale TTSLOADERR must not drop the item its own retry is about to heal"
        );
    }

    /// Issue #59: the worker's readiness wait must NOT ride a wedged child's READY
    /// handshake. The fixture wedges pre-READY, so the heal's `start_locked` blocks for
    /// the full 3 s handshake bound — but the heal now runs on `heal_crashed_child`'s
    /// background thread, so the wait itself returns at ITS OWN (200 ms) deadline.
    /// Before this fix the wait called `restart_if_crashed` synchronously and sat inside
    /// the handshake (then unbounded: forever, mic closed, `stop_speech` unheard).
    #[test]
    fn wait_until_ready_does_not_block_on_a_wedged_child_spawn() {
        let bin = crate::tts::wedge_recovery_tests::fake_helper_bin();
        let dir = tempfile::tempdir().unwrap();
        let q = TtsQueue::test_stub_with_helper(dir.path(), bin);
        q.tts.set_stderr_log_dir_for_test(dir.path().to_path_buf());
        q.tts
            .set_spawn_env_for_test(&[("DONTSPEAK_FAKE_WEDGE_PRE_READY", "1")]);
        // Big enough that a wait that DID ride the handshake would visibly overshoot the
        // 1.5 s assertion below; small enough to keep the cleanup join bounded.
        q.tts
            .set_ready_timeout_for_test(Duration::from_millis(3000));

        let started = std::time::Instant::now();
        let outcome = q.wait_until_ready_with_timeout(
            Some(ds_config::TtsEngine::Kokoro),
            q.generation.load(Ordering::SeqCst),
            Duration::from_millis(200),
        );
        let elapsed = started.elapsed();
        assert_eq!(
            outcome,
            ReadyOutcome::Unavailable(
                "timed out waiting for the Kokoro model to become ready".to_string()
            )
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "the wait must return at its own 200 ms deadline, not the 3 s handshake bound \
             the async heal is stuck in — took {elapsed:?}"
        );
        // Bounded now (change A): waits out at most the rest of the heal's handshake.
        q.tts.set_enabled(false);
    }

    /// Residency observed by the poll returns `Ready` (the gate then re-checks holds).
    #[test]
    fn wait_until_ready_returns_ready_once_the_model_is_resident() {
        let q = mk_queue();
        q.tts.suppress_heal_for_test();
        q.tts.set_tts_loaded_for_test();
        assert_eq!(
            q.wait_until_ready(
                Some(ds_config::TtsEngine::Kokoro),
                q.generation.load(Ordering::SeqCst)
            ),
            ReadyOutcome::Ready
        );
    }

    /// Regression (audit): `pause_in_background` focus loss DURING the readiness wait must
    /// re-enter the hold gate once the model becomes ready — the old shape went straight to
    /// playback, speaking into whatever app was frontmost after up to 60 s of warm-up.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn gate_item_rechecks_the_focus_hold_after_the_readiness_wait() {
        let q = mk_queue();
        q.tts.suppress_heal_for_test();
        *q.config.lock().unwrap() = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::Kokoro],
            ..VoiceConfig::default()
        };
        q.set_terminal_front(true); // arm the self-disabling focus gate
        q.set_pause_in_background(true);

        let gen0 = q.generation.load(Ordering::SeqCst);
        let gated = Arc::clone(&q);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            done_tx
                .send(gated.gate_item(&item("held across warm-up"), gen0))
                .unwrap();
        });
        // The gate publishes in-flight once it passes the (currently clear) hold gate; give it
        // a beat more so it is inside `wait_until_ready`'s 50 ms poll loop.
        while !q.in_flight.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));

        q.set_terminal_front(false); // the user tabs away mid-wait…
        q.tts.set_tts_loaded_for_test(); // …then the model becomes ready

        let early = done_rx.recv_timeout(Duration::from_millis(600));
        assert!(
            matches!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "the item must stay held while no terminal is frontmost: {early:?}"
        );

        q.set_terminal_front(true);
        let outcome = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the gate returns once focus is back");
        assert!(matches!(outcome, GateOutcome::Play { .. }), "{outcome:?}");
        handle.join().unwrap();
    }

    #[test]
    fn active_session_prefers_explicit_over_recent_and_set_active_session_writes_explicit() {
        let q = mk_queue();
        assert_eq!(q.active_session(), None);

        // `enqueue` records the recency fallback.
        q.enqueue("hi".into(), None, None, Some("recent-sess".into()))
            .unwrap();
        assert_eq!(q.active_session(), Some("recent-sess".into()));

        // `set_active_session` writes the authoritative explicit pick, which wins.
        q.set_active_session(Some("explicit-sess".into()));
        assert_eq!(q.active_session(), Some("explicit-sess".into()));
    }

    #[test]
    fn set_terminal_front_latches_seen_and_set_pause_in_background_publishes() {
        let q = mk_queue();
        assert!(!q.terminal_seen.load(Ordering::SeqCst));

        q.set_terminal_front(false);
        assert!(
            !q.terminal_seen.load(Ordering::SeqCst),
            "front=false never latches seen"
        );
        assert!(!q.terminal_front.load(Ordering::SeqCst));

        q.set_terminal_front(true);
        assert!(
            q.terminal_seen.load(Ordering::SeqCst),
            "front=true latches seen"
        );
        assert!(q.terminal_front.load(Ordering::SeqCst));

        q.set_terminal_front(false); // tabbed away again
        assert!(
            q.terminal_seen.load(Ordering::SeqCst),
            "seen stays latched once a terminal has been seen"
        );
        assert!(!q.terminal_front.load(Ordering::SeqCst));

        q.set_pause_in_background(true);
        assert!(q.pause_in_background.load(Ordering::SeqCst));
        q.set_pause_in_background(false);
        assert!(!q.pause_in_background.load(Ordering::SeqCst));
    }

    #[test]
    fn worker_hold_state_wires_live_flags_into_hold_state() {
        // Just prove the composition is wired correctly — `hold_state` itself is the
        // oracle here, exercised (and truth-tabled) by its own dedicated test above.
        let q = mk_queue();
        for (pause_bg, seen, front) in [
            (false, false, false),
            (true, false, false),
            (true, true, false),
            (true, true, true),
        ] {
            q.pause_in_background.store(pause_bg, Ordering::SeqCst);
            q.terminal_seen.store(seen, Ordering::SeqCst);
            q.terminal_front.store(front, Ordering::SeqCst);
            let expected = hold_state(
                q.tts.is_full_duplex_active(),
                q.mic.is_active(),
                pause_bg,
                seen,
                front,
            );
            assert_eq!(
                q.worker_hold_state(),
                expected,
                "pause_bg={pause_bg} seen={seen} front={front}"
            );
        }
    }

    #[test]
    fn end_session_barges_the_window_and_forgets_its_pool_assignment() {
        let q = mk_queue();
        q.pool_assignments
            .lock()
            .unwrap()
            .insert("s1".to_string(), "af_sarah".to_string());
        q.items
            .lock()
            .unwrap()
            .extend([narr(Some("other")), narr(Some("s1"))]);
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing_session.lock().unwrap() = Some(Some("s1".to_string()));
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.end_session(Some("s1".to_string()));

        // Per-window barge: s1's queued item pruned, the other session's kept.
        let remaining: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(remaining, vec![Some("other".to_string())]);
        // s1's own in-flight item is cancelled (playing_session matched).
        assert!(q.generation.load(Ordering::SeqCst) > gen_before);
        assert!(!q.tts_active.load(Ordering::SeqCst));
        // The pool assignment is forgotten so it doesn't linger past this window's life.
        assert!(!q.pool_assignments.lock().unwrap().contains_key("s1"));
    }

    #[test]
    fn assign_pool_voice_records_and_reuses_the_pick_per_session() {
        let q = mk_queue();
        let pool = vec!["af_sarah".to_string(), "am_adam".to_string()];

        let v1 = q.assign_pool_voice("sess-a", &pool);
        assert_eq!(v1, "af_sarah");
        assert_eq!(q.pool_assignments.lock().unwrap().get("sess-a"), Some(&v1));
        // The same session reuses its recorded pick.
        assert_eq!(q.assign_pool_voice("sess-a", &pool), v1);
        // A different session gets the next untaken pool voice.
        assert_eq!(q.assign_pool_voice("sess-b", &pool), "am_adam");
    }

    #[test]
    fn resolve_engine_voice_off_returns_none() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: Vec::new(), // empty ladder = off
            ..VoiceConfig::default()
        };
        assert_eq!(q.resolve_engine_voice(&cfg, &None), None);
    }

    // System is only buildable on macOS/Windows (see `system_tts_buildable_on`) — gated like
    // `TtsManager::stop`'s own System-only path, so this stays meaningful cross-platform
    // rather than silently skipping the assertion on the platforms that actually run it.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn resolve_engine_voice_system_returns_the_configured_voice_verbatim() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::System],
            tts_system_voice: "Ava (Premium)".to_string(),
            ..VoiceConfig::default()
        };
        assert_eq!(
            q.resolve_engine_voice(&cfg, &None),
            Some((ds_config::TtsEngine::System, "Ava (Premium)".to_string()))
        );
    }

    // Kokoro is usable everywhere EXCEPT Intel macOS without an onnxruntime dylib present
    // (a runtime capability, not a static (os,arch) fact — see `intel_mac_builtin_ort_available`),
    // so gate out only that one platform, matching `voice.rs`'s own tests of this ladder.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_kokoro_with_pool_and_session_delegates_to_pool_assignment() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::Kokoro],
            tts_built_in_voices: vec!["af_sarah".to_string(), "am_adam".to_string()],
            ..VoiceConfig::default()
        };
        let (engine, voice) = q
            .resolve_engine_voice(&cfg, &Some("sess-1".to_string()))
            .expect("Kokoro is usable on this build");
        assert_eq!(engine, ds_config::TtsEngine::Kokoro);
        assert_eq!(voice, "af_sarah", "claims the first untaken pool voice");
        // The session id is threaded through to the pool-assignment map.
        assert_eq!(
            q.pool_assignments.lock().unwrap().get("sess-1"),
            Some(&voice)
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_kokoro_falls_back_to_current_voice_without_pool_or_session() {
        let q = mk_queue();
        let with_pool = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::Kokoro],
            tts_built_in_voices: vec!["af_sarah".to_string(), "am_adam".to_string()],
            ..VoiceConfig::default()
        };
        // Non-empty pool, but NO session id → falls back to `current_voice()`, untracked.
        assert_eq!(
            q.resolve_engine_voice(&with_pool, &None),
            Some((ds_config::TtsEngine::Kokoro, with_pool.current_voice()))
        );
        assert!(q.pool_assignments.lock().unwrap().is_empty());

        // A session id, but an EMPTY pool → also falls back to `current_voice()`.
        let empty_pool = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::Kokoro],
            tts_built_in_voices: vec![],
            ..VoiceConfig::default()
        };
        assert_eq!(
            q.resolve_engine_voice(&empty_pool, &Some("sess-2".to_string())),
            Some((ds_config::TtsEngine::Kokoro, empty_pool.current_voice()))
        );
    }
}
