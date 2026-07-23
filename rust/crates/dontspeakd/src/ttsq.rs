//! Engine-owned audio queue — single serializer for speech and earcons.
//!
//! Bounded FIFO; one worker on the warm child. Ordering, mic gate, barge/pause
//! live here (child stays dumb). Exception: needs-input under focus hold
//! ([`TtsQueue::dispatch_earcon`]).
//!
//! Focus (`pause_bg`): hold off-terminal after first sighting. Half-duplex record
//! barge pauses; full-duplex coexists. Hard barge clears.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use ds_config::{Paths, TtsArgPools, VoiceConfig, WiredAgent};

use crate::status::StatusGate;
use crate::tts::TtsManager;

struct HealingGuard(Arc<AtomicBool>);
impl Drop for HealingGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// In-flight claim. `session: None` = untagged/global.
#[derive(Clone, Debug)]
struct PlayingClaim {
    source: Option<WiredAgent>,
    session: Option<String>,
    /// Speech, not a cue — the in-flight half of the depth the status reports.
    speech: bool,
    /// Record for this utterance: id from admit, voice/language filled once the play gate
    /// resolves them, `outcome` only when it is written to [`TtsQueue::utterances`].
    /// `None` for a cue, which has no utterance to report.
    utterance: Option<ds_status::UtteranceStatus>,
}

struct PlayingGuard<'a>(&'a Mutex<Option<PlayingClaim>>);
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

static GREET_ROTATION: AtomicUsize = AtomicUsize::new(0);

/// Admission IDs outlive playback (producer retry); SessionEnd prunes; cap bounds leaks.
const ACCEPTED_NARRATION_IDS_MAX: usize = 8192;

/// Terminal utterance records kept for `model_status`. Deep enough that a producer polling
/// after its own `speak` still finds its handle behind a reply's worth of narration chunks,
/// shallow enough to ride every status push.
const RECENT_UTTERANCES_MAX: usize = 16;

#[derive(Default)]
struct AcceptedNarrations {
    seen: HashSet<String>,
    order: VecDeque<(String, Option<String>)>,
}

impl AcceptedNarrations {
    fn insert(&mut self, id: String, session: Option<String>) {
        if !self.seen.insert(id.clone()) {
            return;
        }
        self.order.push_back((id, session));
        while self.order.len() > ACCEPTED_NARRATION_IDS_MAX {
            if let Some((old, _)) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
    }

    fn forget_session(&mut self, session: &str) {
        self.order
            .retain(|(id, owner)| owner.as_deref() != Some(session) || !self.seen.remove(id));
    }
}

/// Per-item text cap (IPC + in-process) — enforced here so wire can't bypass.
pub(crate) const MAX_SPEAK_BYTES: usize = 10 * 1024;

/// The detection corpus rides the same wire line as spoken text, so the producer-side cap
/// must not exceed what this queue accepts. Enforced, not just documented.
const _: () = assert!(ds_narrate::DETECTION_TEXT_MAX_BYTES == MAX_SPEAK_BYTES);

/// Pending bounds under stalled focus/mic (speech vs cue quotas).
const MAX_PENDING_ITEMS: usize = 128;
const MAX_PENDING_CUES: usize = 64;
const MAX_PENDING_BYTES: usize = 1024 * 1024;
const MAX_SESSION_PENDING_ITEMS: usize = 32;
const MAX_SESSION_PENDING_CUES: usize = 16;
const MAX_SESSION_PENDING_BYTES: usize = 256 * 1024;

/// `{n}` = voice display name.
const GREETINGS: &[&str] = &[
    "{n} here — I'm with you today.",
    "Hey, it's {n}. Ready when you are.",
    "{n} speaking. Let's get into it.",
    "{n} here. Good to see you.",
    "{n} with you. Let's go.",
    "Hi, {n} here — what are we building?",
];

/// System / empty `tts_voices.system`.
const GREETINGS_ANON: &[&str] = &[
    "I'm with you today.",
    "Ready when you are.",
    "Let's get into it.",
    "Good to see you.",
    "With you. Let's go.",
    "What are we building?",
];

fn greeting_line(name: Option<&str>, idx: usize) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => GREETINGS[idx % GREETINGS.len()].replace("{n}", n),
        None => GREETINGS_ANON[idx % GREETINGS_ANON.len()].to_string(),
    }
}

fn playback_rate(cfg: &VoiceConfig, engine: Option<ds_config::TtsEngine>) -> f32 {
    match engine {
        Some(ds_config::TtsEngine::System) => cfg.system_rate(),
        Some(ds_config::TtsEngine::BuiltIn) if cfg.tts_model.descriptor().supports_rate => {
            cfg.model_rate(cfg.tts_model)
        }
        _ => 1.0,
    }
}

fn apply_tts_arg_params(
    cfg: &mut VoiceConfig,
    engine: Option<ds_config::TtsEngine>,
    args: Option<&ds_config::TtsTargetArgs>,
) {
    let (Some(engine), Some(args)) = (engine, args) else {
        return;
    };
    match engine {
        ds_config::TtsEngine::System => cfg.tts_params.system.extend(args.params().clone()),
        ds_config::TtsEngine::BuiltIn => cfg
            .tts_params
            .for_model_mut(cfg.tts_model)
            .extend(args.params().clone()),
    }
}

/// Reuse if still in `pool`; else free; else least-loaded. `pool` non-empty; `roll(n) < n`.
/// Load is counted only among agents holding a voice for the SAME language, so spreading
/// voices across agents stays independent per language.
fn pick_agent_voice(
    assignments: &HashMap<(Option<WiredAgent>, String), String>,
    pool: &[String],
    agent: &(Option<WiredAgent>, String),
    roll: &mut dyn FnMut(usize) -> usize,
) -> String {
    if let Some(v) = assignments.get(agent)
        && pool.iter().any(|p| p == v)
    {
        return v.clone();
    }
    let load = |v: &str| {
        assignments
            .iter()
            .filter(|(held_by, held)| {
                held_by != &agent && held_by.1 == agent.1 && held.as_str() == v
            })
            .count()
    };
    let free: Vec<&String> = pool.iter().filter(|v| load(v) == 0).collect();
    let candidates = if free.is_empty() {
        let min = pool.iter().map(|v| load(v)).min().unwrap_or(0);
        pool.iter().filter(|v| load(v) == min).collect()
    } else {
        free
    };
    candidates[roll(candidates.len())].clone()
}

#[derive(Debug)]
enum QueueAction {
    Speech {
        text: String,
        /// ISO code detected for this chunk at admit; clamped to the live model at play.
        language: String,
        tts_args: Option<Box<TtsArgPools>>,
    },
    Earcon(ds_earcon::EarconEvent),
}

impl QueueAction {
    fn pending_bytes(&self) -> usize {
        match self {
            Self::Speech { text, .. } => text.len(),
            Self::Earcon(_) => 0,
        }
    }

    fn speech_text(&self) -> Option<&str> {
        match self {
            Self::Speech { text, .. } => Some(text),
            Self::Earcon(_) => None,
        }
    }

    fn requeueable(&self) -> bool {
        self.speech_text()
            .is_none_or(|text| !text.trim().is_empty())
    }
}

/// Ordered audio. Earcons share session/cancel path (cannot overtake narration).
struct Item {
    action: QueueAction,
    /// Producer → `activity.speaker` while in flight.
    source: Option<WiredAgent>,
    /// Session tag (`None` = global). Voice keys off `source`.
    session: Option<String>,
    /// Batch resume high-water (`PROGRESS`). Record-barge resume sends as `skip`.
    resume_skip: usize,
    /// Handle `speak` hands back, and the key of this utterance's terminal record.
    /// `None` for a cue. Survives a requeue so a resumed utterance keeps its handle.
    utterance_id: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TtsStatusSample {
    pub speaking: bool,
    pub speaker: Option<WiredAgent>,
    pub queued: u64,
    pub utterance: Option<ds_status::UtteranceStatus>,
    /// Terminal records, most recent first.
    pub recent_utterances: Vec<ds_status::UtteranceStatus>,
}

/// Pending utterances in `q`. Earcons are cues rather than things to say, so they are not
/// counted: the depth the status reports is "how much is there left to speak".
fn speech_depth(q: &VecDeque<Item>) -> usize {
    q.iter()
        .filter(|item| item.action.speech_text().is_some())
        .count()
}

/// Active terminal: last `MarkActive` (`explicit`), else most-recent enqueue.
#[derive(Default)]
struct ActiveSel {
    explicit: Option<String>,
    recent: Option<String>,
}

impl ActiveSel {
    fn effective(&self) -> Option<String> {
        self.explicit.clone().or_else(|| self.recent.clone())
    }
}

/// Grok Stop sticky: digests/earcons under `grok-stop:<real>` survive
/// MarkActive `clear_on_input=[current]` exact prune. Voice still from `source`.
const GROK_STOP_STICKY_PREFIX: &str = "grok-stop:";

/// `abc` → `grok-stop:abc`. Already sticky / bare → `None`.
fn grok_stop_sticky_sibling(session: &str) -> Option<String> {
    if session.starts_with(GROK_STOP_STICKY_PREFIX) || session == "grok-stop" {
        None
    } else {
        Some(format!("{GROK_STOP_STICKY_PREFIX}{session}"))
    }
}

/// Exact or Grok sticky for `keep` (`clear_on_input=[other]`).
fn session_is_keep_or_sticky(item: &Option<String>, keep: &Option<String>) -> bool {
    if item == keep {
        return true;
    }
    match (item.as_deref(), keep.as_deref()) {
        (Some(i), Some(k)) => i.strip_prefix(GROK_STOP_STICKY_PREFIX) == Some(k),
        _ => false,
    }
}

/// Exact or sticky-under-real `target` (real is not under a sticky target).
fn session_belongs_to_real(session: &Option<String>, target: &str) -> bool {
    match session.as_deref() {
        Some(s) if s == target => true,
        Some(s) => s.strip_prefix(GROK_STOP_STICKY_PREFIX) == Some(target),
        None => false,
    }
}

/// Drop what `keep` rejects, returning the handles of the utterances that will never be
/// spoken — oldest first, so pushing them onto the record ring in order leaves newest first.
fn discard_items(q: &mut VecDeque<Item>, keep: impl Fn(&Item) -> bool) -> Vec<u64> {
    let mut discarded = Vec::new();
    q.retain(|it| {
        if keep(it) {
            return true;
        }
        discarded.extend(it.utterance_id);
        false
    });
    discarded
}

/// Drop only exact `target`. Sticky survives MarkActive current-clear; Stop/SessionEnd
/// also clear the sticky sibling.
fn prune_session(q: &mut VecDeque<Item>, target: &Option<String>) -> Vec<u64> {
    discard_items(q, |it| &it.session != target)
}

/// Keep only `keep` + sticky sibling.
fn retain_only_session(q: &mut VecDeque<Item>, keep: &Option<String>) -> Vec<u64> {
    discard_items(q, |it| session_is_keep_or_sticky(&it.session, keep))
}

/// Untagged, exact, or sticky under `active`.
fn session_preferred_for_active(item_session: &Option<String>, active: &str) -> bool {
    match item_session {
        None => true,
        Some(s) if s == active => true,
        Some(s) => s
            .strip_prefix(GROK_STOP_STICKY_PREFIX)
            .is_some_and(|real| real == active),
    }
}

/// Prefer active/untagged/sticky; else FIFO (avoids starving backgrounded windows).
fn select_pos(q: &VecDeque<Item>, active: &Option<String>) -> Option<usize> {
    match active {
        None => (!q.is_empty()).then_some(0),
        Some(active_id) => q
            .iter()
            .position(|it| session_preferred_for_active(&it.session, active_id))
            .or_else(|| (!q.is_empty()).then_some(0)),
    }
}

/// Worker claim gate: a paused queue claims nothing, however selectable the head is.
fn claimable_pos(paused: bool, q: &VecDeque<Item>, active: &Option<String>) -> Option<usize> {
    if paused {
        return None;
    }
    select_pos(q, active)
}

/// Asymmetric: Dictation wins; barge auto-resume is no-op under Dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseCause {
    /// Caps/PTT; cleared only by matching `resume()`.
    Dictation,
    /// Foreign-mic watcher; only cause `resume_if_barge_speculative` clears.
    BargeSpeculative,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PausedState {
    paused: bool,
    cause: Option<PauseCause>,
}

pub struct TtsQueue {
    /// Every mutation must publish the new depth via [`TtsQueue::publish_queue_depth`]
    /// before releasing the guard — `queue_depth` mirrors [`speech_depth`].
    items: Mutex<VecDeque<Item>>,
    /// Lock-free mirror of pending SPEECH depth for status reads. `items` is held across
    /// helper-child I/O (`hard_cancel_in_flight` → `stop_fade`), so a status read that
    /// took the lock would stall behind a wedged helper.
    queue_depth: AtomicU64,
    /// Lock before `items` (see enqueue_narration).
    accepted_narrations: Mutex<AcceptedNarrations>,
    cv: Condvar,
    /// Barge/pause gen; worker abandons when gen advances.
    generation: AtomicU64,
    /// One lock so barge can't relabel Dictation during pause→`set_stt_active` race.
    paused: Mutex<PausedState>,
    /// Requeue intent by cancel's pre-bump gen (`true` = record-barge). Lookup uses that
    /// gen (not live `paused`) so a later pause can't resurrect a cleared item.
    cancel_kind: Mutex<HashMap<u64, bool>>,
    tts_active: AtomicBool,
    /// Readiness/mic holds set this; focus holds leave it idle (always-listen off-terminal).
    in_flight: AtomicBool,
    /// Poll thread → worker (NSWorkspace not worker-safe). Init true.
    terminal_front: AtomicBool,
    /// First frontmost sighting; unrecognized terminals stay unmuted.
    terminal_seen: AtomicBool,
    /// Focus gate. Init false; first poll applies config.
    pause_bg: AtomicBool,
    config: Mutex<VoiceConfig>,
    /// Lazy pool roll; SessionEnd keeps assignment. Keyed per language so an agent holds one
    /// voice per language it speaks instead of flapping when a reply switches language.
    /// Bounded by wired-client cardinality plus one unwired bucket × languages spoken.
    agent_voices: Mutex<HashMap<(Option<WiredAgent>, String), String>>,
    /// `say -v ?` enumeration, read once per process for System language matching.
    system_voices: OnceLock<Vec<ds_tts::SpeakerVoice>>,
    /// Always acquired inside `items`.
    active: Mutex<ActiveSel>,
    playing: Mutex<Option<PlayingClaim>>,
    /// Monotonic per process; `0` is never issued, so it cannot be confused with a default.
    next_utterance_id: AtomicU64,
    /// Terminal records, most recent first, capped at [`RECENT_UTTERANCES_MAX`].
    utterances: Mutex<VecDeque<ds_status::UtteranceStatus>>,
    tts: Arc<TtsManager>,
    gate: Arc<StatusGate>,
    /// MarkActive de-dups auto-Enter vs real text submit.
    last_voice_submit: Mutex<Option<Instant>>,
    /// Cached mic-in-use (worker reads cache, not device).
    mic: ds_platform::MicState,
    healing: Arc<AtomicBool>,
    oob_cue: AtomicBool,
}

impl TtsQueue {
    pub fn start(
        tts: Arc<TtsManager>,
        paths: Paths,
        gate: Arc<StatusGate>,
        mic: ds_platform::MicState,
    ) -> Arc<Self> {
        let config = VoiceConfig::load(&paths);
        let q = Arc::new(Self {
            items: Mutex::new(VecDeque::new()),
            queue_depth: AtomicU64::new(0),
            accepted_narrations: Mutex::new(AcceptedNarrations::default()),
            cv: Condvar::new(),
            generation: AtomicU64::new(0),
            paused: Mutex::new(PausedState::default()),
            cancel_kind: Mutex::new(HashMap::new()),
            tts_active: AtomicBool::new(false),
            in_flight: AtomicBool::new(false),
            terminal_front: AtomicBool::new(true),
            terminal_seen: AtomicBool::new(false),
            pause_bg: AtomicBool::new(false),
            config: Mutex::new(config),
            agent_voices: Mutex::new(HashMap::new()),
            system_voices: OnceLock::new(),
            active: Mutex::new(ActiveSel::default()),
            last_voice_submit: Mutex::new(None),
            playing: Mutex::new(None),
            next_utterance_id: AtomicU64::new(1),
            utterances: Mutex::new(VecDeque::new()),
            tts,
            gate,
            mic,
            healing: Arc::new(AtomicBool::new(false)),
            oob_cue: AtomicBool::new(false),
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

    /// No worker; unstarted helper → not STT-ready.
    #[cfg(test)]
    pub(crate) fn test_stub() -> Arc<Self> {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("ds-test-nonexistent-helper");
        Self::test_stub_with_helper(
            dir.path(),
            helper,
            crate::tts::TtsManagerTestOptions::default(),
        )
    }

    /// Real helper path for readiness tests. Keep `root` alive if spawning.
    #[cfg(test)]
    pub(crate) fn test_stub_with_helper(
        root: &std::path::Path,
        helper: std::path::PathBuf,
        test_options: crate::tts::TtsManagerTestOptions,
    ) -> Arc<Self> {
        let paths = Paths::rooted_at(root);
        let tts = Arc::new(TtsManager::new_for_test(
            helper,
            paths.log_file.clone(),
            Arc::new(crate::stats::TtsStats::new()),
            Arc::new(crate::stats::SttStats::new()),
            Arc::new(crate::stats::LifetimeSeconds::load(
                root.join("ds-ttsq-test-lifetime.json"),
            )),
            test_options,
        ));
        // MicState only via watcher; drop freezes last reading.
        let mic = ds_platform::MicWatcher::spawn(|_| {}).handle();
        Arc::new(TtsQueue {
            items: Mutex::new(VecDeque::new()),
            queue_depth: AtomicU64::new(0),
            accepted_narrations: Mutex::new(AcceptedNarrations::default()),
            cv: Condvar::new(),
            generation: AtomicU64::new(0),
            paused: Mutex::new(PausedState::default()),
            cancel_kind: Mutex::new(HashMap::new()),
            tts_active: AtomicBool::new(false),
            in_flight: AtomicBool::new(false),
            terminal_front: AtomicBool::new(true),
            terminal_seen: AtomicBool::new(false),
            pause_bg: AtomicBool::new(false),
            config: Mutex::new(VoiceConfig::load(&paths)),
            agent_voices: Mutex::new(HashMap::new()),
            system_voices: OnceLock::new(),
            active: Mutex::new(ActiveSel::default()),
            last_voice_submit: Mutex::new(None),
            playing: Mutex::new(None),
            next_utterance_id: AtomicU64::new(1),
            utterances: Mutex::new(VecDeque::new()),
            tts,
            gate: StatusGate::new(),
            mic,
            healing: Arc::new(AtomicBool::new(false)),
            oob_cue: AtomicBool::new(false),
        })
    }

    #[cfg(test)]
    pub(crate) fn set_active_for_test(&self, on: bool) {
        self.tts_active.store(on, Ordering::SeqCst);
    }

    /// Enqueue speech (empty ignored). Optional target args; `source` → `activity.speaker`.
    /// `Ok(None)` = nothing to say, so no handle exists.
    pub fn enqueue(
        &self,
        text: String,
        tts_args: Option<TtsArgPools>,
        source: Option<WiredAgent>,
        session: Option<String>,
    ) -> Result<Option<u64>, String> {
        if text.len() > MAX_SPEAK_BYTES {
            return Err(format!("text exceeds the {MAX_SPEAK_BYTES}-byte limit"));
        }
        if text.trim().is_empty() {
            return Ok(None);
        }
        // MCP Speak / greeting: the chunk is all there is, so it stands on its own text.
        self.enqueue_action(
            QueueAction::Speech {
                language: self.chunk_language(&text, None),
                text,
                tts_args: tts_args.map(Box::new),
            },
            source,
            session,
        )
    }

    /// Enqueue ordered cue. Sound/mute resolved at dequeue (later mute suppresses).
    pub fn enqueue_earcon(
        &self,
        event: ds_earcon::EarconEvent,
        source: Option<WiredAgent>,
        session: Option<String>,
    ) -> Result<(), String> {
        self.enqueue_action(QueueAction::Earcon(event), source, session)
            .map(|_| ())
    }

    /// Route earcon: ordered queue by default.
    /// Exception: needs-input under focus hold + idle → immediate detached play
    /// (alert when user left the terminal). Idle gate required: focus holds only at
    /// item boundaries; oob must not mix over in-flight speech. Thread re-checks
    /// before play and falls back to queue on TOCTOU (speak starting mid-dispatch).
    ///
    /// Bypass properties:
    /// * one `oob_cue` thread (bursts coalesce)
    /// * does not set `tts_active`/`in_flight` (`is_busy` must stay false under hold)
    /// * not a queue item / not in `playing` — only global cancel/mute/stopfade stops it
    /// * no warm child → same "TTS child not running" as queued path
    pub fn dispatch_earcon(
        self: &Arc<Self>,
        event: ds_earcon::EarconEvent,
        source: Option<WiredAgent>,
        session: Option<String>,
    ) -> Result<(), String> {
        // reply_done never bypasses — skip hold-state snapshot.
        if matches!(event, ds_earcon::EarconEvent::NeedsInput)
            && earcon_bypasses_queue(
                event,
                self.worker_hold_state(),
                !self.tts_active.load(Ordering::SeqCst),
            )
        {
            // Single-flight: in-flight oob → Ok, nothing enqueued.
            if self
                .oob_cue
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Ok(());
            }
            let worker = self.clone();
            let thread_session = session.clone();
            let thread_source = source;
            let spawned = std::thread::Builder::new()
                .name("ds-cue-oob".into())
                .spawn(move || {
                    let _clear = InFlightGuard(&worker.oob_cue);
                    // Re-check: speak/mic may have started; fall back to queue.
                    if !earcon_bypasses_queue(
                        event,
                        worker.worker_hold_state(),
                        !worker.tts_active.load(Ordering::SeqCst),
                    ) {
                        if let Err(e) =
                            worker.enqueue_earcon(event, thread_source, thread_session)
                        {
                            log::warn!(target: "ttsq", "out-of-band cue fallback enqueue failed: {e}");
                        }
                        return;
                    }
                    if let Err(e) = worker.cue_one(event) {
                        log::warn!(target: "ttsq", "out-of-band cue failed: {e}");
                    }
                });
            match spawned {
                Ok(_) => return Ok(()),
                Err(e) => {
                    // Spawn failed: clear flag, fall through to ordered queue.
                    log::error!(target: "ttsq", "failed to spawn out-of-band cue thread: {e}");
                    self.oob_cue.store(false, Ordering::SeqCst);
                }
            }
        }
        self.enqueue_earcon(event, source, session)
    }

    /// `Ok(Some(id))` for admitted speech; `Ok(None)` for a cue, which has no handle.
    fn enqueue_action(
        &self,
        action: QueueAction,
        source: Option<WiredAgent>,
        session: Option<String>,
    ) -> Result<Option<u64>, String> {
        let mut q = self.items.lock().unwrap();
        let is_cue = matches!(&action, QueueAction::Earcon(_));
        let pending_items = q
            .iter()
            .filter(|item| matches!(&item.action, QueueAction::Earcon(_)) == is_cue)
            .count();
        let max_pending_items = if is_cue {
            MAX_PENDING_CUES
        } else {
            MAX_PENDING_ITEMS
        };
        if pending_items >= max_pending_items {
            let kind = if is_cue { "audio cue" } else { "speech" };
            return Err(format!(
                "{kind} queue is full ({max_pending_items} pending items)"
            ));
        }
        let pending_bytes = q.iter().try_fold(0usize, |total, item| {
            total.checked_add(item.action.pending_bytes())
        });
        if !matches!(
            pending_bytes.and_then(|n| n.checked_add(action.pending_bytes())),
            Some(total) if total <= MAX_PENDING_BYTES
        ) {
            return Err(format!(
                "speech queue is full ({MAX_PENDING_BYTES} pending text bytes)"
            ));
        }
        let mut session_items = 0usize;
        let mut session_bytes = 0usize;
        for item in q.iter().filter(|item| item.session == session) {
            if matches!(&item.action, QueueAction::Earcon(_)) == is_cue {
                session_items += 1;
            }
            session_bytes = session_bytes.saturating_add(item.action.pending_bytes());
        }
        let max_session_pending_items = if is_cue {
            MAX_SESSION_PENDING_CUES
        } else {
            MAX_SESSION_PENDING_ITEMS
        };
        if session_items >= max_session_pending_items {
            let kind = if is_cue { "audio cue" } else { "speech" };
            return Err(format!(
                "session {kind} queue is full ({max_session_pending_items} pending items)"
            ));
        }
        if !matches!(
            session_bytes.checked_add(action.pending_bytes()),
            Some(total) if total <= MAX_SESSION_PENDING_BYTES
        ) {
            return Err(format!(
                "session speech queue is full ({MAX_SESSION_PENDING_BYTES} pending text bytes)"
            ));
        }
        self.note_recent(&session);
        // Issued only past admission, so a rejected enqueue never burns a handle.
        let utterance_id = action
            .speech_text()
            .is_some()
            .then(|| self.next_utterance_id.fetch_add(1, Ordering::SeqCst));
        let before = speech_depth(&q);
        q.push_back(Item {
            action,
            source,
            session,
            resume_skip: 0,
            utterance_id,
        });
        self.publish_queue_depth(before, speech_depth(&q));
        self.cv.notify_one();
        Ok(utterance_id)
    }

    /// Admit narration by id once. Dup id → Ok even if queue full; rejected first stays retryable.
    /// Optional `detection_text` is the turn corpus behind this chunk's language.
    pub fn enqueue_narration(
        &self,
        text: String,
        source: Option<WiredAgent>,
        session: Option<String>,
        narration_id: Option<String>,
        detection_text: Option<String>,
    ) -> Result<(), String> {
        if text.len() > MAX_SPEAK_BYTES {
            return Err(format!("text exceeds the {MAX_SPEAK_BYTES}-byte limit"));
        }
        if text.trim().is_empty() {
            return Ok(());
        }
        // Re-cap the detection corpus (an old or hand-rolled producer may not have);
        // never reject on detection size alone.
        let detection_text = detection_text
            .filter(|s| !s.trim().is_empty())
            .map(ds_narrate::cap_detection_text);
        let action = QueueAction::Speech {
            language: self.chunk_language(&text, detection_text.as_deref()),
            text,
            tts_args: None,
        };

        if let Some(id) = narration_id {
            let mut accepted = self.accepted_narrations.lock().unwrap();
            if accepted.seen.contains(&id) {
                return Ok(());
            }
            // Lock order: accepted_narrations before items (existing).
            self.enqueue_action(action, source, session.clone())?;
            accepted.insert(id, session);
        } else {
            self.enqueue_action(action, source, session)?;
        }
        Ok(())
    }

    /// Fresh in-flight record for a claimed item: the handle is known at admit, the
    /// resolution only once the play gate runs.
    fn claimed_utterance(item: &Item) -> Option<ds_status::UtteranceStatus> {
        item.utterance_id.map(|id| ds_status::UtteranceStatus {
            id,
            voice: None,
            language: None,
            warning: None,
            outcome: None,
        })
    }

    /// Language for one chunk, decided at admit so the queued item carries it. `corpus` is
    /// the turn text the producer sent, used only when the chunk cannot classify itself.
    fn chunk_language(&self, text: &str, corpus: Option<&str>) -> String {
        // Config Copy, so the lock drops before detection runs (same as play_speech).
        let cfg = self.config.lock().unwrap();
        match cfg.resolved_tts() {
            Some(ds_config::TtsEngine::System) => ds_tts::chunk_language_any(text, corpus),
            _ => ds_tts::chunk_language(text, corpus, cfg.tts_model),
        }
    }

    pub fn forget_narration_session(&self, session: &str) {
        self.accepted_narrations
            .lock()
            .unwrap()
            .forget_session(session);
    }

    /// Claim under `items`: publish `playing` so clears can't race mid-claim.
    /// The caller snapshots the cancel generation under this same guard BEFORE its pause
    /// check — any bump between that snapshot and the claim invalidates the claim, which is
    /// intended: only `pause_with_cause` bumps without holding `items` (clear /
    /// clear_session / cancel_for_submit / hard_cancel_in_flight_locked all hold it).
    /// Do not load the generation here — that is what absorbed the pause's bump.
    fn claim_item(&self, q: &mut VecDeque<Item>, pos: usize) -> Item {
        let before = speech_depth(q);
        let item = q.remove(pos).expect("select_pos returns a valid index");
        self.publish_queue_depth(before, speech_depth(q));
        self.in_flight.store(true, Ordering::SeqCst);
        *self.playing.lock().unwrap() = Some(PlayingClaim {
            source: item.source,
            session: item.session.clone(),
            speech: item.action.speech_text().is_some(),
            utterance: Self::claimed_utterance(&item),
        });
        item
    }

    /// Update recency fallback. Caller holds `items` (lock order: items → active).
    fn note_recent(&self, session: &Option<String>) {
        self.active.lock().unwrap().recent = session.clone();
    }

    /// Global mute on warm child (speech silent; cues suppressed).
    pub fn set_muted(&self, on: bool) {
        self.tts.set_muted(on);
    }

    /// Hard-cancel in-flight: clear tts_active, gen bump, drop intent, fade.
    /// Does not touch `items`/`paused`. `skip_current` skips the tts_active toggle.
    fn hard_cancel_in_flight(&self) {
        self.set_tts_active(false);
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        self.record_cancel_kind(gen0, false);
        self.tts.stop_fade();
    }

    /// Like hard cancel; `_items` witnesses prune + playing snapshot + bump share one section.
    fn hard_cancel_in_flight_locked(&self, _items: &MutexGuard<'_, VecDeque<Item>>) {
        self.hard_cancel_in_flight();
    }

    /// Global hard barge: clear queue, cancel in-flight, clear pause (fade out).
    pub fn clear(&self) {
        let mut items = self.items.lock().unwrap();
        let before = speech_depth(&items);
        let discarded = discard_items(&mut items, |_| false);
        self.publish_queue_depth(before, speech_depth(&items));
        drop(items);
        // Never while holding `items`: that guard spans helper I/O, and status reads the
        // record ring.
        self.record_discarded(&discarded);
        *self.paused.lock().unwrap() = PausedState::default();
        self.hard_cancel_in_flight();
        self.cv.notify_one();
    }

    /// Skip in-flight only (caps double-tap). Keeps queue; no-op if nothing playing.
    pub fn skip_current(&self) {
        // Leave items/paused/tts_active alone — worker re-asserts on next dequeue.
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        self.record_cancel_kind(gen0, false);
        self.tts.stop_fade();
        self.cv.notify_one();
    }

    /// Per-window barge: drop this session's queue; cancel in-flight only if matching.
    /// `Stop { session: None }` → [`clear`](Self::clear).
    pub fn clear_session(&self, session: Option<String>) {
        // items → playing same order as worker; prune + snapshot under one lock.
        let mut items = self.items.lock().unwrap();
        let before = speech_depth(&items);
        let discarded = prune_session(&mut items, &session);
        self.publish_queue_depth(before, speech_depth(&items));
        let cancel_current = self
            .playing
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|p| p.session == session);
        if cancel_current {
            self.hard_cancel_in_flight_locked(&items);
        }
        drop(items);
        self.record_discarded(&discarded);
        self.cv.notify_one();
    }

    /// Active session snapshot — resolve once for multi-scope `clear_on_input`.
    pub fn active_session(&self) -> Option<String> {
        self.active.lock().unwrap().effective()
    }

    /// Apply `clear_on_input` against one resolved `target`. No-op if `target` is None
    /// or neither scope requested.
    ///
    /// `current`: prune target (+ sticky) and hard-cancel in-flight iff it is the target's
    /// ([`session_belongs_to_real`] — the same set the prune removes).
    /// Gate on `playing`, never `tts_active`: a record-barge pause clears `tts_active` while
    /// the item is still claimed, so a `tts_active` gate would leak.
    /// `other`: retain only target (+ sticky); cancel in-flight iff not target's.
    /// Both scopes together empty the queue.
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
            let mut discarded;
            {
                let mut items = self.items.lock().unwrap();
                let before = speech_depth(&items);
                // Snapshot `playing` under `items` (the worker's claim order, like the `other`
                // branch) so a claim can't slip in between the prune and the decision.
                let playing_is_target = self
                    .playing
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|p| session_belongs_to_real(&p.session, target.as_str()));
                discarded = prune_session(&mut items, &Some(target.clone()));
                // Voice submit path (not MarkActive): also drop sticky of same terminal.
                if let Some(sticky) = grok_stop_sticky_sibling(&target) {
                    discarded.extend(prune_session(&mut items, &Some(sticky)));
                }
                self.publish_queue_depth(before, speech_depth(&items));
                if playing_is_target {
                    self.hard_cancel_in_flight_locked(&items);
                }
            }
            self.record_discarded(&discarded);
        }
        if cancel_other {
            let discarded;
            {
                // Prune + playing snapshot under items (avoid claim race after return).
                let mut items = self.items.lock().unwrap();
                let before = speech_depth(&items);
                let playing_is_other = self
                    .playing
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|p| !session_belongs_to_real(&p.session, target.as_str()));
                discarded = retain_only_session(&mut items, &Some(target));
                self.publish_queue_depth(before, speech_depth(&items));
                if playing_is_other {
                    self.hard_cancel_in_flight_locked(&items);
                }
            }
            self.record_discarded(&discarded);
        }
        self.cv.notify_one();
    }

    /// Mark voice-submit Enter for MarkActive auto-Enter de-dup.
    pub fn note_voice_submit(&self) {
        *self.last_voice_submit.lock().unwrap() = Some(Instant::now());
    }

    /// True if voice submit within ~3s (UserPromptSubmit is that submit's echo).
    pub fn take_recent_voice_submit(&self) -> bool {
        let mut g = self.last_voice_submit.lock().unwrap();
        let recent = voice_submit_recent(*g, Instant::now());
        if recent {
            *g = None;
        }
        recent
    }

    /// Pause under `cause`, gen bump + requeue intent, fade.
    /// GUARD: `BargeSpeculative` is no-op if already `Dictation` (race:
    /// `pause_for_record` before `set_stt_active` — barge must not relabel/clear later).
    /// `Dictation` always applies.
    fn pause_with_cause(&self, cause: PauseCause) {
        {
            let mut st = self.paused.lock().unwrap();
            if cause == PauseCause::BargeSpeculative && st.cause == Some(PauseCause::Dictation) {
                return;
            }
            st.paused = true;
            st.cause = Some(cause);
        }
        self.set_tts_active(false);
        let gen0 = self.generation.fetch_add(1, Ordering::SeqCst);
        // Requeue intent pinned to this gen (not live paused).
        self.record_cancel_kind(gen0, true);
        self.tts.stop_fade();
    }

    /// Caps/PTT record barge — always applies; queue kept for resume.
    pub fn pause_for_record(&self) {
        self.pause_with_cause(PauseCause::Dictation);
    }

    /// Foreign-mic speculative barge — no-op under Dictation (see pause_with_cause).
    pub fn pause_for_suspected_barge(&self) {
        self.pause_with_cause(PauseCause::BargeSpeculative);
    }

    /// Full-duplex AEC active (mic-barge stands down — VPIO mic always live).
    pub fn is_full_duplex(&self) -> bool {
        self.tts.is_full_duplex_active()
    }

    /// One-shot diarize on warm helper. Blocks; exclusive with speak/listen.
    pub(crate) fn diarize(&self, seconds: u64) -> std::io::Result<String> {
        self.tts.diarize(seconds)
    }

    /// One-shot enroll on warm helper. Blocks.
    pub(crate) fn enroll(&self, seconds: u64) -> std::io::Result<Vec<f32>> {
        self.tts.enroll(seconds)
    }

    /// Mark active terminal (`MarkActive`). Holds items around update (no lost wakeup).
    pub fn set_active_session(&self, session: Option<String>) {
        let _q = self.items.lock().unwrap();
        self.active.lock().unwrap().explicit = session;
        self.cv.notify_one();
    }

    /// Poll → worker: terminal frontmost. Latches `terminal_seen` (unrecognized never mute).
    pub fn set_terminal_front(&self, front: bool) {
        if front {
            self.terminal_seen.store(true, Ordering::SeqCst);
        }
        self.terminal_front.store(front, Ordering::SeqCst);
    }

    /// Poll → worker: focus-gate config.
    pub fn set_pause_bg(&self, pause: bool) {
        self.pause_bg.store(pause, Ordering::SeqCst);
    }

    pub(crate) fn set_config(&self, config: VoiceConfig) {
        *self.config.lock().unwrap() = config;
    }

    /// Caps/PTT resume — lifts pause of any cause. No-op if not paused.
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

    /// Clear pause only if `BargeSpeculative` (never touches Dictation).
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

    /// Lock-free `activity.speaking` (must not take `items`).
    pub fn is_tts_active(&self) -> bool {
        self.tts_active.load(Ordering::SeqCst)
    }

    /// Status snapshot that never waits on `items`, which spans helper I/O.
    pub fn tts_status_sample(&self) -> TtsStatusSample {
        let active = self.is_tts_active();
        let pending = self.queue_depth.load(Ordering::SeqCst);
        let claim = active
            .then(|| self.playing.lock().unwrap().clone())
            .flatten();
        let source = claim.as_ref().and_then(|claim| claim.source);
        // Playback edges already publish changes to this in-flight addend.
        let speaking_utterance = u64::from(claim.as_ref().is_some_and(|claim| claim.speech));
        TtsStatusSample {
            speaking: active,
            speaker: source,
            queued: pending + speaking_utterance,
            utterance: claim.and_then(|claim| claim.utterance),
            recent_utterances: self.utterances.lock().unwrap().iter().cloned().collect(),
        }
    }

    /// Publish the new pending speech depth, then bump WaitModelStatus only when it changed.
    /// Store-before-bump so a woken waiter reads the depth that caused the wake.
    /// Call under the `items` guard from every site that mutates the queue.
    fn publish_queue_depth(&self, before: usize, after: usize) {
        let prev = self.queue_depth.swap(after as u64, Ordering::SeqCst);
        debug_assert_eq!(
            prev, before as u64,
            "pending speech depth drifted: an `items` mutation skipped publish_queue_depth"
        );
        if before != after {
            self.gate.bump();
        }
    }

    /// Seed/mutate `items` in tests through the production publish path, so hand-built
    /// queue state can't leave the depth mirror stale and mask a real drift.
    #[cfg(test)]
    fn edit_items_locked_for_test<R>(
        &self,
        items: &mut VecDeque<Item>,
        f: impl FnOnce(&mut VecDeque<Item>) -> R,
    ) -> R {
        let before = speech_depth(items);
        let out = f(items);
        self.publish_queue_depth(before, speech_depth(items));
        out
    }

    #[cfg(test)]
    fn edit_items_for_test<R>(&self, f: impl FnOnce(&mut VecDeque<Item>) -> R) -> R {
        let mut items = self.items.lock().unwrap();
        self.edit_items_locked_for_test(&mut items, f)
    }

    /// Sole `tts_active` writer; bumps gate only on real transitions.
    fn set_tts_active(&self, on: bool) {
        if self.tts_active.swap(on, Ordering::SeqCst) != on {
            self.gate.bump();
        }
    }

    /// Publish resolution before the speaking transition so one wake exposes both.
    fn publish_resolved_utterance(
        &self,
        voice: &str,
        language: &str,
        warning: Option<ds_status::UtteranceWarning>,
    ) {
        if let Some(claim) = self.playing.lock().unwrap().as_mut()
            && let Some(utterance) = claim.utterance.as_mut()
        {
            utterance.voice = Some(voice.to_string());
            utterance.language = Some(language.to_string());
            utterance.warning = warning;
        }
    }

    /// Utterances a barge or session clear threw away before they ever played. Without a
    /// record, "no entry for my handle" would mean both "still queued" and "silently gone".
    /// They never reached the play gate, so they carry no voice or language.
    fn record_discarded(&self, discarded: &[u64]) {
        if discarded.is_empty() {
            return;
        }
        let mut utterances = self.utterances.lock().unwrap();
        for id in discarded {
            utterances.push_front(ds_status::UtteranceStatus {
                id: *id,
                voice: None,
                language: None,
                warning: None,
                outcome: Some(ds_status::UtteranceOutcome::Cancelled),
            });
        }
        utterances.truncate(RECENT_UTTERANCES_MAX);
        drop(utterances);
        self.gate.bump();
    }

    /// Close out the claimed utterance: stamp its fate onto the claim's own record and
    /// publish it. Retries and requeues re-enter the play gate, so this runs once per
    /// utterance — when the queue is done with the item, not when playback returns. A cue
    /// has no record and is skipped.
    fn finish_utterance(&self, outcome: ds_status::UtteranceOutcome) {
        let Some(mut record) = self
            .playing
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|claim| claim.utterance.clone())
        else {
            return;
        };
        record.outcome = Some(outcome);
        let mut utterances = self.utterances.lock().unwrap();
        utterances.push_front(record);
        utterances.truncate(RECENT_UTTERANCES_MAX);
        drop(utterances);
        self.gate.bump();
    }

    /// Pause flag (cause-agnostic).
    pub(crate) fn is_paused(&self) -> bool {
        self.paused.lock().unwrap().paused
    }

    /// Store requeue intent for cancel's PRE-bump gen. Cap map size if unclaimed.
    fn record_cancel_kind(&self, gen0: u64, requeue: bool) {
        let mut m = self.cancel_kind.lock().unwrap();
        m.insert(gen0, requeue);
        if m.len() > 32
            && let Some(oldest) = m.keys().copied().min()
        {
            m.remove(&oldest);
        }
    }

    /// Close always-listening mic? Focus hold reads idle (dictation off-terminal).
    /// Holds `items` with in_flight sample (avoids false-idle race).
    pub fn is_busy(&self) -> bool {
        if self.worker_focus_hold() && !self.tts_active.load(Ordering::SeqCst) {
            return false;
        }
        let items = self.items.lock().unwrap();
        self.tts_active.load(Ordering::SeqCst)
            || self.in_flight.load(Ordering::SeqCst)
            || !items.is_empty()
    }

    /// STT resident+warm (dictation start-guard).
    pub fn stt_loaded(&self) -> bool {
        self.tts.is_stt_loaded()
    }

    /// Non-blocking crash heal on a throwaway thread (must not block poll/readiness —
    /// start holds lifecycle for seconds). Caps-only users never queue speak that would
    /// heal; readiness wait must keep ticking (#59). Single-flight.
    pub fn heal_crashed_child(&self) {
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

    /// SessionEnd: per-window barge. Agent voice survives (keyed by client).
    pub fn end_session(&self, session: Option<String>) {
        self.clear_session(session);
    }

    /// Get-or-assign agent voice ([`pick_agent_voice`]). Pool non-empty.
    fn assign_agent_voice(
        &self,
        agent: Option<WiredAgent>,
        language: &str,
        pool: &[String],
    ) -> String {
        let key = (agent, language.to_string());
        let mut map = self.agent_voices.lock().unwrap();
        let voice = pick_agent_voice(&map, pool, &key, &mut |n| fastrand::usize(..n));
        map.insert(key, voice.clone());
        voice
    }

    /// Enumerated System voices, read once per process. Reached only when a language actually
    /// has to be matched, so `say -v ?` stays off the greeting path and out of tests.
    fn system_voice_catalog(&self) -> &[ds_tts::SpeakerVoice] {
        self.system_voices
            .get_or_init(ds_tts::enumerate::system_voices)
    }

    /// Shared greeting+worker resolver: `(engine, voice)`. None if TTS off / empty pool.
    /// `language` is the detected language of the utterance; `None` (greeting) keeps the
    /// agent's default assignment.
    fn resolve_engine_voice(
        &self,
        cfg: &VoiceConfig,
        source: Option<WiredAgent>,
        language: Option<&str>,
    ) -> Option<(ds_config::TtsEngine, String)> {
        let engine = cfg.resolved_tts()?;
        let voice = match engine {
            ds_config::TtsEngine::System if cfg.tts_voices.system.is_empty() => String::new(),
            ds_config::TtsEngine::System => self.pick_for_language(
                || ds_tts::enumerate::VoiceCatalog::System(self.system_voice_catalog()),
                source,
                language,
                &cfg.tts_voices.system,
            ),
            ds_config::TtsEngine::BuiltIn => {
                let pool = cfg.active_voices();
                if pool.is_empty() {
                    return None;
                }
                self.pick_for_language(
                    || ds_tts::enumerate::VoiceCatalog::BuiltIn(cfg.tts_model),
                    source,
                    language,
                    pool,
                )
            }
        };
        Some((engine, voice))
    }

    /// Assign this agent a voice that can actually speak `language`.
    ///
    /// One path for every engine. The configured pool is narrowed to the voices that own the
    /// language; a language the user configured nothing for substitutes the model's whole
    /// catalog for it, so the choice is still made among that language's own voices. Either
    /// way the result goes through [`pick_agent_voice`], which keeps the roll random, the
    /// assignment sticky per agent, and agents on distinct voices while spares remain.
    ///
    /// Catalogs whose voices are not locked to a language (Chatterbox, Qwen, OmniVoice, and
    /// System names that cannot be resolved) return the pool unnarrowed, so this is a no-op
    /// for them rather than a per-engine branch. `catalog` is built only once a language is
    /// known, keeping System voice enumeration off the greeting path.
    fn pick_for_language<'a>(
        &self,
        catalog: impl FnOnce() -> ds_tts::enumerate::VoiceCatalog<'a>,
        source: Option<WiredAgent>,
        language: Option<&str>,
        pool: &[String],
    ) -> String {
        let Some(language) = language else {
            return self.assign_agent_voice(source, "", pool);
        };
        let catalog = catalog();
        let mut candidates = catalog.pool_for_language(pool, language);
        if candidates.is_empty() {
            candidates = catalog.voices_for_language(language);
        }
        if candidates.is_empty() {
            // No voice anywhere owns the language (fresh install, or a language the catalog
            // does not cover). Keep the agent's usual voice rather than drop the utterance:
            // synthesis still receives the detected language, so pronunciation stays right.
            return self.assign_agent_voice(source, "", pool);
        }
        self.assign_agent_voice(source, language, &candidates)
    }

    /// Greet on open (if enabled). Claims agent voice now so same agent matches on open.
    pub fn greet_session(&self, source: Option<WiredAgent>, session: Option<String>) {
        let cfg = self.config.lock().unwrap().clone();
        if !cfg.greet {
            return;
        }
        let Some((engine, voice)) = self.resolve_engine_voice(&cfg, source, None) else {
            return;
        };
        let name = ds_tts::enumerate::voice_display_name(engine, cfg.tts_model, &voice);
        let idx = GREET_ROTATION.fetch_add(1, Ordering::Relaxed);
        let text = greeting_line(name.as_deref(), idx);
        let args = TtsArgPools::with_voice(engine, cfg.tts_model, voice);
        if let Err(e) = self.enqueue(text, Some(args), source, session) {
            log::warn!(target: "ttsq", "greeting rejected: {e}");
        }
    }

    /// Mic + focus hold snapshot (one sample for hold/busy agreement).
    fn worker_hold_state(&self) -> HoldState {
        hold_state(
            self.tts.is_full_duplex_active(),
            self.mic.is_active(),
            self.pause_bg.load(Ordering::SeqCst),
            self.terminal_seen.load(Ordering::SeqCst),
            self.terminal_front.load(Ordering::SeqCst),
        )
    }

    fn worker_focus_hold(&self) -> bool {
        focus_holds(
            self.pause_bg.load(Ordering::SeqCst),
            self.terminal_seen.load(Ordering::SeqCst),
            self.terminal_front.load(Ordering::SeqCst),
        )
    }

    fn run(self: Arc<Self>) {
        'outer: loop {
            // Wait for playable item ([`claimable_pos`]); lock order items → active.
            let (mut item, gen0) = {
                let mut q = self.items.lock().unwrap();
                loop {
                    // Snapshot the cancel generation BEFORE reading `paused`:
                    // `pause_with_cause` sets `paused` then bumps, so a snapshot taken
                    // after the pause check could absorb that bump and play the item
                    // while paused.
                    let gen0 = self.generation.load(Ordering::SeqCst);
                    let active = self.active.lock().unwrap().effective();
                    if let Some(pos) = claimable_pos(self.is_paused(), &q, &active) {
                        break (self.claim_item(&mut q, pos), gen0);
                    }
                    q = self.cv.wait(q).unwrap();
                }
            };
            let _in_flight = InFlightGuard(&self.in_flight);
            let _playing = PlayingGuard(&self.playing);

            let mut played = None;
            match &item.action {
                QueueAction::Speech {
                    text,
                    language,
                    tts_args,
                } => {
                    let (outcome, resume_skip) =
                        self.play_speech(&item, gen0, text, language, tts_args.as_deref());
                    item.resume_skip = resume_skip;
                    match outcome {
                        SpeechOutcome::Requeue => {
                            self.settle_cancelled(item, gen0);
                            continue 'outer;
                        }
                        // A cancel that landed during playback still cut it short, so the
                        // shared trailer re-checks the generation before recording this.
                        SpeechOutcome::Done(outcome) => played = Some(outcome),
                    }
                }
                QueueAction::Earcon(event) => {
                    if !self.gate_earcon(*event, gen0) {
                        self.requeue_if_resuming(item, gen0);
                        continue 'outer;
                    }
                    self.set_tts_active(true);
                    if let Err(e) = self.cue_one(*event) {
                        log::warn!(target: "ttsq", "queued earcon failed: {e}");
                    }
                }
            }
            if self.generation.load(Ordering::SeqCst) != gen0 {
                self.settle_cancelled(item, gen0);
            } else if let Some(outcome) = played {
                self.finish_utterance(outcome);
            }
            self.set_tts_active(false);
        }
    }

    /// Play speech; one retry on warm-child transport loss (reload mid-write).
    fn play_speech(
        &self,
        item: &Item,
        gen0: u64,
        text: &str,
        detected_language: &str,
        tts_args: Option<&TtsArgPools>,
    ) -> (SpeechOutcome, usize) {
        let mut retries_left = 1u8;
        let mut resume_skip = item.resume_skip;
        loop {
            let (engine, model, voice, language, rate, params) =
                match self.gate_item(item, gen0, detected_language, tts_args) {
                    GateOutcome::Play {
                        engine,
                        model,
                        voice,
                        language,
                        rate,
                        params,
                    } => (engine, model, voice, language, rate, params),
                    GateOutcome::Requeue => return (SpeechOutcome::Requeue, resume_skip),
                    GateOutcome::Drop(reason) => {
                        log::warn!(target: "ttsq", "queued speak could not start: {reason}");
                        return (
                            SpeechOutcome::Done(ds_status::UtteranceOutcome::Dropped),
                            resume_skip,
                        );
                    }
                };

            self.publish_resolved_utterance(
                &voice,
                &language,
                utterance_warning(engine, model, &voice, &language),
            );
            self.set_tts_active(true);
            let result =
                self.speak_one(engine, text, &voice, &language, rate, &params, resume_skip);
            let model_supports_resume = model.descriptor().supports_resume;
            if matches!(engine, Some(ds_config::TtsEngine::BuiltIn)) && model_supports_resume {
                resume_skip = resume_skip.max(self.tts.last_speak_progress());
            }
            match result {
                Ok(()) => {
                    return (
                        SpeechOutcome::Done(ds_status::UtteranceOutcome::Spoken),
                        resume_skip,
                    );
                }
                Err(e)
                    if retries_left > 0
                        && should_retry_speak(engine, model_supports_resume, &e) =>
                {
                    retries_left -= 1;
                    // Clear active before re-gate so cancel can't strand tts_active.
                    self.set_tts_active(false);
                    log::warn!(
                        target: "ttsq",
                        "queued speak lost its child during dispatch; retrying once: {e}"
                    );
                }
                Err(e) => {
                    log::warn!(target: "ttsq", "queued speak failed: {e}");
                    return (
                        SpeechOutcome::Done(ds_status::UtteranceOutcome::Failed),
                        resume_skip,
                    );
                }
            }
        }
    }

    /// Hold → readiness → re-hold if focus/mic changed during wait (up to 60s).
    /// Focus only stores atomics (no gen bump), so the re-hold loop is required.
    fn gate_item(
        &self,
        item: &Item,
        gen0: u64,
        detected_language: &str,
        tts_args: Option<&TtsArgPools>,
    ) -> GateOutcome {
        loop {
            // Hold (don't drop): half-duplex mic live; focus gate (both duplex modes;
            // self-arm via terminal_seen; off when pause_bg false).
            // Gen bump (pause/Stop) breaks the
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

            let mut cfg = self.config.lock().unwrap().clone();
            let selected_engine = cfg.resolved_tts();
            let target_args = selected_engine.and_then(|engine| {
                tts_args.and_then(|args| args.for_target(engine, cfg.tts_model))
            });
            let requested_language = target_args
                .and_then(ds_config::TtsTargetArgs::language)
                .unwrap_or(detected_language);
            // System accepts the detector/explicit code directly. Built-in models clamp an
            // admit-time code after a model switch to the live model's supported default.
            let language = match selected_engine {
                Some(ds_config::TtsEngine::System) => requested_language.to_string(),
                _ => ds_tts::supported_language(requested_language, cfg.tts_model),
            };
            // Engine + base voice come from config via the SAME shared helper the greeting
            // uses — System and built-in both claim this agent's configured pool
            // voice. Off / no usable rung / empty pool ⇒ a blank voice (speak_one no-ops,
            // value unused). The selected target's `voice` then overrides that base.
            let (engine, base_voice) =
                match self.resolve_engine_voice(&cfg, item.source, Some(&language)) {
                    Some((e, v)) => (Some(e), v),
                    None => (None, String::new()),
                };
            // Only the live target's block participates, so an override for another model cannot
            // leak across a config switch. System voices remain freeform; built-in ids are clamped.
            let voice_override = target_args.and_then(ds_config::TtsTargetArgs::voice);
            let voice = match voice_override {
                Some(v)
                    if engine == Some(ds_config::TtsEngine::BuiltIn)
                        && !ds_tts::enumerate::is_model_voice(cfg.tts_model, v) =>
                {
                    log::debug!(
                        target: "ttsq",
                        "dropping stale voice '{v}' not in the {} catalog; using '{base_voice}'",
                        cfg.tts_model.as_str()
                    );
                    base_voice
                }
                Some(v) => v.to_string(),
                None => base_voice,
            };
            apply_tts_arg_params(&mut cfg, selected_engine, target_args);
            let rate = playback_rate(&cfg, engine);
            let params = cfg
                .tts_model
                .descriptor()
                .resolve_params(cfg.tts_params.for_model(cfg.tts_model));

            // Never send built-in TTS work before its model is ready. Accepted work is HELD
            // during an ordinary warm-up and remains busy; it is dropped only for an explicit
            // cancel, disabled engine, terminal load failure, or readiness timeout. The wait
            // never blocks on child lifecycle calls (healing is async, issue #59), so its
            // deadline is a real upper bound.
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
                model: cfg.tts_model,
                voice,
                language,
                rate,
                params,
            };
        }
    }

    fn gate_earcon(&self, event: ds_earcon::EarconEvent, gen0: u64) -> bool {
        while self.generation.load(Ordering::SeqCst) == gen0 {
            let hold = self.worker_hold_state();
            // A queued needs_input passes a focus-only hold instead of parking until
            // refocus (e.g. it arrived while the reply was still draining after the user
            // tabbed away, so the idle gate routed it here) — held, it would defeat the
            // alert exactly like the pre-dispatch_earcon behavior. `tts_idle` is
            // structurally true at this point: the single worker cleared `tts_active`
            // before claiming this item, and only an out-of-band cue could be sounding,
            // which the manager's cue lease serializes with the play below. Mic/duplex
            // holds still park it; reply_done still parks.
            if !hold.any() || earcon_bypasses_queue(event, hold, true) {
                self.in_flight.store(true, Ordering::SeqCst);
                return true;
            }
            self.in_flight.store(hold.reports_busy(), Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(120));
        }
        false
    }

    fn cue_one(&self, event: ds_earcon::EarconEvent) -> Result<(), String> {
        if self.tts.is_muted() {
            return Ok(());
        }
        let cfg = self.config.lock().unwrap().clone();
        let Some(path) = ds_earcon::resolve_cue(&cfg.earcon_reply, &cfg.earcon_input, event) else {
            return Ok(());
        };
        self.tts.cue(&path).map_err(|e| e.to_string())
    }

    /// Resume uses deterministic helper batch indices; stale `skip` values clamp safely.
    /// System TTS ignores `skip`. Mute consumes speech in either path.
    #[allow(clippy::too_many_arguments)] // one wire request's fields
    fn speak_one(
        &self,
        engine: Option<ds_config::TtsEngine>,
        text: &str,
        voice: &str,
        language: &str,
        rate: f32,
        params: &ds_config::ResolvedTtsParams,
        skip: usize,
    ) -> std::io::Result<()> {
        match engine {
            None => Err(std::io::Error::other("TTS is disabled")),
            Some(ds_config::TtsEngine::System) => {
                self.tts.speak_system(text, voice, language, rate)
            }
            Some(ds_config::TtsEngine::BuiltIn) => {
                self.tts.ensure_started();
                self.tts.speak(text, voice, language, rate, params, skip)
            }
        }
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
            Some(TtsEngine::BuiltIn) => {}
        }

        // Complete mark_dead's "next speak heals" contract WITHOUT blocking: the heal
        // routes through `heal_crashed_child`'s single-flight background thread, never a
        // synchronous `restart_if_crashed` — a start can hold the manager's `lifecycle`
        // lock across a whole spawn+READY handshake (bounded since issue #59, but still
        // up to `READY_HANDSHAKE_TIMEOUT`), and riding it here kept the claimed item
        // `in_flight` (mic closed, `stop` unheard) for the duration. This worker
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
                self.tts.load_engine(ds_helper_proto::HelperModel::Tts); // fire-and-forget stdin write — non-blocking
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
                    self.tts.load_engine(ds_helper_proto::HelperModel::Tts);
                    load_requested = true;
                }
            }
            if Instant::now() >= deadline {
                let model = self.tts.selected_tts_model().descriptor().display_name;
                return ReadyOutcome::Unavailable(format!(
                    "timed out waiting for the {model} model to become ready"
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// On a cancel: if the SPECIFIC cancellation that interrupted THIS item (recorded
    /// against its own `gen0` in `cancel_kind`, at the moment that bump fired) was a
    /// record-barge pause, re-enqueue the interrupted item (narration OR reply) at the
    /// front so the worker resumes the whole queue from there. The resume is
    /// batch-granular: [`run`](Self::run) merged the helper's played-batch `PROGRESS`
    /// mark into the item's [`Item::resume_skip`] before requeueing, so the next run
    /// skips the already-heard prefix; from-the-top is the no-mark fallback (mark 0 —
    /// older helper, full-duplex, or nothing played). A hard cancel re-enqueues
    /// nothing — it dropped the item on purpose.
    ///
    /// Deliberately does NOT just re-read the CURRENT `paused` flag: by the time playback
    /// actually unwinds, an unrelated LATER event may have moved `paused` on — e.g. an
    /// explicit `cancel_for_submit` current-scope drop (intending no requeue) immediately followed
    /// by an unrelated record-barge `pause_for_record` (which sets `paused = true`) would
    /// make a live read see `paused == true` and wrongly resurrect an item the clear had
    /// already cancelled. Falls back to the live flag only if this bump's intent was never
    /// recorded (defensive — every generation-bumping site above records one).
    fn requeue_if_resuming(&self, item: Item, gen0: u64) -> bool {
        let requeue = self
            .cancel_kind
            .lock()
            .unwrap()
            .remove(&gen0)
            .unwrap_or_else(|| self.is_paused());
        if !should_requeue(requeue, &item.action) {
            return false;
        }
        let mut q = self.items.lock().unwrap();
        let before = speech_depth(&q);
        q.push_front(item);
        self.publish_queue_depth(before, speech_depth(&q));
        true
    }

    /// A cancel reached this item. Requeued it is still going to be spoken; dropped it is
    /// over, and its terminal record says the cancel is why.
    fn settle_cancelled(&self, item: Item, gen0: u64) {
        if !self.requeue_if_resuming(item, gen0) {
            self.finish_utterance(ds_status::UtteranceOutcome::Cancelled);
        }
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
        model: ds_config::TtsModel,
        voice: String,
        language: String,
        rate: f32,
        /// Helper re-resolution handles model-switch races by falling back to defaults.
        params: ds_config::ResolvedTtsParams,
    },
    Requeue,
    Drop(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeechOutcome {
    /// The play gate is finished with this item; the payload is its terminal record's fate.
    Done(ds_status::UtteranceOutcome),
    Requeue,
}

fn utterance_warning(
    engine: Option<ds_config::TtsEngine>,
    model: ds_config::TtsModel,
    voice: &str,
    language: &str,
) -> Option<ds_status::UtteranceWarning> {
    if engine != Some(ds_config::TtsEngine::BuiltIn) || model != ds_config::TtsModel::Kokoro {
        return None;
    }
    let voice_language = ds_tts::enumerate::kokoro_language(voice);
    (voice_language != "other" && voice_language != language)
        .then_some(ds_status::UtteranceWarning::VoiceLanguageMismatch)
}

fn should_retry_speak(
    engine: Option<ds_config::TtsEngine>,
    model_supports_resume: bool,
    error: &std::io::Error,
) -> bool {
    model_supports_resume
        && matches!(engine, Some(ds_config::TtsEngine::BuiltIn))
        && matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
        )
}

/// Whether an interrupted item (narration OR reply) should be RE-ENQUEUED to resume later.
/// Only when we were PAUSED for a record-barge (resume mode) — a hard clear/Stop
/// leaves `paused == false` and re-enqueues nothing (it dropped on purpose). Empty text is
/// never requeued. Pure, so the "resume keeps the item, clear drops it" rule is unit-tested.
fn should_requeue(resuming: bool, action: &QueueAction) -> bool {
    resuming && action.requeueable()
}

/// The worker's two independent "hold, don't drop" gates.
///
/// - MIC LIVE (half-duplex only): never speak into a recording. Full-duplex skips this
///   — the VPIO mic is always live, so the AEC handles the overlap instead (coexist;
///   the voice stops only on an explicit `stop`/`stopfade` op, never a talk-over barge).
/// - FOCUS (both modes, only when `pause_bg`): no terminal frontmost (you
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

fn focus_holds(pause_bg: bool, terminal_seen: bool, terminal_front: bool) -> bool {
    pause_bg && terminal_seen && !terminal_front
}

fn hold_state(
    full_duplex: bool,
    mic_active: bool,
    pause_bg: bool,
    terminal_seen: bool,
    terminal_front: bool,
) -> HoldState {
    HoldState {
        mic: mic_holds(full_duplex, mic_active),
        focus: focus_holds(pause_bg, terminal_seen, terminal_front),
    }
}

/// The COMPLETE out-of-band routing decision for one earcon: only needs_input, only while
/// the focus hold is actively silencing the queue, never while a half-duplex mic is live,
/// and only while playback is idle — see [`TtsQueue::dispatch_earcon`] for the full
/// rationale.
fn earcon_bypasses_queue(event: ds_earcon::EarconEvent, hold: HoldState, tts_idle: bool) -> bool {
    matches!(event, ds_earcon::EarconEvent::NeedsInput) && hold.focus && !hold.mic && tts_idle
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
        // FOCUS gate: only when pause_bg AND a terminal has been seen AND none
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
            "pause_bg off → play"
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
    fn playback_rate_is_scoped_to_system_and_kokoro() {
        let mut cfg = VoiceConfig::default();
        cfg.tts_params
            .system
            .insert("rate".into(), ds_config::TtsParamValue::Float(1.25));
        cfg.tts_params
            .kokoro
            .insert("rate".into(), ds_config::TtsParamValue::Float(0.8));

        assert_eq!(
            playback_rate(&cfg, Some(ds_config::TtsEngine::System)),
            1.25
        );
        assert_eq!(
            playback_rate(&cfg, Some(ds_config::TtsEngine::BuiltIn)),
            0.8
        );

        for model in [
            ds_config::TtsModel::Chatterbox,
            ds_config::TtsModel::Qwen,
            ds_config::TtsModel::OmniVoice,
        ] {
            cfg.tts_model = model;
            assert_eq!(
                playback_rate(&cfg, Some(ds_config::TtsEngine::BuiltIn)),
                1.0,
                "{} has no rate parameter",
                model.as_str()
            );
        }
        assert_eq!(playback_rate(&cfg, None), 1.0);
    }

    #[test]
    fn per_target_args_override_only_the_live_target_settings() {
        let pools = TtsArgPools::parse(&serde_json::json!({
            "system": { "rate": 1.5 },
            "kokoro": { "rate": 1.1 },
            "qwen": { "repetition_penalty": 1.8 }
        }))
        .unwrap();
        let mut cfg = VoiceConfig::default();
        let model = cfg.tts_model;

        apply_tts_arg_params(
            &mut cfg,
            Some(ds_config::TtsEngine::System),
            pools.for_target(ds_config::TtsEngine::System, model),
        );
        assert_eq!(playback_rate(&cfg, Some(ds_config::TtsEngine::System)), 1.5);
        assert!(cfg.tts_params.kokoro.is_empty());

        apply_tts_arg_params(
            &mut cfg,
            Some(ds_config::TtsEngine::BuiltIn),
            pools.for_target(ds_config::TtsEngine::BuiltIn, model),
        );
        assert_eq!(
            playback_rate(&cfg, Some(ds_config::TtsEngine::BuiltIn)),
            1.1
        );
        assert!(cfg.tts_params.qwen.is_empty());
    }

    #[test]
    fn should_requeue_only_when_paused_and_nonempty() {
        let speech = QueueAction::Speech {
            text: "the held narration".into(),
            language: "en".into(),
            tts_args: None,
        };
        // Resume mode (paused) keeps a non-empty item → re-enqueued to continue.
        assert!(should_requeue(true, &speech));
        // A hard clear / Stop leaves paused == false → dropped on purpose.
        assert!(!should_requeue(false, &speech));
        // Empty / whitespace-only text is never requeued, even when paused.
        assert!(!should_requeue(
            true,
            &QueueAction::Speech {
                text: "   \n\t ".into(),
                language: "en".into(),
                tts_args: None,
            }
        ));
        assert!(should_requeue(
            true,
            &QueueAction::Earcon(ds_earcon::EarconEvent::ReplyDone)
        ));
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

    #[test]
    fn utterance_warning_only_flags_kokoro_language_mismatches() {
        use ds_config::{TtsEngine, TtsModel};
        use ds_status::UtteranceWarning;

        assert_eq!(
            utterance_warning(Some(TtsEngine::BuiltIn), TtsModel::Kokoro, "af_sarah", "it"),
            Some(UtteranceWarning::VoiceLanguageMismatch)
        );
        assert_eq!(
            utterance_warning(Some(TtsEngine::BuiltIn), TtsModel::Kokoro, "if_sara", "it"),
            None
        );
        assert_eq!(
            utterance_warning(
                Some(TtsEngine::BuiltIn),
                TtsModel::Chatterbox,
                "default",
                "it"
            ),
            None
        );
    }

    fn pool() -> Vec<String> {
        vec!["af_sarah".into(), "am_adam".into(), "bf_emma".into()]
    }

    /// Assignment key for `agent` speaking English.
    fn key(agent: WiredAgent) -> (Option<WiredAgent>, String) {
        (Some(agent), "en".to_string())
    }

    #[test]
    fn fresh_pick_rolls_across_the_candidate_pool() {
        let p = pool();
        let a = HashMap::new();
        for (roll, expect) in [(0, "af_sarah"), (1, "am_adam"), (2, "bf_emma")] {
            assert_eq!(
                pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |_| roll),
                expect
            );
        }
    }

    #[test]
    fn distinct_agents_claim_distinct_voices_while_free() {
        // While free voices remain, every agent gets its own — under ANY roll.
        for mut roll in [
            Box::new(|_: usize| 0usize) as Box<dyn FnMut(usize) -> usize>,
            Box::new(|n: usize| n - 1),
        ] {
            let p = pool();
            let mut a = HashMap::new();
            for agent in [
                WiredAgent::ClaudeCode,
                WiredAgent::Codex,
                WiredAgent::QwenCode,
            ] {
                let v = pick_agent_voice(&a, &p, &key(agent), &mut roll);
                assert!(
                    !a.values().any(|held| held == &v),
                    "{agent:?} must get a voice no other agent holds"
                );
                a.insert(key(agent), v);
            }
        }
    }

    #[test]
    fn agent_reuses_its_assignment_across_sessions_and_requests() {
        // The assignment is keyed by agent, not session/terminal: repeated resolutions
        // (new windows, later replies) return the SAME voice, whatever the roll says.
        let p = pool();
        let mut a = HashMap::new();
        let first = pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |_| 1);
        a.insert(key(WiredAgent::ClaudeCode), first.clone());
        a.insert(key(WiredAgent::Codex), "am_adam".into());
        assert_eq!(
            pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |_| 0),
            first
        );
        assert_eq!(
            pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |n| n - 1),
            first
        );
    }

    #[test]
    fn load_spreading_is_scoped_to_one_language() {
        // Another agent holding a voice for a DIFFERENT language must not make that voice look
        // taken. It matters for language-agnostic models, where every language draws the same
        // pool and cross-language load would starve agents of their assignment.
        let p = pool();
        let mut a = HashMap::new();
        a.insert(
            (Some(WiredAgent::Codex), "it".to_string()),
            "af_sarah".into(),
        );
        assert_eq!(
            pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |_| 0),
            "af_sarah"
        );
        a.insert(
            (Some(WiredAgent::Codex), "en".to_string()),
            "af_sarah".into(),
        );
        assert_eq!(
            pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |_| 0),
            "am_adam"
        );
    }

    #[test]
    fn agents_beyond_pool_reuse_least_loaded_voices() {
        // More agents than voices → double up on the least-loaded voice, never pile on
        // one voice while another sits lighter.
        let p = vec!["af_sarah".to_string(), "am_adam".to_string()];
        let mut a = HashMap::new();
        for agent in [
            WiredAgent::ClaudeCode,
            WiredAgent::Codex,
            WiredAgent::QwenCode,
            WiredAgent::Grok,
        ] {
            let v = pick_agent_voice(&a, &p, &key(agent), &mut |_| 0);
            a.insert(key(agent), v);
        }
        for v in &p {
            assert_eq!(
                a.values().filter(|held| held == &v).count(),
                2,
                "4 agents over 2 voices must spread 2-and-2, not clump"
            );
        }
    }

    #[test]
    fn roll_selects_among_free_candidates_only() {
        // Carries the old stale-repick regression under agent keying: a repick after a
        // stale (removed-from-pool) assignment must avoid voices held by OTHER agents —
        // the roll ranges over the free set only, so index 0 is not pool[0] here.
        let p = vec!["af_nicole".to_string(), "am_adam".to_string()];
        let mut a = HashMap::new();
        a.insert(key(WiredAgent::ClaudeCode), "af_sarah".to_string()); // stale (old pool)
        a.insert(key(WiredAgent::Codex), "af_nicole".to_string()); // valid, holds af_nicole
        let mut seen_n = usize::MAX;
        let v = pick_agent_voice(&a, &p, &key(WiredAgent::ClaudeCode), &mut |n| {
            seen_n = n;
            0
        });
        assert_eq!(v, "am_adam", "must not land on Codex's voice");
        assert_eq!(seen_n, 1, "the roll ranges over the free set only");

        // Seeded-RNG determinism: the same seed yields the same pick (this is exactly the
        // production `roll` shape; no live randomness asserted).
        let empty = HashMap::new();
        let picks: Vec<String> = (0..2)
            .map(|_| {
                let mut rng = fastrand::Rng::with_seed(7);
                pick_agent_voice(&empty, &pool(), &key(WiredAgent::Grok), &mut |n| {
                    rng.usize(..n)
                })
            })
            .collect();
        assert_eq!(picks[0], picks[1]);
    }

    #[test]
    fn stale_assignment_is_dropped_when_voice_leaves_the_pool() {
        // Regression: an agent assigned under the OLD pool keeps speaking the old
        // voice after the user changes the selected model's voice pool (the assignment cache
        // survives a config hot-reload). The stale pick must be discarded and a voice
        // from the CURRENT pool chosen instead — otherwise the agent keeps using a
        // voice the user dropped ("Sarah introduces herself as Nicole").
        let mut a = HashMap::new();
        a.insert(key(WiredAgent::ClaudeCode), "af_sarah".to_string()); // old default
        let new_pool = vec!["af_nicole".to_string()]; // user switched to Nicole-only
        let v = pick_agent_voice(&a, &new_pool, &key(WiredAgent::ClaudeCode), &mut |_| 0);
        assert_eq!(
            v, "af_nicole",
            "a voice no longer in the pool must not be reused"
        );
        // And once re-recorded, the fresh pick is stable.
        a.insert(key(WiredAgent::ClaudeCode), v);
        assert_eq!(
            pick_agent_voice(&a, &new_pool, &key(WiredAgent::ClaudeCode), &mut |_| 0),
            "af_nicole"
        );
    }

    /// Build a narration `Item` tagged with `session` (the only field `select_pos`
    /// inspects), for the selection truth-table tests.
    fn narr(session: Option<&str>) -> Item {
        Item {
            action: QueueAction::Speech {
                text: "x".into(),
                language: "en".into(),
                tts_args: None,
            },
            source: None,
            session: session.map(str::to_string),
            resume_skip: 0,
            utterance_id: None,
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
        // session == None is reserved for global/sessionless hook audio. It plays even
        // when another terminal is active; MCP speak/stop never use this path.
        let q = deque(&[Some("a"), None, Some("a")]);
        assert_eq!(select_pos(&q, &Some("b".into())), Some(1));
    }

    #[test]
    fn grok_stop_sticky_tag_is_preferred_with_active_session() {
        // Grok Stop digests admit under `grok-stop:<real>` so MarkActive current-clear
        // cannot prune them. They must still win active-session priority over another
        // terminal's FIFO items (not fall through to "no preferred → play front").
        let q = deque(&[Some("other"), Some("grok-stop:active"), Some("active")]);
        assert_eq!(
            select_pos(&q, &Some("active".into())),
            Some(1),
            "sticky digests for the active terminal must not wait behind other sessions"
        );
        // Real session id still preferred when it appears first among preferred tags.
        let q2 = deque(&[Some("active"), Some("grok-stop:active")]);
        assert_eq!(select_pos(&q2, &Some("active".into())), Some(0));
        // Sticky for a different session is not preferred.
        let q3 = deque(&[Some("grok-stop:other"), Some("active")]);
        assert_eq!(select_pos(&q3, &Some("active".into())), Some(1));
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
        // `clear_on_input = [other]`: keeping "a" drops b's item AND the untagged/global
        // one — unlike `prune_session`, untagged audio does NOT survive here.
        let mut q = deque(&[Some("a"), Some("b"), None, Some("a")]);
        retain_only_session(&mut q, &Some("a".into()));
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(kept, vec![Some("a".into()), Some("a".into())]);
    }

    #[test]
    fn retain_only_session_keeps_grok_stop_sticky_of_target() {
        // Sticky digests for the submitting terminal must survive clear_on_input=[other].
        let mut q = deque(&[
            Some("a"),
            Some("grok-stop:a"),
            Some("b"),
            None,
            Some("grok-stop:b"),
        ]);
        retain_only_session(&mut q, &Some("a".into()));
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(
            kept,
            vec![Some("a".into()), Some("grok-stop:a".into())],
            "sticky sibling of keep must be retained; other sticky must drop"
        );
    }

    #[test]
    fn prune_session_leaves_sticky_for_mark_active_current() {
        // MarkActive current-clear uses exact prune so Grok sticky digests survive.
        let mut q = deque(&[Some("a"), Some("grok-stop:a"), Some("b")]);
        prune_session(&mut q, &Some("a".into()));
        let kept: Vec<_> = q.iter().map(|it| it.session.clone()).collect();
        assert_eq!(
            kept,
            vec![Some("grok-stop:a".into()), Some("b".into())],
            "exact prune must not drop sticky sibling"
        );
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
            action: QueueAction::Speech {
                text: text.to_string(),
                language: "en".into(),
                tts_args: None,
            },
            source: None,
            session: None,
            resume_skip: 0,
            utterance_id: None,
        }
    }

    #[test]
    fn requeue_if_resuming_puts_the_item_back_at_front_when_marked_for_resume() {
        // A record-barge pause records `true` against the generation the interrupted item was
        // running under; `requeue_if_resuming` must look THAT up (not the live `paused` flag)
        // and re-enqueue the item at the front, ahead of whatever was already queued.
        let q = mk_queue();
        q.edit_items_for_test(|q| q.push_back(item("already queued")));
        q.record_cancel_kind(7, true);
        q.requeue_if_resuming(item("interrupted"), 7);
        let items = q.items.lock().unwrap();
        assert_eq!(
            items.len(),
            2,
            "the interrupted item is re-enqueued, not dropped"
        );
        assert_eq!(
            items.front().and_then(|it| it.action.speech_text()),
            Some("interrupted"),
            "it lands at the FRONT, ahead of the rest of the queue"
        );
    }

    #[test]
    fn requeue_if_resuming_drops_the_item_when_marked_a_hard_cancel() {
        // A hard cancel (clear/skip/clear_session/cancel_for_submit) records `false` — the
        // interrupted item must be dropped, not resurrected — resume mark and all (the
        // played prefix of a deliberately dropped block is not coming back).
        let q = mk_queue();
        q.record_cancel_kind(9, false);
        let mut dropped = item("dropped");
        dropped.resume_skip = 4;
        q.requeue_if_resuming(dropped, 9);
        assert!(
            q.items.lock().unwrap().is_empty(),
            "a hard-cancel-marked generation must not requeue its item"
        );
    }

    /// The batch-granular resume contract at the queue layer: a record-barge requeue
    /// keeps the item's `resume_skip` (merged from the helper's PROGRESS mark by the
    /// worker), so the next run starts past the already-heard prefix.
    #[test]
    fn requeue_if_resuming_preserves_the_items_resume_skip() {
        let q = mk_queue();
        q.record_cancel_kind(7, true);
        let mut interrupted = item("interrupted");
        interrupted.resume_skip = 3;
        q.requeue_if_resuming(interrupted, 7);
        let items = q.items.lock().unwrap();
        assert_eq!(
            items.front().map(|it| it.resume_skip),
            Some(3),
            "the requeued item must carry its resume point"
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
            q.tts_status_sample().queued,
            0,
            "empty/whitespace-only text is dropped, not queued"
        );
        q.enqueue("hello there".into(), None, None, None).unwrap();
        assert_eq!(
            q.tts_status_sample().queued,
            1,
            "a real text block is queued"
        );
    }

    #[test]
    fn speech_and_cues_preserve_fifo_action_order() {
        let q = mk_queue();
        let session = Some("turn-1".to_string());
        q.enqueue("first".into(), None, None, session.clone())
            .unwrap();
        q.enqueue_earcon(ds_earcon::EarconEvent::ReplyDone, None, session.clone())
            .unwrap();
        q.enqueue("later".into(), None, None, session).unwrap();

        let items = q.items.lock().unwrap();
        assert!(matches!(
            items[0].action,
            QueueAction::Speech { ref text, .. } if text == "first"
        ));
        assert!(matches!(
            items[1].action,
            QueueAction::Earcon(ds_earcon::EarconEvent::ReplyDone)
        ));
        assert!(matches!(
            items[2].action,
            QueueAction::Speech { ref text, .. } if text == "later"
        ));
    }

    #[test]
    fn earcon_bypasses_queue_only_for_idle_needs_input_under_a_focus_only_hold() {
        // Truth table for the PURE predicate — the complete routing decision.
        let focus_only = HoldState {
            mic: false,
            focus: true,
        };
        let focus_and_mic = HoldState {
            mic: true,
            focus: true,
        };
        let no_hold = HoldState {
            mic: false,
            focus: false,
        };
        assert!(earcon_bypasses_queue(
            ds_earcon::EarconEvent::NeedsInput,
            focus_only,
            true
        ));
        assert!(
            !earcon_bypasses_queue(ds_earcon::EarconEvent::NeedsInput, focus_and_mic, true),
            "never play into a half-duplex recording"
        );
        assert!(
            !earcon_bypasses_queue(ds_earcon::EarconEvent::NeedsInput, no_hold, true),
            "no hold silencing the queue → ordered like everything else"
        );
        assert!(
            !earcon_bypasses_queue(ds_earcon::EarconEvent::NeedsInput, focus_only, false),
            "playback sounding → never mix the cue over it"
        );
        assert!(
            !earcon_bypasses_queue(ds_earcon::EarconEvent::ReplyDone, focus_only, true),
            "only needs_input escapes"
        );
    }

    #[test]
    fn dispatch_earcon_bypasses_the_held_queue_only_for_idle_needs_input() {
        // Force full-duplex: `mic_holds(full_duplex=true, …)` is always false, so a dev
        // machine's genuinely live microphone can't turn the focus-only hold under test
        // into focus+mic and silently defeat the bypass.
        let q = mk_queue();
        q.tts.set_full_duplex_active_for_test(true);
        let s = Some("term-1".to_string());

        // Engage the focus hold: config on, terminal seen frontmost once, then backgrounded.
        q.set_pause_bg(true);
        q.set_terminal_front(true);
        q.set_terminal_front(false);

        // needs_input under the hold with playback idle → bypass. The branch never touches
        // `items`; the detached thread re-checks the (still-holding) predicate, resolves the
        // default EMPTY needs_input sound and returns before any helper contact. Wait for
        // its single-flight flag to clear so the thread can't interleave with the phases
        // below (where a flipped tts_active would make its re-check fall back to enqueue).
        q.dispatch_earcon(ds_earcon::EarconEvent::NeedsInput, None, s.clone())
            .unwrap();
        let done = std::time::Instant::now() + Duration::from_secs(5);
        while q.oob_cue.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < done,
                "oob cue thread never exited"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            q.items.lock().unwrap().is_empty(),
            "a bypassed cue is never queued"
        );

        // A bypass raced by an in-flight oob cue is coalesced: Ok, nothing enqueued, and
        // the (simulated) in-flight thread keeps sole ownership of the flag.
        q.oob_cue.store(true, Ordering::SeqCst);
        q.dispatch_earcon(ds_earcon::EarconEvent::NeedsInput, None, s.clone())
            .unwrap();
        assert!(
            q.items.lock().unwrap().is_empty(),
            "a coalesced cue is dropped, not queued"
        );
        assert!(q.oob_cue.load(Ordering::SeqCst), "the flag stays owned");
        q.oob_cue.store(false, Ordering::SeqCst);

        // reply_done never escapes the ordered queue.
        q.dispatch_earcon(ds_earcon::EarconEvent::ReplyDone, None, s.clone())
            .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), 1);

        // An utterance already sounding: the idle gate routes needs_input to the queue
        // instead of mixing the cue over it.
        q.tts_active.store(true, Ordering::SeqCst);
        q.dispatch_earcon(ds_earcon::EarconEvent::NeedsInput, None, s.clone())
            .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), 2);
        q.tts_active.store(false, Ordering::SeqCst);

        // Hold cleared (terminal refocused) → ordered queue again.
        q.set_terminal_front(true);
        q.dispatch_earcon(ds_earcon::EarconEvent::NeedsInput, None, s)
            .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), 3);
    }

    #[test]
    fn gate_earcon_lets_a_queued_needs_input_pass_a_focus_only_hold() {
        // Full-duplex for the same reason as the dispatch test above: a dev machine's
        // live microphone must not add a mic hold to the focus-only one.
        let q = mk_queue();
        q.tts.set_full_duplex_active_for_test(true);
        q.set_pause_bg(true);
        q.set_terminal_front(true);
        q.set_terminal_front(false);

        // The speech-tail case: a needs_input routed to the QUEUE (idle gate) must not
        // park until refocus — the exemption admits it promptly and publishes in_flight.
        let gen0 = q.generation.load(Ordering::SeqCst);
        assert!(
            q.gate_earcon(ds_earcon::EarconEvent::NeedsInput, gen0),
            "needs_input must pass a focus-only hold"
        );
        assert!(q.in_flight.load(Ordering::SeqCst));
        q.in_flight.store(false, Ordering::SeqCst);

        // reply_done would park under the hold forever; bump the generation first so the
        // loop exits via its cancel arm and returns false (the requeue path) instead of
        // this test spinning on an infinite hold.
        q.generation.fetch_add(1, Ordering::SeqCst);
        assert!(
            !q.gate_earcon(ds_earcon::EarconEvent::ReplyDone, gen0),
            "a cancelled generation refuses the item"
        );
    }

    #[test]
    fn queued_cue_is_suppressed_when_mute_arrives_before_dequeue() {
        let q = mk_queue();
        q.enqueue_earcon(
            ds_earcon::EarconEvent::ReplyDone,
            None,
            Some("turn-1".into()),
        )
        .unwrap();
        q.set_muted(true);
        let item = q.edit_items_for_test(|q| q.pop_front()).unwrap();
        let QueueAction::Earcon(event) = item.action else {
            panic!("queued action must remain a cue");
        };
        assert!(
            q.cue_one(event).is_ok(),
            "mute must suppress before the absent helper is contacted"
        );
    }

    #[test]
    fn explicit_clears_prune_queued_cues_with_their_sessions() {
        let q = mk_queue();
        for session in [Some("a".into()), Some("b".into()), None] {
            q.enqueue_earcon(ds_earcon::EarconEvent::NeedsInput, None, session)
                .unwrap();
        }
        q.clear_session(Some("a".into()));
        let kept: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|item| item.session.clone())
            .collect();
        assert_eq!(kept, vec![Some("b".into()), None]);

        q.cancel_for_submit(Some("b".into()), false, true);
        let kept: Vec<_> = q
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|item| item.session.clone())
            .collect();
        assert_eq!(kept, vec![Some("b".into())]);
        q.clear();
        assert!(q.items.lock().unwrap().is_empty());
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

    /// In-flight claim carries the producer's `WiredAgent` so `activity.speaker`
    /// can highlight the matching Usage card. Non-client producers stay null.
    #[test]
    fn tts_status_sample_exposes_wireable_playing_source_only() {
        let q = mk_queue();
        let idle = q.tts_status_sample();
        assert!(!idle.speaking);
        assert_eq!(idle.speaker, None);
        assert_eq!(idle.queued, 0);

        q.enqueue(
            "hello".into(),
            None,
            Some(WiredAgent::ClaudeCode),
            Some("sess".into()),
        )
        .unwrap();
        {
            let mut items = q.items.lock().unwrap();
            let _ = q.claim_item(&mut items, 0);
        }
        q.set_active_for_test(true);
        // Nothing waiting, but the claimed utterance is still outstanding → 1.
        let speaking = q.tts_status_sample();
        assert!(speaking.speaking);
        assert_eq!(speaking.speaker, Some(WiredAgent::ClaudeCode));
        assert_eq!(speaking.queued, 1);

        // Unwired producers must not light a Usage card.
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: None,
            speech: true,
            utterance: None,
        });
        let speaking = q.tts_status_sample();
        assert!(speaking.speaking);
        assert_eq!(speaking.speaker, None);
        assert_eq!(speaking.queued, 1);

        // GreetSession-style path: wired-client attribution lights the matching card.
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: Some(WiredAgent::Grok),
            session: Some("open".into()),
            speech: true,
            utterance: None,
        });
        let speaking = q.tts_status_sample();
        assert!(speaking.speaking);
        assert_eq!(speaking.speaker, Some(WiredAgent::Grok));
        assert_eq!(speaking.queued, 1);

        q.set_active_for_test(false);
        let idle = q.tts_status_sample();
        assert!(!idle.speaking);
        assert_eq!(idle.speaker, None);
        assert_eq!(idle.queued, 0);
    }

    /// Stand in for the worker's claim so the record tests can drive resolution and fate
    /// without a live helper child.
    fn claim_utterance_for_test(q: &TtsQueue, id: u64) {
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: None,
            speech: true,
            utterance: Some(ds_status::UtteranceStatus {
                id,
                voice: None,
                language: None,
                warning: None,
                outcome: None,
            }),
        });
    }

    /// The `speak` handle: every admitted utterance gets one, nothing else consumes one.
    #[test]
    fn handles_are_issued_per_admitted_utterance_only() {
        let q = mk_queue();
        assert_eq!(q.enqueue("one".into(), None, None, None).unwrap(), Some(1));
        assert_eq!(q.enqueue("two".into(), None, None, None).unwrap(), Some(2));
        assert_eq!(
            q.enqueue("   \n".into(), None, None, None).unwrap(),
            None,
            "nothing to say ⇒ nothing to correlate"
        );
        q.enqueue_earcon(ds_earcon::EarconEvent::ReplyDone, None, None)
            .unwrap();
        assert_eq!(
            q.enqueue("three".into(), None, None, None).unwrap(),
            Some(3),
            "blank text and cues must not burn handles"
        );

        let items = q.items.lock().unwrap();
        assert_eq!(
            items
                .iter()
                .map(|item| item.utterance_id)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2), None, Some(3)],
            "each item carries the handle its producer was given"
        );
    }

    /// Status answers "what happened to that utterance": the handle at claim, the voice and
    /// language once the play gate resolves them, the fate once the queue is done with it.
    #[test]
    fn an_utterances_record_gains_its_resolution_then_its_outcome() {
        let q = mk_queue();
        claim_utterance_for_test(&q, 4);
        q.set_active_for_test(true);

        let claimed = q.tts_status_sample().utterance.expect("claim carries one");
        assert_eq!(claimed.id, 4);
        assert_eq!(claimed.voice, None, "no voice before the play gate");

        q.publish_resolved_utterance(
            "if_sara",
            "it",
            Some(ds_status::UtteranceWarning::VoiceLanguageMismatch),
        );
        let resolved = q.tts_status_sample().utterance.unwrap();
        assert_eq!(resolved.voice.as_deref(), Some("if_sara"));
        assert_eq!(resolved.language.as_deref(), Some("it"));
        assert_eq!(
            resolved.warning,
            Some(ds_status::UtteranceWarning::VoiceLanguageMismatch)
        );
        assert_eq!(resolved.outcome, None, "in flight has no outcome yet");
        assert!(
            q.tts_status_sample().recent_utterances.is_empty(),
            "an in-flight utterance is not a terminal record"
        );

        q.finish_utterance(ds_status::UtteranceOutcome::Spoken);
        let recent = q.tts_status_sample().recent_utterances;
        assert_eq!(recent[0].id, 4);
        assert_eq!(recent[0].voice.as_deref(), Some("if_sara"));
        assert_eq!(recent[0].outcome, Some(ds_status::UtteranceOutcome::Spoken));
    }

    /// The failure this surface exists for: speech off or a model that never loaded ends the
    /// utterance before any voice is picked, and the record has to say so rather than lie.
    #[test]
    fn an_utterance_dropped_before_the_play_gate_reports_no_voice() {
        let q = mk_queue();
        claim_utterance_for_test(&q, 2);
        q.finish_utterance(ds_status::UtteranceOutcome::Dropped);

        let recent = q.tts_status_sample().recent_utterances;
        assert_eq!(recent[0].id, 2);
        assert_eq!(recent[0].voice, None);
        assert_eq!(recent[0].language, None);
        assert_eq!(
            recent[0].outcome,
            Some(ds_status::UtteranceOutcome::Dropped)
        );
    }

    /// A cue has nothing to report, so it must not push an entry a producer could mistake
    /// for its own utterance.
    #[test]
    fn a_finished_cue_records_nothing() {
        let q = mk_queue();
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: None,
            speech: false,
            utterance: None,
        });
        q.finish_utterance(ds_status::UtteranceOutcome::Spoken);
        assert!(q.tts_status_sample().recent_utterances.is_empty());
    }

    #[test]
    fn the_record_ring_keeps_the_newest_handles_first() {
        let q = mk_queue();
        let overflow = RECENT_UTTERANCES_MAX as u64 + 2;
        for id in 1..=overflow {
            claim_utterance_for_test(&q, id);
            q.finish_utterance(ds_status::UtteranceOutcome::Spoken);
        }

        let recent = q.tts_status_sample().recent_utterances;
        assert_eq!(recent.len(), RECENT_UTTERANCES_MAX);
        assert_eq!(recent[0].id, overflow, "most recent first");
        assert_eq!(
            recent[RECENT_UTTERANCES_MAX - 1].id,
            3,
            "the two oldest fell off the end"
        );
    }

    /// A barge throws away utterances that never played. They ended, so they get records —
    /// otherwise a producer polling its handle cannot tell "discarded" from "still queued".
    #[test]
    fn a_barge_records_the_queued_utterances_it_threw_away() {
        let q = mk_queue();
        let first = q
            .enqueue("one".into(), None, None, Some("sess".into()))
            .unwrap();
        let second = q
            .enqueue("two".into(), None, None, Some("sess".into()))
            .unwrap();
        let other = q
            .enqueue("keep me".into(), None, None, Some("elsewhere".into()))
            .unwrap();

        q.clear_session(Some("sess".into()));

        let recent = q.tts_status_sample().recent_utterances;
        assert_eq!(
            recent.iter().map(|u| u.id).collect::<Vec<_>>(),
            vec![second.unwrap(), first.unwrap()],
            "both discarded handles are recorded, most recent first"
        );
        assert!(
            recent.iter().all(
                |u| u.outcome == Some(ds_status::UtteranceOutcome::Cancelled) && u.voice.is_none()
            ),
            "never played ⇒ cancelled with no voice"
        );
        assert!(
            !recent.iter().any(|u| Some(u.id) == other),
            "another window's queued utterance is untouched"
        );
    }

    /// A record-barge requeue is not a terminal outcome — the utterance is still going to be
    /// spoken, under the same handle. A hard cancel ends it, and the record says why.
    #[test]
    fn only_a_cancel_that_drops_the_item_ends_its_utterance() {
        let q = mk_queue();
        claim_utterance_for_test(&q, 5);
        q.record_cancel_kind(1, true);
        let mut resumed = item("interrupted");
        resumed.utterance_id = Some(5);
        q.settle_cancelled(resumed, 1);

        assert!(
            q.tts_status_sample().recent_utterances.is_empty(),
            "a resumable utterance has not ended"
        );
        assert_eq!(
            q.items
                .lock()
                .unwrap()
                .front()
                .and_then(|it| it.utterance_id),
            Some(5),
            "the requeued item keeps the handle its producer holds"
        );

        q.record_cancel_kind(2, false);
        let mut barged = item("barged");
        barged.utterance_id = Some(5);
        q.settle_cancelled(barged, 2);

        let recent = q.tts_status_sample().recent_utterances;
        assert_eq!(recent[0].id, 5);
        assert_eq!(
            recent[0].outcome,
            Some(ds_status::UtteranceOutcome::Cancelled)
        );
    }

    /// Only utterances count: cues share the queue but are not things to say.
    #[test]
    fn cues_never_count_toward_the_reported_depth() {
        let q = mk_queue();
        q.enqueue_earcon(ds_earcon::EarconEvent::ReplyDone, None, None)
            .unwrap();
        assert_eq!(
            q.tts_status_sample().queued,
            0,
            "a pending cue is not an utterance"
        );

        // Playing that cue must not invent one either.
        {
            let mut items = q.items.lock().unwrap();
            let _ = q.claim_item(&mut items, 0);
        }
        q.set_active_for_test(true);
        assert_eq!(
            q.tts_status_sample().queued,
            0,
            "a cue in flight is not an utterance"
        );

        // …while speech in flight does count, and stops counting when playback ends.
        q.enqueue(
            "spoken".into(),
            None,
            Some(WiredAgent::ClaudeCode),
            Some("sess".into()),
        )
        .unwrap();
        assert_eq!(q.tts_status_sample().queued, 1, "waiting utterance");
        {
            let mut items = q.items.lock().unwrap();
            let _ = q.claim_item(&mut items, 0);
        }
        assert_eq!(
            q.tts_status_sample().queued,
            1,
            "same utterance, now speaking"
        );
        q.set_active_for_test(false);
        assert_eq!(
            q.tts_status_sample().queued,
            0,
            "silence reports nothing outstanding"
        );
    }

    /// Queue-depth changes wake WaitModelStatus so hosts can render `activity.queued`.
    /// Failed enqueue / no-op prune must not spam the gate.
    #[test]
    fn queue_depth_changes_bump_status_gate() {
        let q = mk_queue();
        let seq0 = q.gate.seq();

        // Successful enqueue while TTS is already active (pending grows).
        q.set_active_for_test(true);
        q.enqueue(
            "waiting".into(),
            None,
            Some(WiredAgent::ClaudeCode),
            Some("a".into()),
        )
        .unwrap();
        let seq1 = q.gate.seq();
        assert_eq!(seq1, seq0.wrapping_add(1), "enqueue must bump gate");
        let sample = q.tts_status_sample();
        assert!(sample.speaking);
        assert_eq!(sample.speaker, None);
        assert_eq!(sample.queued, 1);

        // Claim empties pending and bumps, but the count holds: the utterance moved from
        // waiting to speaking, and both count.
        {
            let mut items = q.items.lock().unwrap();
            let _ = q.claim_item(&mut items, 0);
        }
        let seq2 = q.gate.seq();
        assert_eq!(seq2, seq1.wrapping_add(1), "claim must bump gate");
        assert_eq!(q.queue_depth.load(Ordering::SeqCst), 0, "nothing waits");
        assert_eq!(
            q.tts_status_sample().queued,
            1,
            "the spoken utterance counts"
        );

        // Failed enqueue (empty / oversize / full) must not bump.
        q.enqueue(
            "   ".into(),
            None,
            Some(WiredAgent::ClaudeCode),
            Some("a".into()),
        )
        .unwrap();
        assert_eq!(q.gate.seq(), seq2, "empty text must not bump");
        let _ = q
            .enqueue(
                "x".repeat(MAX_SPEAK_BYTES + 1),
                None,
                Some(WiredAgent::ClaudeCode),
                Some("a".into()),
            )
            .unwrap_err();
        assert_eq!(q.gate.seq(), seq2, "oversize reject must not bump");

        // Empty clear (nothing pending): the ONE bump is the speaking edge from
        // hard_cancel_in_flight's set_tts_active(false), never a depth bump.
        q.clear();
        let after_clear = q.gate.seq();
        assert_eq!(
            after_clear,
            seq2.wrapping_add(1),
            "empty clear: speaking edge bumps exactly once, depth does not"
        );
        q.clear();
        assert_eq!(
            q.gate.seq(),
            after_clear,
            "second empty clear must not bump depth"
        );

        // Per-session prune: only a prune that actually drops an item bumps.
        q.edit_items_for_test(|q| q.push_back(narr(Some("a"))));
        let before_prune = q.gate.seq();
        q.clear_session(Some("nobody".into()));
        assert_eq!(
            q.gate.seq(),
            before_prune,
            "clear_session that prunes nothing must not bump"
        );
        q.clear_session(Some("a".into()));
        assert_eq!(
            q.gate.seq(),
            before_prune.wrapping_add(1),
            "clear_session that prunes must bump once"
        );
        assert_eq!(q.tts_status_sample().queued, 0);

        // Resume push_front re-grows pending depth.
        let before_requeue = q.gate.seq();
        q.cancel_kind.lock().unwrap().insert(7, true);
        q.requeue_if_resuming(narr(Some("a")), 7);
        assert_eq!(
            q.gate.seq(),
            before_requeue.wrapping_add(1),
            "resume push_front must bump"
        );
        assert_eq!(q.tts_status_sample().queued, 1);
        assert_eq!(
            q.queue_depth.load(Ordering::SeqCst) as usize,
            q.items.lock().unwrap().len(),
            "the published depth must equal the real queue"
        );
    }

    /// Regression: `model_status` sampled pending depth under `items`, which cancel paths
    /// hold across helper-child I/O (`hard_cancel_in_flight` → `stop_fade`). A wedged
    /// helper then stalled every host status refresh. The read is lock-free now.
    #[test]
    fn tts_status_sample_does_not_wait_on_the_queue_lock() {
        let q = mk_queue();
        q.enqueue(
            "pending".into(),
            None,
            Some(WiredAgent::ClaudeCode),
            Some("a".into()),
        )
        .unwrap();

        // Stand in for a cancel path blocked inside `stop_fade` with `items` held.
        let items = q.items.lock().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let reader = Arc::clone(&q);
        let handle = std::thread::spawn(move || tx.send(reader.tts_status_sample()).unwrap());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("status read must not wait on the queue lock")
                .queued,
            1,
            "the lock-free read still reports the pending depth"
        );

        drop(items);
        handle.join().unwrap();
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
    fn overflowed_stream_narration_retries_once_after_queue_drain() {
        // Issue #62: fill one paused/background-style session, complete a streamed digest,
        // then make queue drainage (without another Codex event) admit it exactly once.
        let q = mk_queue();
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let session = "full";
        for _ in 0..MAX_SESSION_PENDING_ITEMS {
            q.enqueue("filler".into(), None, None, Some(session.into()))
                .unwrap();
        }

        let delta = ds_narrate::StreamBatch {
            key: "item-1".into(),
            payload: ds_narrate::BatchPayload::Delta {
                index: Some(0),
                text: "> Blocked digest.".into(),
            },
            is_final: false,
        };
        ds_narrate::deliver_batch(&paths, session, &delta, false, true, false, |utt| {
            q.enqueue_narration(
                utt.text.clone(),
                None,
                Some(session.into()),
                Some(utt.id.clone()),
                Some(utt.detection_text.clone()).filter(|s| !s.is_empty()),
            )
        })
        .unwrap();

        let completed = ds_narrate::StreamBatch {
            key: "item-1".into(),
            payload: ds_narrate::BatchPayload::Cumulative {
                text: "> Blocked digest.\n\nBody.".into(),
            },
            is_final: true,
        };
        let error =
            ds_narrate::deliver_batch(&paths, session, &completed, false, true, false, |utt| {
                q.enqueue_narration(
                    utt.text.clone(),
                    None,
                    Some(session.into()),
                    Some(utt.id.clone()),
                    Some(utt.detection_text.clone()).filter(|s| !s.is_empty()),
                )
            })
            .unwrap_err();
        assert!(error.contains("session speech queue is full"));

        q.edit_items_for_test(|q| q.clear());
        let mut admitted_id = None;
        ds_narrate::retry_pending(&paths, session, |utt| {
            admitted_id = Some(utt.id.clone());
            q.enqueue_narration(
                utt.text.clone(),
                None,
                Some(session.into()),
                Some(utt.id.clone()),
                Some(utt.detection_text.clone()).filter(|s| !s.is_empty()),
            )
        })
        .unwrap();
        assert_eq!(
            q.items
                .lock()
                .unwrap()
                .iter()
                .filter(|item| item.action.speech_text() == Some("Blocked digest."))
                .count(),
            1
        );

        // Re-offer the ID to model a producer crash after the engine accepted it but before
        // the state commit. Engine-side idempotency reports success without duplicating work.
        q.enqueue_narration(
            "Blocked digest.".into(),
            None,
            Some(session.into()),
            admitted_id,
            None,
        )
        .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), 1);
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
    fn passive_cues_cannot_consume_reserved_speech_capacity() {
        let global = mk_queue();
        for i in 0..MAX_PENDING_CUES {
            global
                .enqueue_earcon(
                    ds_earcon::EarconEvent::ReplyDone,
                    None,
                    Some(format!("cue-session-{i}")),
                )
                .unwrap();
        }
        assert!(
            global
                .enqueue_earcon(
                    ds_earcon::EarconEvent::ReplyDone,
                    None,
                    Some("overflow".into())
                )
                .unwrap_err()
                .contains("audio cue queue is full")
        );
        global
            .enqueue(
                "narration survives".into(),
                None,
                None,
                Some("overflow".into()),
            )
            .unwrap();

        let per_session = mk_queue();
        for _ in 0..MAX_SESSION_PENDING_CUES {
            per_session
                .enqueue_earcon(
                    ds_earcon::EarconEvent::NeedsInput,
                    None,
                    Some("held".into()),
                )
                .unwrap();
        }
        assert!(
            per_session
                .enqueue_earcon(
                    ds_earcon::EarconEvent::NeedsInput,
                    None,
                    Some("held".into())
                )
                .unwrap_err()
                .contains("session audio cue queue is full")
        );
        per_session
            .enqueue("still admitted".into(), None, None, Some("held".into()))
            .unwrap();
    }

    #[test]
    fn speech_saturation_leaves_bounded_cue_capacity() {
        let q = mk_queue();
        for i in 0..MAX_PENDING_ITEMS {
            q.enqueue("x".into(), None, None, Some(format!("session-{i}")))
                .unwrap();
        }
        q.enqueue_earcon(
            ds_earcon::EarconEvent::ReplyDone,
            None,
            Some("session-0".into()),
        )
        .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), MAX_PENDING_ITEMS + 1);
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
        q.edit_items_for_test(|q| q.extend([narr(Some("a")), narr(Some("b"))]));
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
        q.edit_items_for_test(|q| {
            q.extend([
                narr(Some("a")),
                narr(Some("b")),
                narr(None),
                narr(Some("a")),
            ])
        });
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
    fn clear_session_cancels_in_flight_only_when_playing_matches() {
        // Playing the TARGET session → the in-flight item is cancelled too.
        let q = mk_queue();
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("a".into()),
            speech: true,
            utterance: None,
        });
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
        *q2.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("b".into()),
            speech: true,
            utterance: None,
        });
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
        q.edit_items_for_test(|q| q.extend([narr(Some("a")), narr(Some("b"))]));
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

    /// Seed a queue whose worker is claiming `playing` and speaking.
    fn queue_playing(sessions: &[Option<&str>], playing: Option<&str>) -> Arc<TtsQueue> {
        let q = mk_queue();
        q.edit_items_for_test(|q| q.extend(sessions.iter().map(|s| narr(*s))));
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: playing.map(str::to_string),
            speech: true,
            utterance: None,
        });
        q
    }

    fn queued_sessions(q: &TtsQueue) -> Vec<Option<String>> {
        q.items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect()
    }

    #[test]
    fn cancel_for_submit_current_cancels_only_the_submitting_terminals_playback() {
        // A voice / hands-free submit in terminal "a" must not barge terminal "b": the
        // prune and the in-flight cancel cover the same set (`session_belongs_to_real`).
        let q = queue_playing(&[Some("a"), Some("b")], Some("b"));
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.cancel_for_submit(Some("a".into()), true, false);

        assert_eq!(
            queued_sessions(&q),
            vec![Some("b".into())],
            "current prunes only target's own queued items"
        );
        assert_eq!(
            q.generation.load(Ordering::SeqCst),
            gen_before,
            "another terminal's speech must survive a `current` clear"
        );
        assert!(q.tts_active.load(Ordering::SeqCst));

        // Playing the target itself → cancelled.
        let q2 = queue_playing(&[Some("a"), Some("b")], Some("a"));
        let gen_before2 = q2.generation.load(Ordering::SeqCst);
        q2.cancel_for_submit(Some("a".into()), true, false);
        assert!(
            q2.generation.load(Ordering::SeqCst) > gen_before2,
            "the submitting terminal's own playback is cancelled"
        );
        assert!(!q2.tts_active.load(Ordering::SeqCst));

        // Playing the target's Grok sticky digest → also the submitting terminal's, and
        // this branch prunes that sibling too (unlike `clear_session`, which keeps it).
        let q3 = queue_playing(&[Some("a"), Some("grok-stop:a")], Some("grok-stop:a"));
        let gen_before3 = q3.generation.load(Ordering::SeqCst);
        q3.cancel_for_submit(Some("a".into()), true, false);
        assert!(
            queued_sessions(&q3).is_empty(),
            "voice submit drops the target's sticky sibling as well"
        );
        assert!(
            q3.generation.load(Ordering::SeqCst) > gen_before3,
            "sticky playback of the submitting terminal is cancelled"
        );
    }

    #[test]
    fn current_scope_leaves_untagged_global_playback_alone() {
        // Prune and cancel cover the identical set: untagged MCP speech is `other`'s
        // scope per `CancelSpeechScope`, and the typing route (`clear_session`) already
        // leaves it alone.
        let q = queue_playing(&[None, Some("a")], None);
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.cancel_for_submit(Some("a".into()), true, false);

        assert_eq!(queued_sessions(&q), vec![None]);
        assert_eq!(
            q.generation.load(Ordering::SeqCst),
            gen_before,
            "untagged global speech is not the submitting terminal's"
        );
        assert!(q.tts_active.load(Ordering::SeqCst));
    }

    #[test]
    fn current_scope_is_idle_safe() {
        // `playing = None` is an IDLE worker, not "playing the untagged global session".
        let q = mk_queue();
        q.edit_items_for_test(|q| q.extend([narr(Some("a"))]));
        let gen_before = q.generation.load(Ordering::SeqCst);

        q.cancel_for_submit(Some("a".into()), true, false);

        assert!(queued_sessions(&q).is_empty());
        assert_eq!(
            q.generation.load(Ordering::SeqCst),
            gen_before,
            "an idle worker must not be hard-cancelled"
        );
    }

    #[test]
    fn both_scopes_still_cancel_a_foreign_in_flight_item() {
        // The `clear_on_input = ["current", "other"]` escape hatch for users who want
        // every terminal barged still works — via the `other` branch.
        let q = queue_playing(&[Some("a"), Some("b")], Some("b"));
        let gen_before = q.generation.load(Ordering::SeqCst);
        q.cancel_for_submit(Some("a".into()), true, true);
        assert!(q.items.lock().unwrap().is_empty());
        assert!(
            q.generation.load(Ordering::SeqCst) > gen_before,
            "a foreign in-flight item is cancelled once `other` is requested"
        );

        // …including untagged MCP speech ("other" = everything else, per CancelSpeechScope).
        let q2 = queue_playing(&[Some("a"), None], None);
        let gen_before2 = q2.generation.load(Ordering::SeqCst);
        q2.cancel_for_submit(Some("a".into()), true, true);
        assert!(
            q2.generation.load(Ordering::SeqCst) > gen_before2,
            "untagged in-flight speech is cancelled once `other` is requested"
        );
    }

    #[test]
    fn cancel_for_submit_other_keeps_only_target_and_cancels_only_when_playing_is_other() {
        // Playing a DIFFERENT session than the target → `other` cancels it.
        let q = mk_queue();
        q.edit_items_for_test(|q| q.extend([narr(Some("a")), narr(Some("b")), narr(None)]));
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("other".into()),
            speech: true,
            utterance: None,
        });
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
        q2.edit_items_for_test(|q| q.extend([narr(Some("a")), narr(Some("b"))]));
        q2.tts_active.store(true, Ordering::SeqCst);
        *q2.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("a".into()),
            speech: true,
            utterance: None,
        });
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

        // Playing sticky digests for the target → also not "other".
        let q3 = mk_queue();
        q3.edit_items_for_test(|q| {
            q.extend([narr(Some("a")), narr(Some("grok-stop:a")), narr(Some("b"))])
        });
        q3.tts_active.store(true, Ordering::SeqCst);
        *q3.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("grok-stop:a".into()),
            speech: true,
            utterance: None,
        });
        let gen_before3 = q3.generation.load(Ordering::SeqCst);
        q3.cancel_for_submit(Some("a".into()), false, true);
        let kept3: Vec<_> = q3
            .items
            .lock()
            .unwrap()
            .iter()
            .map(|it| it.session.clone())
            .collect();
        assert_eq!(kept3, vec![Some("a".into()), Some("grok-stop:a".into())]);
        assert_eq!(
            q3.generation.load(Ordering::SeqCst),
            gen_before3,
            "playing sticky of target must not cancel"
        );
    }

    #[test]
    fn cancel_for_submit_both_scopes_compose_to_an_empty_queue() {
        // `current` first drops the target's own queued items, then `other` retains ONLY
        // the target's items (now none) — the combination empties the queue entirely.
        let q = mk_queue();
        q.edit_items_for_test(|q| {
            q.extend([
                narr(Some("a")),
                narr(Some("b")),
                narr(None),
                narr(Some("a")),
            ])
        });
        q.cancel_for_submit(Some("a".into()), true, true);
        assert!(q.items.lock().unwrap().is_empty());
    }

    /// Lock-in for the worker's claim ordering: `run` snapshots the generation BEFORE its
    /// pause check, so a pause landing in that window still invalidates the claim and its
    /// requeue intent (keyed to the pre-bump generation) still resolves.
    #[test]
    fn a_pause_after_the_claim_snapshot_abandons_and_requeues_the_item() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(Some("a"))));
        let gen0 = q.generation.load(Ordering::SeqCst); // the worker's snapshot point
        // Holding `items` across the pause is safe only because `pause_with_cause` →
        // `set_tts_active` must not take `items` (see its doc) and `record_cancel_kind`
        // takes only `cancel_kind` — otherwise a change there hangs CI instead of failing it.
        q.pause_for_record(); // pause lands inside the window
        let item = q.claim_item(&mut items, 0);
        drop(items);

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            gen0,
            "the pause's bump must invalidate a claim taken under the pre-pause snapshot"
        );
        q.requeue_if_resuming(item, gen0);
        assert_eq!(
            q.items.lock().unwrap().len(),
            1,
            "the abandoned item is held for resume, not dropped"
        );
    }

    #[test]
    fn a_paused_queue_claims_nothing() {
        let mut q: VecDeque<Item> = VecDeque::new();
        q.push_back(narr(Some("a")));
        let active = Some("a".to_string());
        assert_eq!(claimable_pos(false, &q, &active), Some(0));
        assert_eq!(
            claimable_pos(true, &q, &active),
            None,
            "a paused queue claims nothing, however selectable the head is"
        );
        assert_eq!(claimable_pos(true, &VecDeque::new(), &None), None);
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
        // A Dictation-tagged pause must survive the barge watcher's auto-resume. Drive
        // pause_for_record directly so this isolates the resume-side guard.
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

        q.edit_items_for_test(|q| q.push_back(narr(Some("a"))));
        assert!(q.is_busy(), "anything queued counts as busy");
    }

    /// Regression: sampling `in_flight` before taking `items` allowed the worker to replace the
    /// final queued item with in-flight state while the reader retained the stale `false` sample.
    #[test]
    fn is_busy_couples_the_dequeue_to_in_flight_transition() {
        let q = mk_queue();
        let mut items = q.items.lock().unwrap();
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(Some("a"))));

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

        q.edit_items_locked_for_test(&mut items, |q| q.pop_front())
            .expect("queued item");
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
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(Some("a"))));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.clear();
        });
        started_rx.recv().unwrap();

        let selected_generation = q.generation.load(Ordering::SeqCst);
        let _item = q.claim_item(&mut items, 0);
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
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(Some("a"))));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.clear_session(Some("a".into()));
        });
        started_rx.recv().unwrap();

        let selected_generation = q.generation.load(Ordering::SeqCst);
        let _item = q.claim_item(&mut items, 0);
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
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(Some("other"))));

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let clearer = Arc::clone(&q);
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            clearer.cancel_for_submit(Some("current".into()), false, true);
        });
        started_rx.recv().unwrap();

        let selected_generation = q.generation.load(Ordering::SeqCst);
        let _item = q.claim_item(&mut items, 0);
        drop(items);
        handle.join().unwrap();

        assert_ne!(
            q.generation.load(Ordering::SeqCst),
            selected_generation,
            "the other-scope clear must cancel a foreign item claimed under its queue lock"
        );
    }

    /// Hold `StatusGate::bump` between `tts_active=false` and the generation bump. The old
    /// scoped-cancel shape had already released `items` at that point, allowing this helper
    /// to claim the surviving item under the stale generation. The fixed shape still holds
    /// `items`, so the claim can proceed only after the cancellation transition completes.
    fn claim_survivor_across_scoped_cancel(
        q: &Arc<TtsQueue>,
        survivor: Option<&str>,
        cancel: impl FnOnce(Arc<TtsQueue>) + Send + 'static,
    ) -> (u64, u64) {
        let mut items = q.items.lock().unwrap();
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(survivor)));
        q.tts_active.store(true, Ordering::SeqCst);
        let transition = q.gate.hold_transition_for_test();

        let clearer = Arc::clone(q);
        let handle = std::thread::spawn(move || cancel(clearer));
        drop(items);

        let deadline = Instant::now() + Duration::from_secs(2);
        while q.tts_active.load(Ordering::SeqCst) {
            assert!(
                Instant::now() < deadline,
                "scoped cancellation never reached its status transition"
            );
            std::thread::yield_now();
        }

        let selected_generation = match q.items.try_lock() {
            // This is the buggy ordering: the queue lock escaped before the generation bump.
            Ok(mut items) => {
                // Snapshot under the escaped guard, still ahead of the bump `transition` is
                // holding back — that pre-bump value is what records the regression.
                let generation = q.generation.load(Ordering::SeqCst);
                // try_lock already recorded the regression; release `seq` before claiming,
                // because `claim_item`'s depth bump re-locks it (std mutexes aren't reentrant).
                drop(transition);
                let _ = q.claim_item(&mut items, 0);
                generation
            }
            // The fixed ordering: let the cancellation finish, then claim its survivor.
            Err(std::sync::TryLockError::WouldBlock) => {
                drop(transition);
                let mut items = q.items.lock().unwrap();
                // Post-bump: the cancellation released `items` only after finishing.
                let generation = q.generation.load(Ordering::SeqCst);
                let _ = q.claim_item(&mut items, 0);
                generation
            }
            Err(std::sync::TryLockError::Poisoned(e)) => panic!("items lock poisoned: {e}"),
        };
        handle.join().unwrap();
        let final_generation = q.generation.load(Ordering::SeqCst);
        (selected_generation, final_generation)
    }

    #[test]
    fn session_clear_does_not_cancel_a_foreign_item_claimed_after_pruning() {
        let q = mk_queue();
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("cleared".into()),
            speech: true,
            utterance: None,
        });

        let (selected_generation, final_generation) =
            claim_survivor_across_scoped_cancel(&q, Some("survivor"), |clearer| {
                clearer.clear_session(Some("cleared".into()));
            });

        assert_eq!(
            selected_generation, final_generation,
            "a foreign-session survivor must be claimed after the scoped generation bump"
        );
    }

    #[test]
    fn current_scope_does_not_cancel_a_foreign_item_claimed_after_pruning() {
        let q = mk_queue();
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("current".into()),
            speech: true,
            utterance: None,
        });

        let (selected_generation, final_generation) =
            claim_survivor_across_scoped_cancel(&q, Some("other"), |clearer| {
                clearer.cancel_for_submit(Some("current".into()), true, false);
            });

        assert_eq!(
            selected_generation, final_generation,
            "another session's survivor must be claimed after the current-scope bump"
        );
    }

    #[test]
    fn other_scope_does_not_cancel_the_target_item_claimed_after_pruning() {
        let q = mk_queue();
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("other".into()),
            speech: true,
            utterance: None,
        });

        let (selected_generation, final_generation) =
            claim_survivor_across_scoped_cancel(&q, Some("current"), |clearer| {
                clearer.cancel_for_submit(Some("current".into()), false, true);
            });

        assert_eq!(
            selected_generation, final_generation,
            "the retained target item must be claimed after the other-scope bump"
        );
    }

    #[test]
    fn global_session_identity_is_distinct_from_an_idle_worker() {
        // The idle half: with NO claimed item (`playing` = None), a global-session
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
        q.edit_items_locked_for_test(&mut items, |q| q.push_back(narr(None)));
        let selected_generation = q.generation.load(Ordering::SeqCst);
        let _item = q.claim_item(&mut items, 0);
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
        q.set_pause_bg(true);
        q.set_terminal_front(false);
        q.edit_items_for_test(|q| q.extend([narr(Some("a")), narr(Some("a"))]));
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

    fn queue_with_closing_first_speak(delay_ms: &str) -> (tempfile::TempDir, Arc<TtsQueue>) {
        let bin = crate::tts::wedge_recovery_tests::fake_helper_bin();
        let dir = tempfile::tempdir().unwrap();
        let q = TtsQueue::test_stub_with_helper(
            dir.path(),
            bin,
            crate::tts::TtsManagerTestOptions::default()
                .with_first_spawn_env(&[("DONTSPEAK_FAKE_CLOSE_ON_SPEAK_MS", delay_ms)]),
        );
        *q.config.lock().unwrap() = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            ..VoiceConfig::default()
        };
        (dir, q)
    }

    fn spawn_test_speech(
        q: &Arc<TtsQueue>,
        text: &'static str,
    ) -> std::thread::JoinHandle<(SpeechOutcome, usize)> {
        let worker = Arc::clone(q);
        std::thread::spawn(move || {
            let spoken = item(text);
            let QueueAction::Speech {
                text,
                language,
                tts_args,
            } = &spoken.action
            else {
                unreachable!()
            };
            worker.play_speech(
                &spoken,
                worker.generation.load(Ordering::SeqCst),
                text,
                language,
                tts_args.as_deref(),
            )
        })
    }

    fn wait_for_first_speak(q: &TtsQueue) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while q.tts.last_speak_progress() != 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            q.tts.last_speak_progress(),
            1,
            "the first speak never reached the fake child"
        );
    }

    fn restart_test_child(q: &TtsQueue) {
        q.tts.set_full_duplex_pref(true);
        q.tts.restart_if_full_duplex_stale();
    }

    #[test]
    fn a_queued_speak_retries_once_after_the_warm_child_closes() {
        let (_dir, q) = queue_with_closing_first_speak("5000");
        let handle = spawn_test_speech(&q, "survive the reload");
        wait_for_first_speak(&q);
        restart_test_child(&q);

        let (outcome, resume_skip) = handle.join().expect("speech test thread panicked");

        assert_eq!(
            outcome,
            SpeechOutcome::Done(ds_status::UtteranceOutcome::Spoken)
        );
        assert_eq!(
            resume_skip, 2,
            "progress 2 proves the retry completed on the replacement child"
        );
        q.set_tts_active(false); // the real worker clears this after play_speech returns
        q.tts.set_enabled(false);
    }

    #[test]
    fn cancellation_during_child_close_does_not_leave_tts_active() {
        let (_dir, q) = queue_with_closing_first_speak("5000");
        let handle = spawn_test_speech(&q, "cancel during the reload");
        wait_for_first_speak(&q);
        q.generation.fetch_add(1, Ordering::SeqCst);
        restart_test_child(&q);

        let (outcome, resume_skip) = handle.join().expect("speech test thread panicked");
        assert_eq!(outcome, SpeechOutcome::Requeue);
        assert_eq!(resume_skip, 1);
        assert!(
            !q.tts_active.load(Ordering::SeqCst),
            "the retry gate must not strand playback active when cancellation wins"
        );
        q.tts.set_enabled(false);
    }

    #[test]
    fn only_resume_capable_builtin_transport_errors_are_retried() {
        use std::io::ErrorKind;

        for kind in [
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionAborted,
            ErrorKind::ConnectionReset,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
        ] {
            let error = std::io::Error::new(kind, "child replaced");
            assert!(should_retry_speak(
                Some(ds_config::TtsEngine::BuiltIn),
                true,
                &error
            ));
            assert!(!should_retry_speak(
                Some(ds_config::TtsEngine::System),
                true,
                &error
            ));
            assert!(!should_retry_speak(
                Some(ds_config::TtsEngine::BuiltIn),
                false,
                &error
            ));
        }
        assert!(!should_retry_speak(
            Some(ds_config::TtsEngine::BuiltIn),
            true,
            &std::io::Error::other("helper rejected the utterance")
        ));
    }

    /// Pins the cancel-before-error ordering inside the readiness poll: a generation bump must
    /// come back `Cancelled` (so the worker can requeue a merely-paused item) even when a
    /// manager error is also present — `Unavailable` here would drop the item instead.
    #[test]
    fn wait_until_ready_cancellation_beats_a_manager_error() {
        let q = mk_queue();
        q.tts.restart_if_crashed(); // nonexistent helper: the failed spawn records last_error
        assert!(
            q.tts.last_error().is_some(),
            "premise: the failed spawn must leave a manager error"
        );
        let stale = q.generation.load(Ordering::SeqCst).wrapping_add(1);
        assert_eq!(
            q.wait_until_ready(Some(ds_config::TtsEngine::BuiltIn), stale),
            ReadyOutcome::Cancelled
        );
    }

    /// A manager-level error fails fast as `Unavailable` instead of consuming the deadline.
    #[test]
    fn wait_until_ready_reports_a_manager_error_without_consuming_the_deadline() {
        let q = mk_queue();
        q.tts.restart_if_crashed();
        assert!(
            q.tts.last_error().is_some(),
            "premise: the failed spawn must leave a manager error"
        );
        let started = std::time::Instant::now();
        let outcome = q.wait_until_ready(
            Some(ds_config::TtsEngine::BuiltIn),
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
            Some(ds_config::TtsEngine::BuiltIn),
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

    #[test]
    fn wait_until_ready_names_the_selected_model() {
        let q = mk_queue();
        q.tts.suppress_heal_for_test();
        q.tts.set_tts_selection(ds_config::TtsModel::Qwen);
        let outcome = q.wait_until_ready_with_timeout(
            Some(ds_config::TtsEngine::BuiltIn),
            q.generation.load(Ordering::SeqCst),
            Duration::from_millis(10),
        );
        assert_eq!(
            outcome,
            ReadyOutcome::Unavailable(
                "timed out waiting for the Qwen3-TTS model to become ready".to_string()
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
        let q = TtsQueue::test_stub_with_helper(
            dir.path(),
            bin,
            crate::tts::TtsManagerTestOptions::default(),
        );
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
            Some(ds_config::TtsEngine::BuiltIn),
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
    /// the handshake (then unbounded: forever, mic closed, `stop` unheard).
    #[test]
    fn wait_until_ready_does_not_block_on_a_wedged_child_spawn() {
        let bin = crate::tts::wedge_recovery_tests::fake_helper_bin();
        let dir = tempfile::tempdir().unwrap();
        let q = TtsQueue::test_stub_with_helper(
            dir.path(),
            bin,
            crate::tts::TtsManagerTestOptions::default()
                .with_first_spawn_env(&[("DONTSPEAK_FAKE_WEDGE_PRE_READY", "1")])
                .with_ready_timeout(Duration::from_millis(3000)),
        );
        // Big enough that a wait that DID ride the handshake would visibly overshoot the
        // 1.5 s assertion below; small enough to keep the cleanup join bounded.

        let started = std::time::Instant::now();
        let outcome = q.wait_until_ready_with_timeout(
            Some(ds_config::TtsEngine::BuiltIn),
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
                Some(ds_config::TtsEngine::BuiltIn),
                q.generation.load(Ordering::SeqCst)
            ),
            ReadyOutcome::Ready
        );
    }

    /// Regression (audit): `pause_bg` focus loss DURING the readiness wait must
    /// re-enter the hold gate once the model becomes ready — the old shape went straight to
    /// playback, speaking into whatever app was frontmost after up to 60 s of warm-up.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn gate_item_rechecks_the_focus_hold_after_the_readiness_wait() {
        let q = mk_queue();
        q.tts.suppress_heal_for_test();
        *q.config.lock().unwrap() = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            ..VoiceConfig::default()
        };
        q.set_terminal_front(true); // arm the self-disabling focus gate
        q.set_pause_bg(true);

        let gen0 = q.generation.load(Ordering::SeqCst);
        let gated = Arc::clone(&q);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            done_tx
                .send(gated.gate_item(&item("held across warm-up"), gen0, "en", None))
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
    fn set_terminal_front_latches_seen_and_set_pause_bg_publishes() {
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

        q.set_pause_bg(true);
        assert!(q.pause_bg.load(Ordering::SeqCst));
        q.set_pause_bg(false);
        assert!(!q.pause_bg.load(Ordering::SeqCst));
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
            q.pause_bg.store(pause_bg, Ordering::SeqCst);
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
    fn end_session_barges_the_window_but_keeps_the_agent_assignment() {
        let q = mk_queue();
        q.agent_voices.lock().unwrap().insert(
            (Some(WiredAgent::ClaudeCode), String::new()),
            "af_sarah".to_string(),
        );
        q.edit_items_for_test(|q| q.extend([narr(Some("other")), narr(Some("s1"))]));
        q.tts_active.store(true, Ordering::SeqCst);
        *q.playing.lock().unwrap() = Some(PlayingClaim {
            source: None,
            session: Some("s1".into()),
            speech: true,
            utterance: None,
        });
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
        // s1's own in-flight item is cancelled (playing matched).
        assert!(q.generation.load(Ordering::SeqCst) > gen_before);
        assert!(!q.tts_active.load(Ordering::SeqCst));
        // The agent keeps its voice past the window's life (runtime-stable assignment):
        // the next Claude Code terminal must speak the same voice.
        assert_eq!(
            q.agent_voices
                .lock()
                .unwrap()
                .get(&(Some(WiredAgent::ClaudeCode), String::new())),
            Some(&"af_sarah".to_string())
        );
    }

    #[test]
    fn assign_agent_voice_records_and_reuses_the_pick_per_agent() {
        let q = mk_queue();
        let pool = vec!["af_sarah".to_string(), "am_adam".to_string()];

        let v1 = q.assign_agent_voice(Some(WiredAgent::ClaudeCode), "en", &pool);
        assert!(pool.contains(&v1), "the pick comes from the pool");
        assert_eq!(
            q.agent_voices
                .lock()
                .unwrap()
                .get(&(Some(WiredAgent::ClaudeCode), "en".to_string())),
            Some(&v1)
        );
        // The same agent reuses its recorded pick.
        assert_eq!(
            q.assign_agent_voice(Some(WiredAgent::ClaudeCode), "en", &pool),
            v1
        );
        // A different agent picks among the remaining free voices — with one voice left,
        // that's deterministic: the other one.
        let v2 = q.assign_agent_voice(Some(WiredAgent::Codex), "en", &pool);
        assert!(pool.contains(&v2));
        assert_ne!(v2, v1, "a free voice remains, so agents must not share");
    }

    #[test]
    fn resolve_engine_voice_off_returns_none() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: Vec::new(), // empty ladder = off
            ..VoiceConfig::default()
        };
        assert_eq!(q.resolve_engine_voice(&cfg, None, None), None);
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
            tts_voices: ds_config::TtsVoicePools {
                system: vec!["Ava (Premium)".to_string()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), None),
            Some((ds_config::TtsEngine::System, "Ava (Premium)".to_string()))
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_uses_the_selected_models_pool() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: ds_config::TtsModel::Chatterbox,
            ..VoiceConfig::default()
        };
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), None),
            Some((ds_config::TtsEngine::BuiltIn, "default".to_string()))
        );
        assert_eq!(
            q.agent_voices
                .lock()
                .unwrap()
                .get(&(Some(WiredAgent::ClaudeCode), String::new())),
            Some(&"default".to_string())
        );
    }

    // Kokoro is usable everywhere EXCEPT Intel macOS without an onnxruntime dylib present
    // (a runtime capability, not a static (os,arch) fact — see `intel_mac_builtin_ort_available`),
    // so gate out only that one platform, matching `voice.rs`'s own tests of this ladder.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_kokoro_with_pool_delegates_to_agent_assignment() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec!["af_sarah".to_string(), "am_adam".to_string()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        let (engine, voice) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), None)
            .expect("Kokoro is usable on this build");
        assert_eq!(engine, ds_config::TtsEngine::BuiltIn);
        assert!(cfg.tts_voices.kokoro.contains(&voice));
        // The source is threaded through to the agent-voice map.
        assert_eq!(
            q.agent_voices
                .lock()
                .unwrap()
                .get(&(Some(WiredAgent::ClaudeCode), String::new())),
            Some(&voice)
        );
        // Every later resolution for the same agent — any window, any request — reuses it.
        let (_, again) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), None)
            .expect("Kokoro again");
        assert_eq!(again, voice);
        assert_eq!(q.agent_voices.lock().unwrap().len(), 1);
        // A second agent claims the remaining free voice — no sharing while one is free.
        let (_, other) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::Codex), None)
            .expect("Kokoro for Codex");
        assert_ne!(other, voice);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_kokoro_picks_a_voice_that_owns_the_language() {
        // The gap this closes: an Italian reply used to be spoken by whichever English voice
        // the agent held. The pool is narrowed to voices that own the detected language before
        // the agent assignment runs, and each language keeps its own sticky assignment.
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec![
                    "af_sarah".to_string(),
                    "bf_emma".to_string(),
                    "if_sara".to_string(),
                ],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        let (_, italian) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("it"))
            .expect("Kokoro is usable on this build");
        assert_eq!(italian, "if_sara");

        let (_, english) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("en"))
            .expect("Kokoro again");
        assert!(["af_sarah", "bf_emma"].contains(&english.as_str()));

        // Both assignments coexist: switching language back must not re-roll the other.
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("it"))
                .map(|(_, v)| v),
            Some(italian)
        );
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("en"))
                .map(|(_, v)| v),
            Some(english)
        );
    }

    // System is only buildable on macOS/Windows, and seeding `system_voices` keeps `say -v ?`
    // out of the test — the borrow path is otherwise identical for every catalog.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn resolve_engine_voice_borrows_a_catalog_voice_for_an_unconfigured_language() {
        let q = mk_queue();
        let mk = |id: &str, tag: &str| ds_tts::SpeakerVoice {
            id: id.into(),
            name: id.into(),
            language_tag: tag.into(),
            downloadable: false,
            gender: None,
            quality: None,
        };
        q.system_voices
            .set(vec![
                mk("Samantha", "en-US"),
                mk("Anna", "de-DE"),
                mk("Otto", "de-DE"),
            ])
            .expect("fresh queue");
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::System],
            tts_voices: ds_config::TtsVoicePools {
                system: vec!["Samantha".to_string()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        // The pool owns no German, so the choice is made among the catalog's German voices.
        let (_, german) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("de"))
            .expect("System is usable on this build");
        assert!(["Anna", "Otto"].contains(&german.as_str()));
        // Borrowed voices are sticky too: the roll happens once per agent and language.
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("de"))
                .map(|(_, v)| v),
            Some(german)
        );
        // The configured pool still serves the language it does own.
        assert_eq!(
            q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some("en"))
                .map(|(_, v)| v),
            Some("Samantha".to_string())
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_leaves_language_agnostic_models_alone() {
        // Chatterbox conditions on the language argument, so its voice must survive narrowing
        // untouched — the shared path must not strand models whose voices own no language.
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: ds_config::TtsModel::Chatterbox,
            ..VoiceConfig::default()
        };
        for language in ["en", "it", "ja"] {
            assert_eq!(
                q.resolve_engine_voice(&cfg, Some(WiredAgent::ClaudeCode), Some(language)),
                Some((ds_config::TtsEngine::BuiltIn, "default".to_string()))
            );
        }
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn resolve_engine_voice_kokoro_empty_pool_is_none() {
        // No fallback voice exists: an empty pool (only constructible directly — load()
        // clamps it back to the default pool) resolves to nothing and claims nothing.
        let q = mk_queue();
        let empty_pool = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec![],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        assert_eq!(
            q.resolve_engine_voice(&empty_pool, Some(WiredAgent::ClaudeCode), None),
            None
        );
        assert!(q.agent_voices.lock().unwrap().is_empty());
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn unwired_source_with_a_pool_claims_a_stable_voice() {
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec!["af_sarah".to_string(), "am_adam".to_string()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        let (_, voice) = q
            .resolve_engine_voice(&cfg, None, None)
            .expect("Kokoro is usable on this build");
        assert!(cfg.tts_voices.kokoro.contains(&voice));
        assert_eq!(
            q.agent_voices.lock().unwrap().get(&(None, String::new())),
            Some(&voice)
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn greeting_claims_the_agent_voice_reused_by_later_replies() {
        // Locking-at-open: the SessionStart greeting claims the agent's voice, and a later
        // reply from the same agent resolves to that exact voice.
        let q = mk_queue();
        let cfg = VoiceConfig {
            tts_engine_ladder: vec![ds_config::TtsEngine::BuiltIn],
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec!["af_sarah".to_string(), "am_adam".to_string()],
                ..Default::default()
            },
            greet: true,
            ..VoiceConfig::default()
        };
        *q.config.lock().unwrap() = cfg.clone();

        q.greet_session(Some(WiredAgent::Codex), Some("sess-1".to_string()));

        let claimed = q
            .agent_voices
            .lock()
            .unwrap()
            .get(&(Some(WiredAgent::Codex), String::new()))
            .cloned()
            .expect("greeting claims the agent voice at open");
        // The queued greeting carries the claimed voice as its per-item override.
        {
            let items = q.items.lock().unwrap();
            match &items[0].action {
                QueueAction::Speech {
                    tts_args: Some(args),
                    ..
                } => {
                    assert_eq!(
                        args.for_target(ds_config::TtsEngine::BuiltIn, ds_config::TtsModel::Kokoro)
                            .and_then(ds_config::TtsTargetArgs::voice),
                        Some(claimed.as_str())
                    );
                }
                other => panic!("greeting must queue speech, got {other:?}"),
            }
        }
        // A later reply (any session of the same agent) speaks the same voice.
        let (_, later) = q
            .resolve_engine_voice(&cfg, Some(WiredAgent::Codex), None)
            .expect("Kokoro later reply");
        assert_eq!(later, claimed);
    }

    /// Solid English turn corpus: what a short digest falls back to when it cannot
    /// classify itself.
    const EN_DETECTION: &str = "This assistant reply is written entirely in clear English prose so language detection has a solid corpus for the whole turn. Short digests alone can false-positive as French or Portuguese.";
    const IT_QUOTE: &str = "Oggi è una giornata tranquilla e luminosa, e mi fa davvero piacere poter scambiare due parole con te in italiano, una lingua che ha un ritmo caldo e musicale.";
    const EN_QUOTE: &str = "Today has been a calm and bright sort of day, and it is genuinely a pleasure to trade a few words with you in English, a language with a rather different rhythm.";

    /// Languages of the queued items, in order.
    fn queued_languages(q: &TtsQueue) -> Vec<String> {
        q.items
            .lock()
            .unwrap()
            .iter()
            .filter_map(|item| match &item.action {
                QueueAction::Speech { language, .. } => Some(language.clone()),
                QueueAction::Earcon(_) => None,
            })
            .collect()
    }

    #[test]
    fn each_narration_chunk_carries_its_own_language() {
        // Both quotes of one reply arrive under the same turn corpus; the second must not
        // inherit the first's language.
        let q = mk_queue();
        q.config.lock().unwrap().tts_model = ds_config::TtsModel::Kokoro;
        let corpus = format!("> {IT_QUOTE}\n\n> {EN_QUOTE}");
        for (i, quote) in [IT_QUOTE, EN_QUOTE].iter().enumerate() {
            q.enqueue_narration(
                (*quote).into(),
                Some(WiredAgent::ClaudeCode),
                Some("sess".into()),
                Some(format!("n{i}")),
                Some(corpus.clone()),
            )
            .unwrap();
        }
        assert_eq!(queued_languages(&q), vec!["it", "en"]);
    }

    #[test]
    fn a_digest_that_cannot_classify_itself_takes_the_turn_corpus() {
        let q = mk_queue();
        q.config.lock().unwrap().tts_model = ds_config::TtsModel::Kokoro;
        // Short FR/PT false-friend digest inside an English turn.
        q.enqueue_narration(
            "Bon courage.".into(),
            Some(WiredAgent::ClaudeCode),
            Some("sess".into()),
            Some("n1".into()),
            Some(EN_DETECTION.into()),
        )
        .unwrap();
        // Same digest with no corpus behind it: English, never a coin flip.
        q.enqueue_narration(
            "Bon courage.".into(),
            Some(WiredAgent::ClaudeCode),
            Some("sess".into()),
            Some("n2".into()),
            None,
        )
        .unwrap();
        assert_eq!(queued_languages(&q), vec!["en", "en"]);
    }

    #[test]
    fn mcp_enqueue_classifies_the_spoken_text() {
        let q = mk_queue();
        q.config.lock().unwrap().tts_model = ds_config::TtsModel::Kokoro;
        for text in ["hello from MCP", IT_QUOTE] {
            q.enqueue(text.into(), None, None, Some("s".into()))
                .unwrap();
        }
        assert_eq!(queued_languages(&q), vec!["en", "it"]);
    }

    #[test]
    fn detection_corpus_over_limit_is_truncated_not_rejected() {
        let q = mk_queue();
        let huge = format!("{}{}", EN_DETECTION, "x".repeat(MAX_SPEAK_BYTES));
        assert!(huge.len() > MAX_SPEAK_BYTES);
        q.enqueue_narration(
            "digest".into(),
            None,
            Some("s".into()),
            Some("id".into()),
            Some(huge),
        )
        .unwrap();
        assert_eq!(q.items.lock().unwrap().len(), 1);
    }

    #[test]
    fn forget_narration_session_readmits_that_sessions_ids() {
        let q = mk_queue();
        let admit = |session: &str, id: &str| {
            q.enqueue_narration(
                "a".into(),
                Some(WiredAgent::Grok),
                Some(session.into()),
                Some(id.into()),
                None,
            )
            .unwrap();
        };
        admit("real", "id1");
        admit("grok-stop:real", "id2");
        admit("other", "id3");
        q.edit_items_for_test(|q| q.clear());

        q.forget_narration_session("real");
        // Only that session's ids are released; other sessions (including the Grok sticky
        // sibling, which SessionEnd reclaims on its own) keep deduping.
        admit("real", "id1");
        admit("grok-stop:real", "id2");
        admit("other", "id3");
        assert_eq!(q.items.lock().unwrap().len(), 1);
    }
}
