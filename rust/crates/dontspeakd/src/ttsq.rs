//! Engine-owned TTS queue — the single serializer for all speech.
//!
//! Producers (the `Speak` / `SpeakNarration` RPC handlers) enqueue whole blocks onto ONE
//! plain FIFO — there is no "reply vs narration" kind and no cap; what gets spoken is
//! decided upstream by the `narrate` setting. ONE worker thread plays them in order on the
//! WARM child (`TtsManager`), so there is no per-block model reload. The warm child stays
//! DUMB: ordering, the mic feedback gate, and the barge/pause/resume policy all live here.
//!
//! Playback granularity: each block is sent to the warm child as ONE `tts.speak`. The
//! child splits the block through the shared text splitter
//! (`ds_tts::batch::chunk_text`) and streams it gaplessly — the ONNX path then ramps
//! phoneme batches per chunk (`batch::stream_batches`), the Core ML path synthesizes each
//! chunk whole — so there is no per-block reload and no per-sentence splitting here.
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
//! frees. The interrupted item is re-spoken from its top (a block streams gaplessly,
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

/// Rotates greeting templates so consecutive opens don't repeat the same line.
static GREET_ROTATION: AtomicUsize = AtomicUsize::new(0);

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
    if let Some(v) = assignments.get(session) {
        return v.clone();
    }
    pool.iter()
        .find(|v| !assignments.values().any(|a| a == *v))
        .cloned()
        .unwrap_or_else(|| pool[assignments.len() % pool.len()].clone())
}

/// One queued unit of speech. The queue is a plain FIFO — there is no "narration vs
/// reply" kind and no cap: whatever the narration layer enqueues (per the `narrate`
/// setting) is played in order. Items differ only by their optional per-call voice/rate
/// and the session they belong to.
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

pub struct TtsQueue {
    items: Mutex<VecDeque<Item>>,
    cv: Condvar,
    /// Bumped on every barge/pause; the worker abandons its in-flight item when
    /// the generation moves past the one it dequeued under.
    generation: AtomicU64,
    /// True while paused for a record-barge (resume mode): the worker stops
    /// dequeuing until `resume()`.
    paused: AtomicBool,
    /// True while the worker is actively playing an item (set around playback,
    /// cleared when it returns to waiting). Read-only signal for `Status`.
    tts_active: AtomicBool,
    /// Whether a terminal is the frontmost app — published by the engine poll thread
    /// each tick (the worker thread can't call NSWorkspace; it's poll/main-thread
    /// affine). The worker HOLDS the queue while this is false, so narration pauses
    /// when you tab to a browser/other app and resumes when a terminal returns. Init
    /// true: fail-open, never silence before the first sample.
    terminal_front: AtomicBool,
    /// Latches true the first time a terminal IS seen frontmost. The focus gate only
    /// engages once this is set — so a user whose terminal emulator isn't in
    /// `TERM_BUNDLES` (frontmost always reads false) is NEVER silenced; they degrade
    /// to today's always-play instead of going mute.
    terminal_seen: AtomicBool,
    /// Config `pause_in_background`: when true, the frontmost focus gate HOLDS the queue
    /// while no terminal is frontmost; when false it's disabled (speech plays regardless of
    /// which app is frontmost). Published by the engine poll thread each tick. Init false =
    /// the shipped default (keep speaking); the first poll tick applies the live config.
    pause_in_background: AtomicBool,
    /// Transient PER-SESSION voice overrides, keyed by Claude session id (the
    /// empty string is the default/global session). When a reply's session has an
    /// entry, the queue uses it instead of the configured voice, until cleared or
    /// engine restart. NOT persisted to settings.json.
    session_voice: Mutex<HashMap<String, (ds_config::TtsEngine, String)>>,
    /// AUTO voice assignments from the preferred-voices pool, keyed by Claude session id.
    /// Distinct from `session_voice` (explicit `set_voice` overrides): this is filled
    /// lazily — the first reply from a new session claims the next untaken pool voice, so
    /// each terminal speaks with a different voice. In-memory; cleared on engine restart.
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
    playing_session: Mutex<Option<String>>,
    tts: Arc<TtsManager>,
    paths: Paths,
    /// The shared status-push gate: a `tts_active` transition bumps it so a blocked
    /// `WaitModelStatus` sees playback start/stop immediately (the flag drives the
    /// menu-bar TTS dot in `model_status`). Routed through [`set_tts_active`].
    gate: Arc<StatusGate>,
    /// When a VOICE submit (Caps dictation / hands-free) last pressed Enter. The
    /// UserPromptSubmit hook fires for EVERY submit, so `MarkActive` consumes this to tell
    /// a voice submit's own auto-Enter apart from a real keyboard submit (the `keyboard`
    /// drop must not fire on a voice submit). See `note_voice_submit` / `take_recent_voice_submit`.
    last_voice_submit: Mutex<Option<Instant>>,
    /// Shared read handle to the single mic-in-use watcher (CoreAudio listener on macOS, poll
    /// thread elsewhere). The worker's focus-hold reads this CACHED state instead of querying
    /// the audio device every 120 ms while holding an item.
    mic: ds_platform::MicState,
}

impl TtsQueue {
    /// Create the queue and spawn its worker thread.
    pub fn start(
        tts: Arc<TtsManager>,
        paths: Paths,
        gate: Arc<StatusGate>,
        mic: ds_platform::MicState,
    ) -> Arc<Self> {
        let q = Arc::new(Self {
            items: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            generation: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            tts_active: AtomicBool::new(false),
            terminal_front: AtomicBool::new(true),
            terminal_seen: AtomicBool::new(false),
            pause_in_background: AtomicBool::new(false),
            session_voice: Mutex::new(HashMap::new()),
            pool_assignments: Mutex::new(HashMap::new()),
            active: Mutex::new(ActiveSel::default()),
            last_voice_submit: Mutex::new(None),
            playing_session: Mutex::new(None),
            tts,
            paths,
            gate,
            mic,
        });
        let worker = q.clone();
        std::thread::Builder::new()
            .name("ds-ttsq".into())
            .spawn(move || worker.run())
            .ok();
        q
    }

    /// Enqueue the final reply (survives a record-barge in resume mode).
    /// Enqueue one unit of speech onto the FIFO. Empty text is ignored. There is no cap
    /// and no kind: callers (explicit `speak`, the greeting, and mid-turn narration) all
    /// land here and are played in order. `voice`/`rate` are optional per-call overrides
    /// (narration passes `None` for both → the session/config voice at play time).
    pub fn enqueue(
        &self,
        text: String,
        voice: Option<String>,
        rate: Option<f32>,
        session: Option<String>,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let mut q = self.items.lock().unwrap();
        self.note_recent(&session);
        q.push_back(Item {
            text,
            voice,
            rate,
            session,
        });
        self.cv.notify_one();
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

    /// Global hard barge (caps long-press reset / `StopSpeech{None}`): drop everything
    /// pending, cancel whatever is playing, and clear any pause. The audio is faded out
    /// over the short window (not an instant cut) so even this "stop everything" gesture
    /// tapers instead of clicking.
    pub fn clear(&self) {
        self.items.lock().unwrap().clear();
        self.paused.store(false, Ordering::SeqCst);
        // Playback is being cancelled now; reflect it immediately so a `Status`
        // probe right after a barge doesn't report stale `tts_active=true` during
        // the worker's unwind. The worker only ever sets this true at dequeue.
        self.set_tts_active(false);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.tts.stop_fade();
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
        self.generation.fetch_add(1, Ordering::SeqCst);
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
        // Prune this window's pending items; leave other sessions' (and untagged
        // global) items in place. Lock released at the end of the statement.
        prune_session(&mut self.items.lock().unwrap(), &session);
        // Cancel the current item only when it's this window's. `tts_active` gates
        // "something is playing"; `playing_session` names whose it is. A generation
        // bump cancels exactly the one in-flight item (single worker, one warm child),
        // so gating it on the match leaves another window's playback alone. This path
        // serves the per-window StopSpeech (window close), which may target a window
        // that is NOT the one currently playing — hence the gate. (The submit-drop uses
        // [`clear_active_session`], which cancels unconditionally.)
        let cancel_current = self.tts_active.load(Ordering::SeqCst)
            && *self.playing_session.lock().unwrap() == session;
        if cancel_current {
            self.set_tts_active(false);
            self.generation.fetch_add(1, Ordering::SeqCst);
            // Per-window barge: fade the in-flight item out (short window) so a clear-on-
            // submit / window close / newest-reply preempt tapers off instead of clicking.
            // Every user-facing barge fades now (global + record-barge included); only the
            // helper's internal block-to-block preempt stays an instant cut.
            self.tts.stop_fade();
        }
        // Wake the worker so a held item for this (now-pruned) session re-evaluates,
        // and so the active terminal's next item starts promptly after a cancel.
        self.cv.notify_one();
    }

    /// Clear the CURRENTLY-ACTIVE window's speech — the voice-submit (`drop_speech_on`)
    /// barge from the Caps confirm-paste and the hands-free submit word. The submitting
    /// terminal is the active window; it targets the active selection (the explicit
    /// prompt-target, else the most-recent producer). A no-op when no terminal is active
    /// (`effective()` is `None`) so a dictation submit never drops untagged global audio
    /// (e.g. the MCP `speak` tool).
    ///
    /// Unlike [`clear_session`](Self::clear_session), the cancel here is UNCONDITIONAL —
    /// NOT gated on the `tts_active`/`playing_session` snapshot. The worker only ever
    /// plays the active session's (or untagged) audio, so the active session IS what the
    /// helper is emitting; gating on the flags let a submit that landed in a record-barge
    /// transition (flags briefly stale) leak several blocks before stopping. Bumping the
    /// generation + stopping the helper directly makes the drop reliable regardless of
    /// timing (the same unconditional shape `pause_for_record` and `clear` already use).
    pub fn clear_active_session(&self) {
        let active = self.active.lock().unwrap().effective(); // lock released here
        let Some(active) = active else {
            return; // no active terminal → nothing to drop (never touch global audio)
        };
        // Prune the active window's queued items (other windows + untagged global stay).
        prune_session(&mut self.items.lock().unwrap(), &Some(active));
        // Cancel the in-flight item unconditionally (see the doc above): reflect the stop
        // immediately, abandon the worker's current item via a generation bump, and fade
        // the helper out. When NOT paused the worker drops the item (no requeue); when in
        // a record-barge pause the item was already pruned above, so it can't resume.
        self.set_tts_active(false);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.tts.stop_fade();
        self.cv.notify_one();
    }

    /// Mark that a VOICE submit just pressed Enter (Caps dictation / hands-free). Called on
    /// every voice submit regardless of `drop_speech_on`, so the keyboard-drop path can
    /// de-dup the voice submit's own auto-Enter from a real keyboard submit.
    pub fn note_voice_submit(&self) {
        *self.last_voice_submit.lock().unwrap() = Some(Instant::now());
    }

    /// Consume the voice-submit mark: true iff a voice submit happened in the last ~3s — i.e.
    /// the UserPromptSubmit hook now firing is that voice submit's echo, NOT a keyboard submit.
    pub fn take_recent_voice_submit(&self) -> bool {
        let mut g = self.last_voice_submit.lock().unwrap();
        let recent = voice_submit_recent(*g, Instant::now());
        if recent {
            *g = None;
        }
        recent
    }

    /// Record barge (mic active): pause the worker and cancel the current item,
    /// keeping the ENTIRE queue (narration and reply). The worker re-enqueues the
    /// interrupted item on its generation bump, so `resume()` continues the whole
    /// queue from where the mic interrupted it.
    pub fn pause_for_record(&self) {
        self.paused.store(true, Ordering::SeqCst);
        // Nothing is audibly playing while paused for the record-barge; the kept
        // reply resumes on `resume()` (which re-enters the worker and re-sets it).
        self.set_tts_active(false);
        self.generation.fetch_add(1, Ordering::SeqCst);
        // Fade out (short) rather than hard-cut when you press caps to dictate, so the
        // voice tapers as recording starts. ~60 ms keeps mic bleed minimal in half-duplex
        // (full-duplex stands this watcher down entirely, so it never reaches here).
        self.tts.stop_fade();
    }

    /// Whether the warm child is running in full-duplex AEC mode (delegates to the
    /// `TtsManager`). The mic-barge watcher reads it to stand down: in full-duplex
    /// the input device is always live (VPIO), so the `mic_active()` edge is useless,
    /// and the user cancels the voice via the Caps long-press instead.
    pub fn is_full_duplex(&self) -> bool {
        self.tts.full_duplex_active()
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

    /// Mic freed: lift the pause so the worker resumes the kept/interrupted reply.
    /// No-op when not paused.
    pub fn resume(&self) {
        if self.paused.swap(false, Ordering::SeqCst) {
            self.cv.notify_one();
        }
    }

    /// Read-only playback snapshot: `(tts_active, queued, paused, session_voice)`.
    /// `queued` counts items still waiting in the deque (excludes the one being
    /// played); `session_voice` is `"<engine>:<voice>"` when an override is set.
    pub fn snapshot(&self) -> (bool, usize, bool, Option<String>) {
        let queued = self.items.lock().unwrap().len();
        // Report the default/global-session override for the status line (the
        // full per-session map isn't representable in one Option<String>).
        let session_voice = self
            .session_voice
            .lock()
            .unwrap()
            .get("")
            .map(|(e, v)| format!("{}:{}", e.brand(), v));
        (
            self.tts_active.load(Ordering::SeqCst),
            queued,
            self.paused.load(Ordering::SeqCst),
            session_voice,
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

    /// Whether the queue is active (TTS playing) OR has anything pending — the half-duplex
    /// play-gate for always-listening: the listener closes the mic whenever this
    /// is true, which (by freeing the mic) also lets the queue's mic-gate proceed.
    pub fn is_busy(&self) -> bool {
        self.tts_active.load(Ordering::SeqCst) || !self.items.lock().unwrap().is_empty()
    }

    /// Whether the warm helper's Parakeet (STT) model is resident + warm — the dictation
    /// start-guard reads this through the queue (it owns the `TtsManager`).
    pub fn stt_loaded(&self) -> bool {
        self.tts.stt_loaded()
    }

    /// Set the transient voice override for `session` (engine + voice). Takes
    /// effect on that session's next reply; not persisted.
    pub fn set_session_voice(
        &self,
        session: Option<String>,
        engine: ds_config::TtsEngine,
        voice: String,
    ) {
        self.session_voice
            .lock()
            .unwrap()
            .insert(vkey(&session), (engine, voice));
    }

    /// Clear `session`'s voice override → that session falls back to the
    /// configured voice.
    pub fn clear_session_voice(&self, session: Option<String>) {
        self.session_voice.lock().unwrap().remove(&vkey(&session));
    }

    /// SessionEnd (window closed for good): per-window barge like [`clear_session`], then
    /// FORGET this session's transient voice state — its preferred-pool assignment and any
    /// `set_voice` override — so `pool_assignments` / `session_voice` don't accumulate one
    /// entry per distinct session for the daemon's lifetime (they were previously only
    /// reclaimed on engine restart). Called with `Some` session; the `None`/global case
    /// routes to [`clear`](Self::clear) at the IPC site (nothing session-scoped to forget).
    pub fn end_session(&self, session: Option<String>) {
        self.clear_session(session.clone());
        if let Some(s) = &session {
            self.pool_assignments.lock().unwrap().remove(s);
        }
        self.session_voice.lock().unwrap().remove(&vkey(&session));
    }

    /// Get-or-assign this session's voice from the preferred pool (delegates to
    /// [`pick_pool_voice`]), recording the pick so it's stable per session. `pool`
    /// must be non-empty (caller checks).
    fn assign_pool_voice(&self, session: &str, pool: &[String]) -> String {
        let mut map = self.pool_assignments.lock().unwrap();
        let voice = pick_pool_voice(&map, pool, session);
        map.insert(session.to_string(), voice.clone());
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
            // Off never resolves here (a resolved rung is usable); kept for exhaustiveness.
            ds_config::TtsEngine::Off => return None,
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
        let cfg = VoiceConfig::load(&self.paths);
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
        self.enqueue(text, Some(voice), None, session);
    }

    /// Whether the worker must HOLD the dequeued item now (delay playback, dropping
    /// nothing) rather than play it. Reads the live gates; the rule is the pure
    /// [`should_hold`].
    fn worker_should_hold(&self) -> bool {
        should_hold(
            self.tts.full_duplex_active(),
            self.mic.is_active(),
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
            let item = {
                let mut q = self.items.lock().unwrap();
                loop {
                    if !self.paused.load(Ordering::SeqCst) {
                        let active = self.active.lock().unwrap().effective();
                        if let Some(pos) = select_pos(&q, &active) {
                            break q.remove(pos).expect("select_pos returns a valid index");
                        }
                    }
                    q = self.cv.wait(q).unwrap();
                }
            };
            let gen0 = self.generation.load(Ordering::SeqCst);

            // HOLD this item (any kind) while we must stay silent — resume when the
            // gate clears, dropping nothing. Two independent "hold, don't drop" gates:
            //   * mic live (HALF-DUPLEX only): never speak into a recording. Full-duplex
            //     skips this — the VPIO mic is always live (`mic_active()` permanently
            //     true), so the AEC lets us speak into the open mic and `BARGE` handles
            //     overlap.
            //   * no terminal frontmost: you've tabbed to a browser/other app — the
            //     cross-platform focus gate (applies in BOTH duplex modes). The worker
            //     reads the flag the poll thread publishes (NSWorkspace is poll/main-
            //     thread affine). Self-arming via `terminal_seen`, so an unrecognized
            //     terminal never goes mute. DISABLED when `pause_in_background` is false
            //     (config) — then speech plays regardless of which app is frontmost.
            // A generation bump (a pause edge or a hard StopSpeech/clear) breaks the
            // wait so it never sticks.
            while self.generation.load(Ordering::SeqCst) == gen0 && self.worker_should_hold() {
                std::thread::sleep(Duration::from_millis(120));
            }
            if self.generation.load(Ordering::SeqCst) != gen0 {
                self.requeue_if_resuming(item, gen0);
                continue;
            }

            let cfg = VoiceConfig::load(&self.paths);
            // Engine + base voice: a transient session override wins over config;
            // a per-call `item.voice` (e.g. the MCP `speak` voice arg) then
            // overrides just the voice string within the chosen engine.
            let (engine, base_voice) = match self
                .session_voice
                .lock()
                .unwrap()
                .get(&vkey(&item.session))
                .cloned()
            {
                Some((e, v)) => (e, v),
                // No explicit override: resolve via the SAME shared helper the greeting uses —
                // System reads `tts_system_voice`; Kokoro claims this terminal's pool voice (the
                // global/empty session and an empty pool fall back to `current_voice()`). Off /
                // no usable rung ⇒ a blank voice (speak_one no-ops, value unused).
                None => self
                    .resolve_engine_voice(&cfg, &item.session)
                    .unwrap_or((ds_config::TtsEngine::Off, String::new())),
            };
            let voice = item.voice.clone().unwrap_or(base_voice);
            let rate = item.rate.unwrap_or(cfg.tts_rate);

            // GUARD: never play if the selected engine's model isn't ready — a not-yet-warm
            // Kokoro would synth silence/garbage while it downloads/loads. Drop this item
            // (logged); the caller can speak again once the dot goes green. (System needs no
            // model; Off is handled in `speak_one`.)
            if !crate::config_gate::tts_can_play(engine, self.tts.tts_loaded()) {
                crate::logging::log(&format!(
                    "TTS not ready (engine={engine:?}, tts_loaded={}) — dropping queued speak",
                    self.tts.tts_loaded()
                ));
                continue;
            }

            self.set_tts_active(true);
            // Publish whose item is on air so a per-window `clear_session` can tell
            // its own playback from another terminal's. Mirrors `tts_active`.
            *self.playing_session.lock().unwrap() = item.session.clone();
            // Speak the whole block in ONE call (the warm child pipelines synth with
            // playback gaplessly) — uniformly for NARRATION and REPLY. If a record-barge
            // pause (generation bump) interrupts playback mid-way, re-enqueue the item so
            // `resume()` continues it. This is the SAME for both kinds: the old per-kind
            // split re-enqueued only replies, so an interrupted NARRATION was dropped —
            // tap-to-pause then tap-to-resume came back SILENT.
            self.speak_one(engine, &item.text, &voice, rate);
            if self.generation.load(Ordering::SeqCst) != gen0 {
                self.requeue_if_resuming(item, gen0);
            }
            self.set_tts_active(false);
            *self.playing_session.lock().unwrap() = None;
        }
    }

    /// Play one chunk on the warm child using the resolved `engine` (config or a
    /// session override) and `voice`.
    fn speak_one(&self, engine: ds_config::TtsEngine, text: &str, voice: &str, rate: f32) {
        let _ = match engine {
            // TTS off: never speak (defensive — items shouldn't be enqueued when off).
            ds_config::TtsEngine::Off => return,
            ds_config::TtsEngine::System => self.tts.speak_system(text, voice, rate),
            ds_config::TtsEngine::Kokoro => {
                self.tts.ensure_started();
                self.tts.speak(text, voice, rate)
            }
        };
    }

    /// On a cancel: if we were PAUSED for a record-barge, re-enqueue the interrupted
    /// item (narration OR reply) at the front so the worker resumes the whole queue
    /// from there. A block is one synth unit (the warm child streams it gaplessly), so
    /// we re-speak it from the top rather than from a sentence offset. A hard clear
    /// (paused == false) re-enqueues nothing — it dropped the queue on purpose.
    fn requeue_if_resuming(&self, item: Item, _gen0: u64) {
        if !should_requeue(self.paused.load(Ordering::SeqCst), &item.text) {
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

/// Whether an interrupted item (narration OR reply) should be RE-ENQUEUED to resume later.
/// Only when we were PAUSED for a record-barge (resume mode) — a hard clear/StopSpeech
/// leaves `paused == false` and re-enqueues nothing (it dropped on purpose). Empty text is
/// never requeued. Pure, so the "resume keeps the item, clear drops it" rule is unit-tested.
fn should_requeue(paused: bool, text: &str) -> bool {
    paused && !text.trim().is_empty()
}

/// Whether the worker should HOLD (delay, drop nothing) the dequeued item rather than
/// play it now. Two independent "hold, don't drop" gates, OR-ed together:
///
/// - MIC LIVE (half-duplex only): never speak into a recording. Full-duplex skips this
///   — the VPIO mic is always live, so the AEC + `BARGE` handle overlap instead.
/// - FOCUS (both modes, only when `pause_in_background`): no terminal frontmost (you
///   tabbed to a browser) → hold. Self-arming via `terminal_seen`, so an unrecognized
///   terminal emulator (never seen frontmost) degrades to always-play, never mute.
///
/// PURE — the worker re-evaluates it each tick while holding.
fn should_hold(
    full_duplex: bool,
    mic_active: bool,
    pause_in_background: bool,
    terminal_seen: bool,
    terminal_front: bool,
) -> bool {
    (!full_duplex && mic_active) || (pause_in_background && terminal_seen && !terminal_front)
}

/// How recently a voice submit must have happened for the next UserPromptSubmit hook to be
/// its echo (rather than a real keyboard submit). The hook fires sub-second after the voice
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
        assert!(should_hold(false, true, false, false, false), "half-duplex + mic → hold");
        assert!(!should_hold(true, true, false, false, false), "full-duplex ignores mic");
        // FOCUS gate: only when pause_in_background AND a terminal has been seen AND none
        // is frontmost. Self-arming: unseen terminal never holds (degrade to always-play).
        assert!(should_hold(false, false, true, true, false), "bg pause + seen + not front → hold");
        assert!(!should_hold(false, false, true, false, false), "never-seen terminal → play");
        assert!(!should_hold(false, false, true, true, true), "terminal frontmost → play");
        assert!(!should_hold(false, false, false, true, false), "pause_in_background off → play");
        // Nothing gating → play.
        assert!(!should_hold(false, false, false, false, false));
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
        assert!(voice_submit_recent(Some(base), base + Duration::from_secs(2)));
        // Past the window (4s) → a genuine keyboard submit, not the echo.
        assert!(!voice_submit_recent(Some(base), base + Duration::from_secs(4)));
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
    fn first_session_matches_current_voice() {
        // The first terminal's pool voice == the default/current voice (pool[0]).
        let p = pool();
        assert_eq!(pick_pool_voice(&HashMap::new(), &p, "s1"), p[0]);
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
        assert_ne!(greeting_line(Some("Sarah"), 0), greeting_line(Some("Sarah"), 1));
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
}
