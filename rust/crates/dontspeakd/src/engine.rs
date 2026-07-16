//! The `Engine<P>` gesture state machine: the Caps-Lock "tap to dictate, hold to
//! cancel" loop, plus the shared dictation-preview buffer it drives. The states are
//! explicit enums: [`GestureState`] (idle / recording / deferred-submit armed),
//! [`PressState`] (the physical Caps press in flight), and [`FinalState`] (the
//! preview buffer's finalize lifecycle).

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{CancelSpeechScope, VoiceConfig};
use ds_platform::Platform;
use ds_stt::Stt;

use crate::config_gate::{
    build_stt, caps_loop_enabled, full_duplex_wanted, helper_needed, helper_stt_provider,
    helper_uses_stt, helper_uses_tts, local_stt_available, normalize_long_press,
    reconcile_helper_models,
};
use crate::listener;
use crate::status::{CAPS_LOG_MAX, CapsEvent, CapsLog, StatusGate, now_ms};
use crate::tts::TtsManager;
use crate::ttsq::TtsQueue;

/// Double-tap window. Playing: skip message. Stop-dictation: flip paste-vs-submit
/// (`double_tap_submits`). Never armed from silence (zero-latency start).
const DOUBLE_TAP_MS: u64 = 280;

/// Finalize lifecycle for the dictation-preview buffer (armed flag + landed text
/// as one value — old triple fields could diverge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FinalState {
    /// Live partial only (not confirm mode).
    Idle,
    /// Stop tap fired; final may still be landing. Keeps panel up (anti-flicker);
    /// [`dictation_preview`] falls back to still-fresh `partial`.
    Armed,
    /// Non-empty final awaiting confirm paste.
    Ready(String),
    /// Empty final — tick disarms without pasting; still "awaiting" until disarm.
    Empty,
}

/// Engine ↔ confirm-panel channel. `HelperStt` writes partials/finals; engine pastes
/// on confirm, clears on long-press cancel. Via `model_status.dictation`.
/// Manual `Default` so `has_paste_target` starts true (fail-open).
pub(crate) struct PasteBuf {
    /// Live PARTIAL mirror (detached helper thread). Not in [`FinalState`] — coexists
    /// with every finalize state.
    pub partial: String,
    /// See [`FinalState`]. Engine: [`arm`]/[`disarm`]; deposits: helper_stt/listener.
    pub final_state: FinalState,
    /// App focused when recording started.
    pub target: Option<String>,
    /// Editable field focused? Sampled each tick while panel up. Init true (fail-open).
    pub has_paste_target: bool,
    /// Caps physically held — suppress `Ready` while held (might become long-press cancel).
    pub caps_held: bool,
    /// Session counter: detached `stop` joiner deposits only if epoch still matches.
    pub epoch: u64,
    /// Refusal cue deadline after a Caps start the engine couldn't honor.
    pub refused_until: Option<Instant>,
}

impl PasteBuf {
    /// `Idle → Armed`. No-op on Ready/Empty (joiner may deposit between stop and arm).
    fn arm(&mut self) {
        if matches!(self.final_state, FinalState::Idle) {
            self.final_state = FinalState::Armed;
        }
    }

    /// `Armed|Empty → Idle`; leave Ready (call sites take text first). One enum =
    /// armed flag and text can't diverge (old mirror-stuck bug).
    fn disarm(&mut self) {
        if matches!(self.final_state, FinalState::Armed | FinalState::Empty) {
            self.final_state = FinalState::Idle;
        }
    }
}

impl Default for PasteBuf {
    fn default() -> Self {
        Self {
            partial: String::new(),
            final_state: FinalState::Idle,
            target: None,
            has_paste_target: true, // fail-open: no orange warning before the first probe
            caps_held: false,
            epoch: 0,
            refused_until: None,
        }
    }
}

/// Refusal-cue duration after an unhonored Caps start. See [`PasteBuf::refused_until`].
pub(crate) const DICTATION_REFUSAL_MS: u64 = 1500;

/// Refusal window live at `now` — shared by tick digest and `model_status`.
pub(crate) fn refusal_live(refused_until: Option<Instant>, now: Instant) -> bool {
    refused_until.is_some_and(|t| now < t)
}

/// Panel display: `(text, awaiting_confirm)`. Caps held suppresses Ready (anti-flicker);
/// Armed/Empty keep awaiting with partial. PURE.
pub(crate) fn dictation_preview(
    final_state: &FinalState,
    partial: &str,
    caps_held: bool,
) -> (String, bool) {
    if caps_held {
        return (partial.to_string(), false);
    }
    match final_state {
        FinalState::Ready(text) => (text.clone(), true),
        FinalState::Armed | FinalState::Empty => (partial.to_string(), true),
        FinalState::Idle => (partial.to_string(), false),
    }
}

/// Shared handle to the dictation-preview buffer (engine poll thread writes it,
/// the listen thread mirrors partials, the IPC thread reads it for status).
pub(crate) type PasteState = Arc<Mutex<PasteBuf>>;

/// Dictation gesture mode (was lockstep booleans/fields that every disarm reset).
/// Mutually exclusive by construction; confirm sub-state only on ConfirmArmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GestureState {
    Idle,
    /// Mic open; start tap barges TTS + live dot.
    Recording,
    /// Deferred submit armed; tick pastes when final lands. Flip window is time-based
    /// (not a second variant) — see `deferred_submit_held` / `apply_caps_edge`.
    ConfirmArmed {
        /// Flip double-tap anchor; `None` after second tap consumed it.
        stop_tap_at: Option<Instant>,
        /// Press began inside flip window → not a new recording.
        double_pending: bool,
        /// Enter after paste? Armed to `!double_tap_submits`; second tap toggles.
        enter_after_paste: bool,
    },
}

/// Physical Caps press (separate from [`GestureState`] — press occurs in every mode).
/// No LongPressResetting: `cancel_all` returns to idle; latch is `long_press_fired`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PressState {
    Up,
    /// Press since `since`; latch so cancel fires once and release ≠ tap.
    Down {
        since: Instant,
        long_press_fired: bool,
    },
}

/// Engine state + deps. `plat` is `Rc<P>` so boxed STT shares the same platform
/// (Stt is non-`Send`; single poll thread only).
pub(crate) struct Engine<P: Platform + 'static> {
    pub(crate) plat: Rc<P>,
    /// Caps edges route through this.
    pub(crate) stt: Box<dyn Stt>,
    pidfile: std::path::PathBuf,

    /// See [`GestureState`]. `voice_paused`/`caps_phys_prev`/`pending_tap_at` stay
    /// plain fields (orthogonal to every mode). INVARIANT: every `self.gesture = …`
    /// is followed by `sync_caps_led` (or goes through a function that already does).
    gesture: GestureState,
    /// Caps pause/resume when dictation is OFF (same gesture as dictation path).
    voice_paused: bool,
    /// Last physical Caps sample — edge detector; LED is pure output, never read back.
    caps_phys_prev: bool,

    /// Physical hold ≥ this (ms) → force idle, LED off.
    long_press_ms: u64,
    /// See [`PressState`].
    press: PressState,
    /// Deferred tap while speaking (double-tap skip). Coexists with every gesture mode.
    pending_tap_at: Option<Instant>,
    /// Deferred Enter after paste (`paste_submit_delay_ms`); polled via `deferred_ready`.
    pending_enter_at: Option<Instant>,

    /// Last applied config for surgical [`Engine::reload`] diffs.
    pub(crate) cfg: VoiceConfig,
    /// Caps loop live; false ⇒ `tick` no-op.
    pub(crate) caps_enabled: bool,
    /// Warm helper; barge-in / tts toggle. `None` in tests.
    pub(crate) tts: Option<Arc<TtsManager>>,
    /// TTS queue; start-tap barge-in. `None` in tests.
    pub(crate) ttsq: Option<Arc<TtsQueue>>,
    /// Effective caps (config && AX) for status. `None` in tests.
    pub(crate) caps_active: Option<Arc<AtomicBool>>,
    /// Live recording for status. `None` in tests.
    pub(crate) stt_active: Option<Arc<AtomicBool>>,
    /// Recent caps events for Settings. `None` in tests.
    pub(crate) caps_log: Option<CapsLog>,
    /// Hands-free runtime when `listen_mode == Always`.
    pub(crate) listener: Option<listener::Listener<P>>,

    /// Dictation preview buffer. Arming is in `gesture`; mirror in `final_state`.
    pub(crate) paste: PasteState,

    /// Push gate for WaitModelStatus; same Arc as IPC EngineShared.
    pub(crate) status_gate: Option<Arc<StatusGate>>,
    /// Last published overlay digest (bump only on change).
    dict_digest: u64,
    /// Local helper STT vs factory fallback — reload rebuilds when this flips.
    pub(crate) stt_is_local: bool,
    /// Injected so tests never hit real `$HOME` for Claude Code keybindings.
    pub(crate) paths: Option<ds_config::Paths>,
}

impl<P: Platform + 'static> Engine<P> {
    /// Construct with the default ClaudeNative STT engine (used by the
    /// §F tests and as the fallback). `main` uses [`Engine::with_config`] to
    /// honor the configured engine via the `ds-engines` factory.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(plat: P, pidfile: std::path::PathBuf, long_press_ms: u64) -> Self {
        let plat = Rc::new(plat);
        // Default Space chord — this constructor is the §F test / fallback path; the real
        // engine reads Claude Code's bound key via the ds-engines factory (with_config).
        let stt: Box<dyn Stt> = Box::new(ds_stt::ClaudeNative::new(
            plat.clone(),
            ds_platform::KeyChord::default(),
        ));
        Self::assemble(
            plat,
            stt,
            VoiceConfig::default(),
            pidfile,
            long_press_ms,
            None,
        )
    }

    /// Construct selecting the STT engine from config via the factory
    /// (degrade-to-default-never-silent, §A.3).
    pub(crate) fn with_config(
        plat: P,
        cfg: &VoiceConfig,
        pidfile: std::path::PathBuf,
        long_press_ms: u64,
        paths: Option<&ds_config::Paths>,
    ) -> Self {
        let plat = Rc::new(plat);
        let stt = ds_engines::make_stt_at(cfg, plat.clone(), &ds_engines::RealAvailability, paths);
        Self::assemble(
            plat,
            stt,
            cfg.clone(),
            pidfile,
            long_press_ms,
            paths.cloned(),
        )
    }

    fn assemble(
        plat: Rc<P>,
        stt: Box<dyn Stt>,
        cfg: VoiceConfig,
        pidfile: std::path::PathBuf,
        long_press_ms: u64,
        paths: Option<ds_config::Paths>,
    ) -> Self {
        // Caps dictation needs Accessibility trust AND the config toggle(s).
        let caps_enabled = caps_loop_enabled(&cfg) && plat.preflight().is_ok();
        let mut engine = Self {
            plat,
            stt,
            pidfile,
            gesture: GestureState::Idle,
            voice_paused: false,
            caps_phys_prev: false,
            long_press_ms,
            press: PressState::Up,
            pending_tap_at: None,
            pending_enter_at: None,
            cfg,
            // Starts `false` regardless of the computed `caps_enabled` — the call to
            // `set_caps_gate` below is the ONE place that decides whether to acquire the
            // physical Caps key, so "acquire only when starting enabled" is expressed
            // once, not re-derived here too (and here, again, on every future edit to
            // that rule). Safe post-construction: every OTHER field `set_caps_gate`
            // touches (`gesture`, `caps_active`, `status_gate`) is still
            // at its just-initialized default right below, so the call is a pure
            // acquire-or-not with no side effect on them.
            caps_enabled: false,
            tts: None,
            ttsq: None,
            caps_active: None,
            stt_active: None,
            caps_log: None,
            listener: None,
            paste: Arc::new(Mutex::new(PasteBuf::default())),
            status_gate: None,
            dict_digest: 0,
            // `assemble`'s `stt` is always the ClaudeNative/factory engine (the test + fallback
            // constructors); the local helper is only ever installed by `build_stt` in
            // `engine_run`/`reload`, which set this flag alongside.
            stt_is_local: false,
            paths,
        };
        // Push the user's extra paste-target identifiers to the freshly-constructed platform
        // (config.toml `extra_terminals`/`extra_custom_text_editors`, ADDED TO — never
        // replacing — the compiled-in KNOWN_TERMINALS/CUSTOM_TEXT_EXES tables at lookup
        // time). `reload` below refreshes this on every config.toml change too.
        engine
            .plat
            .set_extra_terminals(engine.cfg.extra_terminals.clone());
        engine
            .plat
            .set_extra_custom_text_editors(engine.cfg.extra_custom_text_editors.clone());
        engine.set_caps_gate(caps_enabled);
        engine
    }

    /// Whether dictation is actively recording ([`GestureState::Recording`]).
    fn is_recording(&self) -> bool {
        matches!(self.gesture, GestureState::Recording)
    }

    /// Re-drive the physical Caps LED to match the current gesture
    /// (`is_recording()`). Call this as the LAST statement of any function that
    /// assigns `self.gesture` — it is the ONE place in this file that writes
    /// `Platform::set_caps_lock`, so "did I forget to sync the LED after changing
    /// gesture" reduces to "did I call this line". (`check_long_press`'s per-tick
    /// call is the one exception that isn't about a gesture change — see its doc.)
    fn sync_caps_led(&self) {
        self.plat.set_caps_lock(self.is_recording());
    }

    /// Whether the stop tap has armed the deferred submit
    /// ([`GestureState::ConfirmArmed`]) — the final may or may not have landed yet.
    fn is_confirm_armed(&self) -> bool {
        matches!(self.gesture, GestureState::ConfirmArmed { .. })
    }

    /// Bump the status push gate when the dictation-overlay PREVIEW changes, so a blocked
    /// `WaitModelStatus` (the app's overlay push thread) wakes immediately. Digests the
    /// preview fields that change WITHOUT a recording toggle (live/final text, awaiting
    /// confirm, paste target) — the `recording`/`stt_active` flag itself is pushed at its
    /// flip site by [`set_stt_active`], so re-digesting it here would only double-bump.
    /// Skips the bump when unchanged so an idle engine never wakes waiters every tick.
    /// No-op in tests (`status_gate` is `None`).
    fn publish_status_change(&mut self) {
        use std::hash::{Hash, Hasher};
        let Some(gate) = self.status_gate.clone() else {
            return;
        };
        let (text, awaiting, has_target, refused) = self
            .paste
            .lock()
            .map(|p| {
                let (t, a) = dictation_preview(&p.final_state, &p.partial, p.caps_held);
                // LIVE refusal state folded into the digest: arming it (a refused Caps
                // tap) AND its time-based expiry both change the hash, so the overlay's
                // pop-up and its fade-out each get a push within one tick — no separate
                // expiry bookkeeping.
                (
                    t,
                    a,
                    p.has_paste_target,
                    refusal_live(p.refused_until, Instant::now()),
                )
            })
            .unwrap_or((String::new(), false, true, false));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        awaiting.hash(&mut h);
        has_target.hash(&mut h);
        refused.hash(&mut h);
        let digest = h.finish();
        if digest != self.dict_digest {
            self.dict_digest = digest;
            gate.bump();
        }
    }

    /// Append a caps-trigger event to the shared log (newest last, bounded) so the
    /// app can show it via `model_status`. No-op in tests (the field is `None`).
    fn record_caps(&self, kind: &'static str) {
        if let Some(log) = &self.caps_log
            && let Ok(mut q) = log.lock()
        {
            q.push_back(CapsEvent {
                ts_ms: now_ms(),
                kind,
            });
            while q.len() > CAPS_LOG_MAX {
                q.pop_front();
            }
        }
    }

    /// Publish the live recording state for the app's caps status dot. On a real
    /// transition, bump the status-push gate so a blocked `WaitModelStatus` (the app's
    /// overlay push thread) sees recording start/stop immediately — the authoritative
    /// `stt_active` push for the engine PTT path (so `publish_status_change` need not
    /// re-digest `recording`). No-op bump in tests (`status_gate` is `None`).
    fn set_stt_active(&self, on: bool) {
        if let Some(r) = &self.stt_active
            && r.swap(on, Ordering::Relaxed) != on
            && let Some(gate) = &self.status_gate
        {
            gate.bump();
        }
    }

    /// Tear down an IN-FLIGHT dictation for an INTERNAL reason (a reload swapping the STT
    /// engine, or the caps gate going off mid-hold) — NOT a user cancel, so it does NOT
    /// silence the voice or barge. Aborts the listen and FULLY resets the recording state:
    /// the gesture mode, the published `stt_active` (so the menu-bar icon doesn't stay
    /// "recording" with no actual listen), and the preview buffer. Idempotent when
    /// already idle. Callers that rebuild `self.stt` must invoke this BEFORE the swap (it
    /// aborts the CURRENT engine).
    fn teardown_hold(&mut self) {
        if self.is_recording() {
            self.stt.abort();
        }
        // One assignment ends the hold AND disarms any deferred submit — the confirm
        // sub-state lives inside `ConfirmArmed`, so it can't survive this reset.
        self.gesture = GestureState::Idle;
        // If this was actually recording, the LED must go dark to match — an INTERNAL
        // reason for ending dictation is still the dictation ending, and a caller who
        // silently swaps the STT engine (or bounces into/out of always-listen mode) out
        // from under an active hold must not leave the LED lit with no future edge left
        // to correct it (there may be no physical release coming at all, e.g. the key
        // was already up when the reload landed).
        self.sync_caps_led();
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.final_state = FinalState::Idle;
            p.target = None;
            // New session boundary: invalidate any in-flight `stop` joiner so its
            // late final can't repopulate this just-cleared buffer (the engine
            // hot-swap drops the old HelperStt, but its detached joiner survives).
            p.epoch = p.epoch.wrapping_add(1);
        }
        // Publish `stt_active = false` LAST: it bumps the status-push gate and can wake a
        // blocked status reader immediately, so the buffer must already be fully settled —
        // otherwise that reader could see recording=false with a stale armed/landed
        // `final_state` from the torn-down session (see `stop_recording`'s identical
        // ordering concern).
        self.set_stt_active(false);
    }

    /// Drop the deferred-submit latch: `ConfirmArmed → Idle`, with the whole
    /// insert-only double-tap sub-state (`stop_tap_at`/`double_pending`/
    /// `enter_after_paste`) dropped structurally — it lives inside the variant, so
    /// "every disarm site must reset all four" no longer depends on discipline.
    /// A NO-OP on `Recording` (`start_recording` calls this AFTER setting
    /// `Recording` — the fresh session must not be knocked back to `Idle`).
    ///
    /// Also disarms the [`PasteBuf::final_state`] MIRROR the bar's `awaiting_confirm`
    /// reads (see [`PasteBuf::disarm`], which keeps the war story of the one disarm
    /// site that forgot the mirror). Callers that need the buffer settled EARLIER
    /// than this — `confirm_paste` takes the text and drops to `Idle` atomically
    /// under one lock, before its synchronous paste/Enter syscalls, so a concurrent
    /// status read never sees a half-cleared confirm state mid-syscall — may still do
    /// so themselves; the disarm here is then just a harmless redundant no-op.
    fn disarm_confirm(&mut self) {
        if self.is_confirm_armed() {
            self.gesture = GestureState::Idle;
            self.sync_caps_led();
        }
        if let Ok(mut p) = self.paste.lock() {
            p.disarm();
        }
    }

    /// Whether a finalized transcript is waiting for the user's confirm tap (a landed
    /// `Ready`/`Empty` final, or the stop tap has armed the deferred submit and the
    /// final just hasn't landed yet ⇒ the confirm panel is up and the Caps key means
    /// confirm/cancel) — i.e. any non-`Idle` [`FinalState`]. Gates the live
    /// paste-target probe in `tick` — without the `Armed` state counting here too,
    /// that probe would skip the exact window `dictation_preview` keeps the panel
    /// visible through, leaving `has_paste_target` stale for the length of the async
    /// final.
    fn awaiting_confirm(&self) -> bool {
        self.paste
            .lock()
            .map(|p| !matches!(p.final_state, FinalState::Idle))
            .unwrap_or(false)
    }

    /// Whether the warm helper is running in full-duplex AEC coexist mode (dictation
    /// and TTS overlap). Drives the coexist gesture semantics: a dictation tap does
    /// not barge the voice, the stopping press auto-submits, and long-press meanings
    /// split by state. False in tests (`ttsq` is `None`) and half-duplex.
    fn is_full_duplex(&self) -> bool {
        self.ttsq
            .as_ref()
            .map(|q| q.is_full_duplex())
            .unwrap_or(false)
    }

    /// Submit the just-finalized dictation: paste the pending transcript into the focused
    /// text field — ANY app, the synthetic Cmd+V lands wherever the cursor is — then press
    /// Return whenever `enter_after_paste` is set (armed per `double_tap_submits` and the
    /// stop gesture's tap count — one of the two gestures always submits, the other always
    /// just inserts).
    /// Driven by the deferred-submit check once the stop tap's async final lands.
    /// `disarm_confirm()` below owns the LED sync (it's already off from the stop
    /// tap's own `stop_recording` call anyway; this just goes through the one
    /// shared path rather than re-deriving it here too).
    fn confirm_paste(&mut self) {
        // Capture the armed outcome ONCE at entry (it is read both before and after
        // `disarm_confirm()` below, which drops the variant that holds it). Only ever
        // called from the tick's ConfirmArmed arm; the `true` fallback matches the
        // field's disarmed reset default.
        let enter_after_paste = match self.gesture {
            GestureState::ConfirmArmed {
                enter_after_paste, ..
            } => enter_after_paste,
            _ => true,
        };
        // The confirm tap ALWAYS pastes — there's no focus refusal. The "is there a
        // paste target?" cue is a live glow on the panel (the engine samples
        // `has_paste_target` (the platform probe) each tick → `has_paste_target` (the
        // status field) → the app tints it orange when there's nowhere to land).
        let text = self.paste.lock().ok().and_then(|mut p| {
            p.partial.clear();
            p.target = None;
            // Take the text and drop to `Idle` atomically under this ONE lock — not
            // later via `disarm_confirm()`, which only runs after the syscalls below
            // (see its doc comment). A concurrent status read must never see a
            // half-cleared confirm state mid-syscall.
            match std::mem::replace(&mut p.final_state, FinalState::Idle) {
                FinalState::Ready(text) => Some(text),
                _ => None,
            }
        });
        if let Some(text) = text {
            // Paste into WHATEVER is focused (terminal, Notes, browser, chat, …). The
            // explicit confirm tap + the overlay's "→ <app>" target label are the
            // deliberate gate now, so the paste is no longer restricted to a terminal.
            self.plat.type_text(&text);
            // Exactly one of the two stop-tap gestures always submits (presses Return in
            // ANY focused app — terminal, chat box, search field, editor); the other always
            // just inserts the transcript and the user presses Return themselves. Which TAP
            // COUNT (single vs double) submits for this transcript is `enter_after_paste`,
            // armed per `double_tap_submits` and possibly flipped by a second tap on the
            // stop gesture (captured from the `ConfirmArmed` variant at entry above).
            let submit = enter_after_paste;
            if submit {
                if let Some(q) = &self.ttsq {
                    // Apply `input_clears` immediately at submit — must not wait on the
                    // deferred Enter below; a playing reply is silenced (or not)
                    // exactly when the user submits, not delayed by
                    // `paste_submit_delay_ms`. `current` drops this window's own
                    // now-stale pending speech, `other` drops every other window's (and
                    // untagged global audio). Resolve "active" ONCE and hand it to
                    // `cancel_for_submit` so the two scopes can't disagree (see its doc).
                    q.cancel_for_submit(
                        q.active_session(),
                        self.cfg.input_clears.contains(&CancelSpeechScope::Current),
                        self.cfg.input_clears.contains(&CancelSpeechScope::Other),
                    );
                }
                // Let the async paste settle before Enter lands — deferred via a polled
                // timer (see `pending_enter_at`) rather than a blocking sleep, since this
                // runs on the engine's single tick thread.
                if self.cfg.paste_submit_delay_ms > 0 {
                    self.pending_enter_at = Some(
                        Instant::now() + Duration::from_millis(self.cfg.paste_submit_delay_ms),
                    );
                } else {
                    self.press_deferred_enter();
                }
            }
        }
        let inserted_only = !enter_after_paste;
        self.disarm_confirm();
        self.record_caps("confirm");
        if inserted_only {
            log::debug!(
                target: "engine",
                "deferred submit — pasted pending transcript (insert only, no Enter), LED off"
            );
        } else {
            log::debug!(target: "engine", "deferred submit — pasted pending transcript + Enter, LED off");
        }
    }

    /// Caps HELD past the long-press threshold → the universal CANCEL: discard any
    /// in-flight dictation WITHOUT injecting a partial, SILENCE the current voice /
    /// generation (clear the warm queue + barge the cold speaker), and return to idle with
    /// the LED off. This is the "hold to shut it up" gesture — there is NO hold-to-dictate;
    /// a hold never leaves a recording running (the stuck-state bug the tap/hold split fixes).
    fn cancel_all(&mut self) {
        // Drop any deferred single tap — a long-press supersedes it (don't toggle later).
        self.pending_tap_at = None;
        // End any in-flight listen via ABORT: ClaudeNative releases Ctrl+G (nothing left
        // held); Parakeet/System DISCARD the capture WITHOUT injecting a partial transcript.
        if self.is_recording() {
            self.stt.abort();
        }
        // Silence the voice / cancel generation: clear the warm queue + barge the cold
        // one-shot speaker. (Unlike a normal stop, a hold does NOT resume — it means
        // "stop talking".)
        if let Some(q) = &self.ttsq {
            q.clear();
        }
        let _ = ds_proc::barge_in(&self.pidfile);
        // Reset to idle, LED off. `Idle` in one assignment ends the recording AND
        // disarms any deferred submit (the confirm sub-state lives inside
        // `ConfirmArmed`, so nothing of it can leak past this reset); `sync_caps_led`
        // derives the off write from that same assignment rather than a separate
        // hardcoded literal.
        self.gesture = GestureState::Idle;
        self.sync_caps_led();
        self.set_stt_active(false);
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.final_state = FinalState::Idle;
            p.target = None;
            // A hold means "stop everything" — that includes a still-live refusal cue.
            p.refused_until = None;
            // Invalidate any in-flight `stop` joiner (see PasteBuf::epoch) so a stale
            // final can't land after this cancel reset the buffer.
            p.epoch = p.epoch.wrapping_add(1);
        }
        self.pending_enter_at = None;
        self.record_caps("cancel");
        log::debug!(target: "engine", "HOLD cancel — dictation discarded, voice silenced, LED off, idle");
    }

    /// Whether dictation can START right now: the selected STT engine is on AND ready to
    /// transcribe. `BuiltIn` (Parakeet) needs its model resident + warm; `System` needs the OS
    /// recognizer ready (probed only when selected — the probe isn't free); `ClaudeCode`
    /// delegates so it's always ready; off (`None`) never. See
    /// [`crate::config_gate::stt_can_start`].
    fn stt_ready_to_dictate(&self) -> bool {
        use ds_config::SttEngine;
        // The RESOLVED STT engine (first usable rung); None = dictation off.
        let resolved = self.cfg.resolved_stt();
        // No warm-engine host (tests / pure-RPC): nothing to probe, so don't gate — keep the
        // plain on/off behavior. In production `ttsq` is always Some.
        let Some(q) = self.ttsq.as_ref() else {
            return resolved.is_some();
        };
        let system_ready = resolved == Some(SttEngine::System)
            && ds_stt::system_state() == ds_stt::SystemState::Ready;
        crate::config_gate::stt_can_start(resolved, q.stt_loaded(), system_ready)
    }

    /// A completed Caps TAP toggles dictation: start when idle, stop+submit when recording.
    /// `start_recording`/`stop_recording` each sync the LED themselves as they flip the
    /// gesture state, so the light reflects the real recording state whether this fires
    /// immediately (from a tap's release) or later (from a speaking-deferred tap) — see
    /// [`sync_caps_led`](Self::sync_caps_led)'s doc.
    fn toggle_dictation(&mut self) {
        // GUARD: dictation can START only if the selected STT engine is actually READY to
        // transcribe (BuiltIn/System model resident + warm; ClaudeCode delegates → always).
        // On-but-not-ready REFUSES the tap: the visual cue armed below is the whole
        // response, and the voice is left strictly alone — it must NOT borrow the OFF
        // mode's pause/resume, because nothing would resume the pause when the model
        // finishes loading (TTS went silent for the entire download window, issue #1).
        // While ALREADY recording, readiness is moot — the model was ready when the
        // recording started, and a tap must always still STOP it.
        let dictation_on = self.cfg.resolved_stt().is_some();
        let ready = if self.is_recording() {
            true
        } else {
            let ready = self.stt_ready_to_dictate();
            // A refused START with Parakeet SELECTED may be a warm child that CRASHED
            // post-READY — and a user who only dictates never queues the speak that
            // triggers the worker-side heal, so dictation would stay refused until an app
            // restart. Kick the non-blocking heal; this tap stays a pause/resume, the NEXT
            // finds the model warm. No-op for a child that's alive (still loading) or
            // whose start failed (see `warm_child_heal_action`).
            if !ready
                && self.cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
                && let Some(q) = &self.ttsq
            {
                q.heal_crashed_child();
            }
            // A refused START must not be a silent no-op (the fresh-install trap: model
            // still downloading, Caps does "nothing", the user restarts the app). Arm the
            // refusal cue — the overlay pops up washed in the no-target warning glow for
            // DICTATION_REFUSAL_MS on every platform (`dictation.refused`). Only for the
            // engines with a runtime readiness gate (BuiltIn/System); dictation OFF keeps
            // its documented silent pause/resume tap (deliberate, not an error).
            if !ready && crate::config_gate::refusal_cue_on_refused_start(self.cfg.resolved_stt()) {
                if let Ok(mut p) = self.paste.lock() {
                    p.refused_until =
                        Some(Instant::now() + Duration::from_millis(DICTATION_REFUSAL_MS));
                }
                self.record_caps("refused");
            }
            ready
        };
        match caps_tap_action(dictation_on, ready, self.is_recording(), self.voice_paused) {
            CapsTap::StartRecord => self.start_recording(), // opens mic; pauses the voice
            CapsTap::StopRecord => self.stop_recording(),   // stops+submits; resumes the voice
            // Dictation ON but the engine can't start yet (model still downloading /
            // loading): the refusal cue armed above is the whole response. Deliberately a
            // strict no-op for the voice — the old shared pause path silenced TTS for the
            // entire download window with nothing to resume it (issue #1).
            CapsTap::Refused => {}
            // Dictation OFF: the mic never opens, but the tap still pauses/resumes the
            // voice — the SAME gesture, so the voice is HELD (and any narration that
            // arrives stays QUEUED), never silenced/dropped.
            CapsTap::PauseVoice => {
                if let Some(q) = &self.ttsq {
                    q.pause_for_record();
                }
                self.voice_paused = true;
            }
            CapsTap::ResumeVoice => {
                if let Some(q) = &self.ttsq {
                    q.resume();
                }
                self.voice_paused = false;
            }
        }
    }

    /// §E.4 hot-reload: re-read VoiceConfig and REBUILD the boxed Stt via the
    /// factory, WITHOUT corrupting the running state machine.
    ///
    /// In-flight handling (mirrors `cancel_all`'s teardown, via the same
    /// `teardown_hold` helper):
    ///   * If a dictation is active, `abort()` the OUTGOING engine first —
    ///     ClaudeNative releases Ctrl+G cleanly (nothing left held); Parakeet
    ///     discards the in-flight capture without injecting (§F.1).
    ///   * Swap in a fresh engine built on the SAME platform `Rc` (one event
    ///     source — never two engines fighting over the keyboard).
    ///   * Reset the gesture state to `Idle` so the new engine starts idle.
    ///
    /// `teardown_hold` DOES sync the LED off when it actually ends an in-flight
    /// recording (an internal reason for ending dictation is still the dictation
    /// ending — the LED must not lie that it's still recording just because nothing
    /// physically released the key). This does NOT fabricate a spurious tap: `reload`
    /// leaves the physical key untouched, and tap/long-press detection is edge-based
    /// on the physical key only (never reads the LED/lock state back) — so driving
    /// the LED here has no effect on gesture detection, only on the indicator's
    /// accuracy.
    pub(crate) fn reload(&mut self, cfg: &VoiceConfig) {
        // Diff against the last-applied config and touch ONLY what changed — the
        // "no extra reloads" contract. Per-call params
        // (voice/rate/narrate/region/vocab) need no action: the next call reads
        // them fresh from `self.cfg`, which we update unconditionally below.
        let change = cfg.changes_since(&self.cfg);

        // long_press is a cheap scalar latch — refresh it every reload.
        self.long_press_ms = normalize_long_press(cfg.long_press_ms);

        // Same reasoning as long_press_ms above: cheap to refresh unconditionally, no diff
        // needed. See Engine::assemble for why this lives here too (first-boot coverage).
        self.plat.set_extra_terminals(cfg.extra_terminals.clone());
        self.plat
            .set_extra_custom_text_editors(cfg.extra_custom_text_editors.clone());

        // STT engine: rebuild when the engine SELECTION changed OR when local availability
        // FLIPPED. The latter is the fresh-install case: a model download makes Parakeet
        // present without changing `resolved_stt()` (still `built_in`), so `stt_changed` is
        // false — but `build_stt` would now pick the local helper instead of the ClaudeNative
        // fallback. Without this, dictation stays on the Claude Code tap even though Parakeet
        // downloaded, loaded, and the status dot went green. (The download-completion self-heal
        // nudges a reload precisely so this check re-runs.)
        // Mirror `build_stt`'s decision EXACTLY: it uses the local helper only when a warm
        // helper exists (`tts.is_some()`, always true in production, `None` in tests) AND the
        // model is available. Gating on `tts` keeps this hermetic (tests never touch the real
        // model cache) and correct (no helper ⇒ build_stt always falls to the factory anyway).
        let want_local = self.tts.is_some() && local_stt_available(cfg);
        // Captured BEFORE the rebuild below overwrites `stt_is_local` — the listener
        // lifecycle further down needs the same availability edge (see there).
        let local_avail_flipped = want_local != self.stt_is_local;
        if change.stt_changed || local_avail_flipped {
            // Reset the recording state (incl. the published `stt_active` icon) BEFORE the
            // swap — otherwise a reload mid-dictation leaves the menu-bar icon stuck
            // "recording" with no live listen on the fresh engine.
            self.teardown_hold();
            self.stt = build_stt(
                cfg,
                self.plat.clone(),
                self.tts.as_ref(),
                &self.paste,
                self.paths.as_ref(),
            );
            self.stt_is_local = want_local;
        }

        // Caps loop gate: recomputed EVERY reload (not just when the toggle
        // changed) so a freshly-granted Accessibility trust is picked up by a
        // reload nudge — no restart. If the loop just went OFF mid-hold, end the
        // HOLD cleanly so we never leave a key down or the mic open. Turning it
        // back ON needs no teardown — the next tick re-arms on the live key state.
        let now_on = caps_loop_enabled(cfg) && self.plat.preflight().is_ok();
        self.set_caps_gate(now_on);

        // Full-duplex AEC env for the warm helper (Parakeet STT + Kokoro TTS):
        // store the desired mode BEFORE any (re)start below so a fresh start uses it.
        if let Some(tts) = &self.tts {
            tts.set_full_duplex_pref(full_duplex_wanted(cfg));
            tts.set_stt_provider_pref(helper_stt_provider(cfg));
            tts.set_stt_wanted(helper_uses_stt(cfg));
            tts.set_tts_wanted(helper_uses_tts(cfg));
            // See the identical seeding + rationale in `boot::engine_run`: without this the
            // model-presence gate in `start_locked` would resolve ANE-vs-ONNX from a stale
            // provider preference on the FIRST `set_enabled` below whenever this reload flips
            // TTS on (`apply_provider_and_autofetch` only applies the resolved provider AFTER
            // `daemon.reload` returns, in `boot.rs`'s `ReloadTick::Run` arm).
            tts.set_provider(cfg.resolved_tts_provider().as_str());
        }

        // Warm helper lifecycle: it hosts BOTH engines now, so (re)gate it whenever
        // TTS or STT toggles/engine changes — run it iff either engine needs it.
        if (change.tts_toggled || change.stt_changed)
            && let Some(tts) = &self.tts
        {
            // Debug aid: this is the trigger for a warm-child bounce (set_enabled +
            // reconcile below can kill/respawn ds-helper). Logging WHICH flag fired
            // lets a burst of back-to-back reloads be tied to a cause from the log
            // alone, instead of pieced together from timestamps across several lines.
            log::info!(
                target: "engine",
                "warm helper lifecycle re-gated (tts_toggled={} stt_changed={})",
                change.tts_toggled, change.stt_changed
            );
            tts.set_enabled(helper_needed(cfg));
            // …then make the helper's resident models match the selection
            // (load the selected engine, free the deselected one).
            reconcile_helper_models(tts, cfg);
        }

        // If the helper stayed running but its full-duplex env is now stale (the
        // user toggled `full_duplex`, or switched STT to/from Parakeet without
        // stopping the helper), restart it to pick up the new DONTSPEAK_FULL_DUPLEX.
        if let Some(tts) = &self.tts {
            tts.restart_if_full_duplex_stale();
        }

        // Always-listening lifecycle: (re)build the listener when the mode turns
        // on or its params change; drop it when the mode turns off. Compared
        // against the still-current self.cfg (replaced just below).
        // `paste_submit_delay_ms` deliberately does NOT gate a rebuild: unlike
        // `submit_confirm_ms`/`endpoint_silence_ms`, which are baked into
        // `TurnLogic::new`/`Endpointer::new` at construction, it's just a plain `u64`
        // the listener reads at submit time — pushed live below instead, so changing
        // it mid-utterance doesn't drop an in-flight hands-free capture.
        let listen_changed = cfg.listen_mode != self.cfg.listen_mode
            || cfg.hands_free != self.cfg.hands_free
            || cfg.submit_confirm_ms != self.cfg.submit_confirm_ms
            || cfg.endpoint_silence_ms != self.cfg.endpoint_silence_ms;
        if cfg.listen_mode == ds_config::ListenMode::Always {
            // `local_avail_flipped`: the listener probes model presence ONCE at
            // construction (`Listener::new` → `available`, false ⇒ the loop no-ops), so
            // the fresh-install model download must REBUILD it too — the hands-free twin
            // of the `build_stt` self-heal above; without it always-listen stays inert
            // until an app restart even though the model arrived and the dot went green.
            // `change.stt_changed`: the listener also resolves its provider ONCE at
            // construction (`Listener::new` → `helper_stt_provider`), same as `build_stt`
            // does for tap-to-talk above (line ~946) — without mirroring that same trigger
            // here, switching STT engine/provider (e.g. ANE → CPU, or BuiltIn → System)
            // while Always-listening is already running leaves the background listener on
            // the STALE provider indefinitely, since `local_avail_flipped` alone stays
            // false whenever both the old and new provider are already locally available.
            if self.listener.is_none()
                || listen_changed
                || local_avail_flipped
                || change.stt_changed
            {
                // Entering (or re-parameterizing) hands-free bypasses the Caps PTT in
                // `tick`, so an in-flight dictation or a still-armed deferred submit
                // would sit in stasis and paste a STALE transcript when the mode flips
                // back. Same hazard as the caps gate going off — same teardown.
                // (Any non-Idle gesture ⇔ the old `holding || confirm_armed`: the enum
                // is exhaustive over exactly those two live modes.)
                if !matches!(self.gesture, GestureState::Idle) {
                    self.teardown_hold();
                }
                self.listener = Some(listener::Listener::new(
                    cfg,
                    self.plat.clone(),
                    ds_model::parakeet_dir().unwrap_or_default(),
                    listener::ListenerShared {
                        paste: self.paste.clone(),
                        stt_active: self
                            .stt_active
                            .clone()
                            .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                        ttsq: self.ttsq.clone(),
                        gate: self.status_gate.clone(),
                    },
                ));
            }
        } else if self.listener.is_some() {
            // Leaving Always mid-capture must not strand the shared state the listener was
            // driving: the listener writes the SAME `stt_active` flag + paste buffer the
            // Caps PTT path uses (see `listener::sync_pill`), so simply dropping it here —
            // possibly mid-utterance — left `stt_active` stuck true (a permanently-lit
            // "recording" indicator) with a stale transcript still in the buffer. Route
            // through the same teardown the caps-gate-off and engine-rebuild paths use.
            self.teardown_hold();
            self.listener = None;
        }
        // Live-push the delay onto whatever listener now exists (freshly built above,
        // or untouched) — cheap field copy, no rebuild needed.
        if let Some(l) = self.listener.as_mut() {
            l.set_paste_submit_delay_ms(cfg.paste_submit_delay_ms);
        }

        // Record the applied config so the NEXT reload diffs against it.
        if let Some(q) = &self.ttsq {
            q.set_config(cfg.clone());
        }
        self.cfg = cfg.clone();
        // NOTE: `press` is a physical-key latch; a config reload does not change the
        // physical key, so leave it as-is.
        log::info!(
            target: "engine",
            "dontspeakd reloaded config (caps={} stt={}{} tts={} long_press={}ms narrate={} \
             extra_terminals={} extra_custom_editors={})",
            self.caps_enabled,
            cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off"),
            if change.stt_changed { " [rebuilt]" } else { "" },
            cfg.resolved_tts().map(|e| e.as_str()).unwrap_or("off"),
            self.long_press_ms,
            cfg.narrate_summary(),
            cfg.extra_terminals.len(),
            cfg.extra_custom_text_editors.len(),
        );
    }

    /// Apply the effective caps-loop gate (`caps_loop_enabled(cfg) && AX trusted`).
    /// If it just went OFF mid-hold, end the HOLD cleanly (no key left down / mic
    /// open). Publishes to the shared `caps_active` for the RPC running-dot, and on
    /// a REAL transition bumps the status-push gate so a blocked `WaitModelStatus`
    /// (the app's dot) sees it immediately — mirrors [`set_stt_active`]. Without
    /// this, `refresh_caps_gate`'s live Accessibility-grant re-probe flips the
    /// backend value straight away but the app's dot stays stale until something
    /// else happens to bump the gate (e.g. a relaunch, which resubscribes fresh).
    ///
    /// A REAL transition also acquires/releases physical ownership of the Caps key
    /// (`ds_platform::acquire_caps_key`/`Platform::release_caps_key`) so OFF actually restores
    /// native OS behavior instead of leaving the key suppressed-but-ignored — and so
    /// no backlog of presses made while OFF can replay in a burst once back ON (see
    /// those methods' docs). Only on a real flip: this is called on every reload
    /// regardless of change, and re-acquiring/releasing an already-owned/released key
    /// is harmless but pointless.
    fn set_caps_gate(&mut self, now_on: bool) {
        // Tear down when recording OR when a deferred submit is still armed (any
        // non-Idle gesture): with the loop off, `tick` stops running the
        // deferred-submit check, so an armed paste would sit in stasis and fire a
        // STALE transcript whenever the gate comes back.
        if !now_on && !matches!(self.gesture, GestureState::Idle) {
            self.teardown_hold();
        }
        if now_on != self.caps_enabled {
            if now_on {
                ds_platform::acquire_caps_key(self.plat.as_ref());
            } else {
                // Releasing means the platform's own press-in-flight tracking is about
                // to reset (Windows' `release_caps_key` wipes its edge queue) — any
                // matching release for an in-flight press is now unobservable, so mirror
                // that reset on the engine's own latch. Without this, a press that
                // straddles the OFF edge leaves the press latch stale (`Down` with an
                // old `since`); the first tick after a later re-enable would see a huge
                // elapsed time and fire a spurious long-press `cancel_all()`. Sync the
                // LED too: `teardown_hold` above already did it if gesture was
                // non-Idle, but this also covers the case it was already Idle (a
                // cheap, correct no-op write) — belt-and-suspenders, and harmless
                // since this whole branch only runs on a real, rare gate flip.
                self.press = PressState::Up;
                self.sync_caps_led();
                self.plat.release_caps_key();
            }
        }
        self.caps_enabled = now_on;
        if let Some(ca) = &self.caps_active
            && ca.swap(now_on, Ordering::Relaxed) != now_on
            && let Some(gate) = &self.status_gate
        {
            gate.bump();
        }
    }

    /// Periodic Accessibility re-probe (called from the loop): if AX trust changed
    /// since last time, flip the caps loop on/off live — so GRANTING Accessibility
    /// turns dictation green without a reload/restart, and revoking turns it off.
    pub(crate) fn refresh_caps_gate(&mut self) {
        let now_on = caps_loop_enabled(&self.cfg) && self.plat.preflight().is_ok();
        if now_on != self.caps_enabled {
            self.set_caps_gate(now_on);
            log::info!(
                target: "engine",
                "caps loop {} (Accessibility re-probe)",
                if now_on { "ENABLED" } else { "disabled" }
            );
        }
    }

    /// Whether the platform's physical-key monitor has confirmed it's stuck (see
    /// [`ds_platform::CapsKeyMonitor::caps_monitor_stuck`]) — the caller
    /// (`boot::engine_run`, polled right next to [`Self::refresh_caps_gate`]) reacts
    /// by relaunching the whole process, the only thing that clears this on macOS.
    pub(crate) fn needs_relaunch(&self) -> bool {
        self.plat.caps_monitor_stuck()
    }

    /// Which resource `needs_relaunch()` is `true` because of — see
    /// [`ds_platform::CapsKeyMonitor::caps_monitor_stuck_detail`]. Used for logging
    /// only; never affects the relaunch decision itself.
    pub(crate) fn relaunch_reason(&self) -> Option<&'static str> {
        self.plat.caps_monitor_stuck_detail()
    }

    /// Discard any backlog an event-driven platform (Windows) queued while `tick` was
    /// taking an early return (Always-listen mode, or `caps_enabled` off) instead of
    /// draining it. No-op on polled platforms (`is_caps_event_driven` false) — they have
    /// no queue, just a sampled boolean. `caps_enabled` and `listen_mode` are independent
    /// config axes (see `tick`'s two early returns below), so a platform can still be
    /// physically OWNED — and so still queuing — while either one alone would suggest
    /// otherwise; belt-and-suspenders with `Platform::release_caps_key` actually
    /// uninstalling the hook, so an undrained backlog never survives to replay in one
    /// burst and corrupt the tap/double-tap state machine once the early return stops.
    fn discard_stale_caps_backlog(&self) {
        if self.plat.is_caps_event_driven() {
            self.plat.drain_caps_events();
        }
    }

    /// One poll, driving the "tap to dictate, hold to cancel" gesture machine off the
    /// PHYSICAL Caps key (down/up edges via `caps_phys_prev`), NOT the OS lock latch:
    ///   * a quick TAP (release before `long_press_ms`) toggles recording;
    ///   * a LONG-PRESS (hold ≥ `long_press_ms`) force-resets to idle;
    ///   * the Caps LED is a pure OUTPUT derived from `is_recording()` via
    ///     `sync_caps_led()`, called at every point `self.gesture` changes — never
    ///     read back to decide state.
    ///
    /// See the inline GESTURE MODEL block below for the full rationale.
    pub(crate) fn tick(&mut self) {
        // A confirmed submit's deferred Enter (see `confirm_paste`/`pending_enter_at`)
        // must still fire even if caps gets disabled or the mode changes mid-delay —
        // same belt-and-suspenders reasoning as `discard_stale_caps_backlog` below, so
        // run this before any mode early-return.
        self.check_pending_enter();
        // Publish whether a terminal is the frontmost app for the TTS worker's focus
        // gate. The worker thread can't call this (NSWorkspace is poll/main-thread
        // affine), so the poll thread samples it here — every tick, before any mode
        // early-return, since narration plays even when caps is off or in always-listen
        // mode. Cheap in-process read (NSWorkspace / GetForegroundWindow).
        if let Some(q) = &self.ttsq {
            // `pause_in_background` is the SOLE consumer of `terminal_front` (the worker's
            // focus gate uses `pause_in_background && terminal_seen && !terminal_front`).
            // When it's off, the frontmost probe is dead — so skip the poll/main-thread
            // NSWorkspace round-trip (~33×/s forever in the common idle case) and publish
            // `true`, which keeps the gate's `!terminal_front` term false (never silences).
            let front = if self.cfg.pause_in_background {
                self.plat.is_terminal_frontmost()
            } else {
                true
            };
            q.set_terminal_front(front);
            q.set_pause_in_background(self.cfg.pause_in_background);
        }

        // LIVE paste-target probe: while the dictation panel is up (recording or awaiting
        // the confirm tap), sample whether an editable field is focused so the app can
        // tint the glow when there's nowhere to paste. Only while the panel shows — the
        // Accessibility probe isn't free, and it's meaningless otherwise.
        let recording = self
            .stt_active
            .as_ref()
            .map(|r| r.load(Ordering::Relaxed))
            .unwrap_or(false);
        if recording || self.awaiting_confirm() {
            // A focused editable field is the primary signal, but terminals — the app's
            // MAIN dictation target — frequently don't expose an AX-settable text element
            // (custom-drawn TTY views), so `has_paste_target` reads "no target" even
            // though a synthetic Cmd+V lands fine. Treat a frontmost terminal as a paste
            // target too: both the Caps (`confirm_paste`) and voice-submit
            // (`listener::exec`) paths paste unconditionally into whatever is focused —
            // there's no focus refusal — so the glow must not warn there.
            let present = self.plat.has_paste_target() || self.plat.is_terminal_frontmost();
            if let Ok(mut p) = self.paste.lock() {
                p.has_paste_target = present;
            }
        }

        // PUSH gate: if the dictation-overlay preview changed this tick, wake any blocked
        // `WaitModelStatus` so the app re-renders the overlay immediately (≤ one tick of
        // latency) instead of waiting out its status-poll timer. Runs in every mode
        // (PTT, always-listen, caps-off) — it only bumps on an actual change. Recording
        // start/stop is pushed separately at its flip site (`set_stt_active`).
        self.publish_status_change();

        // Always-listening mode bypasses the Caps-Lock PTT entirely: drive the
        // hands-free loop instead, gated on TTS playback (half-duplex play-gate).
        // Caps state is ignored while this mode is active.
        if self.cfg.listen_mode == ds_config::ListenMode::Always {
            let busy = self.ttsq.as_ref().map(|q| q.is_busy()).unwrap_or(false);
            // A hands-free submit applies `input_clears` per scope, same as any other submit.
            let cancel_current = self.cfg.input_clears.contains(&CancelSpeechScope::Current);
            let cancel_other = self.cfg.input_clears.contains(&CancelSpeechScope::Other);
            if let Some(l) = self.listener.as_mut() {
                l.tick(busy, cancel_current, cancel_other);
            }
            // Always-listen bypasses the Caps gesture, but `caps_enabled` (and so the
            // platform's key ownership) is a SEPARATE axis — an event-driven platform
            // (Windows) may still be queuing every physical press regardless of mode.
            // Discard rather than let it accumulate: an unbounded/backlogged queue would
            // replay in one burst the moment `listen_mode` switches back, corrupting the
            // tap/double-tap state machine exactly like the bug `release_caps_key`
            // otherwise closes (see its doc). Belt-and-suspenders with that fix, and the
            // only guard for this specific axis.
            self.discard_stale_caps_backlog();
            return;
        }

        // caps_enabled gate: when the dictation loop is off, the engine does no
        // polling and no emits — it's a pure RPC host for the other subsystems.
        if !self.caps_enabled {
            // Same reasoning as above — belt-and-suspenders in case the platform's own
            // key release didn't (or couldn't) stop the backlog from growing.
            self.discard_stale_caps_backlog();
            return;
        }

        // ─────────────────────────────────────────────────────────────────────────
        // GESTURE MODEL — "tap to dictate, hold to cancel". Driven entirely off the
        // PHYSICAL Caps key (down / hold / up), NOT the OS lock latch:
        //   • DOWN    — nothing yet. Start the press timer; re-assert the LED to the
        //               real recording state so the OS's own latch-flip never changes
        //               the light on a press.
        //   • HOLD ≥ long_press_ms — CANCEL: discard any in-flight dictation AND silence
        //               the voice/generation, back to idle. Never records, never lights.
        //   • quick UP (released before the threshold) — a TAP toggles dictation: start
        //               when idle, stop+submit when recording, via `start_recording`/
        //               `stop_recording` — which sync the LED themselves. For an
        //               IMMEDIATE tap (not speaking) that happens right here, on this
        //               release; for a speaking-DEFERRED tap it happens on a LATER
        //               tick instead (see `check_pending_tap`), since there's no
        //               release edge left by then to hang it off of.
        // The Caps LED is a pure OUTPUT derived from `is_recording()` — every function
        // that assigns `self.gesture` calls `sync_caps_led()` (or a helper that does),
        // so the light is never read back to decide state and can't independently
        // drift from the gesture that's supposed to drive it. (A sub-poll tap too fast
        // for the ~30 ms poll to even observe the key-down is missed — tap again; the
        // old latch-mirror caught those, at the cost of the desync bugs this model
        // removes.)
        // Feed the gesture machine from whichever source the platform exposes:
        //   • EVENT-DRIVEN (Windows low-level hook) — drain the lossless queue and replay
        //     every real transition. A down+up that both fell inside one tick is two edges
        //     here, so a tap faster than the poll is NEVER dropped (the old miss).
        //   • POLLED (macOS / Linux / tests) — sample the held boolean and synthesize one
        //     edge when it changed since last tick, exactly as before.
        if self.plat.is_caps_event_driven() {
            for e in self.plat.drain_caps_events() {
                self.apply_caps_edge(e.down, e.at);
            }
            // Keep the polled mirror coherent (the event path doesn't read it, but other
            // code may inspect it); it tracks the live latched state.
            self.caps_phys_prev = self.plat.is_caps_physically_down();
        } else {
            let down = self.plat.is_caps_physically_down();
            let prev = self.caps_phys_prev;
            self.caps_phys_prev = down;
            if down != prev {
                self.apply_caps_edge(down, Instant::now());
            }
        }
        // Time-based half of the gesture: a sustained HOLD fires the long-press CANCEL even
        // when no new edge arrives this tick, and the Caps LED is re-pinned to the
        // recording state while the key is down (a no-op on the event-driven port, which
        // owns/suppresses the key and never drives the LED).
        self.check_long_press();
        // Fire a deferred single tap if its double-tap window lapsed with no second tap.
        self.check_pending_tap();

        // DEFERRED submit: the stop tap armed `ConfirmArmed`; the local-transcript engine
        // deposits its FINAL asynchronously, so paste once it lands (or disarm if empty).
        // The LED is already OFF (driven on the stop tap's release) — this only moves text.
        // Held while the stop tap's flip-gesture double-tap window is still open (so a
        // fast final can't paste under the WRONG outcome before the second tap has a
        // chance to land and flip it) and while any Caps press is in flight (so it
        // can't paste under a press meant to cancel).
        if self.is_confirm_armed() && !self.deferred_submit_held() {
            // Read the finalize state under ONE lock so the async joiner can't straddle
            // two separate checks (deposit the final between them and get disarmed).
            let (has_ready, is_empty) = self
                .paste
                .lock()
                .map(|p| {
                    (
                        matches!(p.final_state, FinalState::Ready(_)),
                        matches!(p.final_state, FinalState::Empty),
                    )
                })
                .unwrap_or((false, false));
            if has_ready {
                self.confirm_paste();
            } else if is_empty {
                // The deferred final landed EMPTY — nothing to submit. Disarm
                // (`disarm_confirm` maps the buffer's `Empty → Idle` too).
                self.disarm_confirm();
                self.record_caps("confirm");
            }
            // `Armed` (final not landed yet) / `Idle`: keep waiting.
        }
    }

    /// Whether the deferred submit must WAIT before pasting. Two holds, both bounded:
    ///   * ANY Caps press in flight while armed — the press resolves to the double's
    ///     second tap, a long-press CANCEL, or a new-dictation tap (which wipes
    ///     the landed final anyway); pasting mid-press could drop text+Enter under a press
    ///     meant to cancel.
    ///   * The stop tap's flip-gesture double-tap window is still open and no double
    ///     has landed yet. Unconditionally honored — exactly one of the two stop-tap
    ///     gestures always submits and the other always inserts, so the window always
    ///     matters to whichever outcome lands.
    ///
    /// Deliberately does NOT also check `enter_after_paste`: with `double_tap_submits`
    /// on, the stop tap arms with it FALSE (insert-only provisionally), and a real
    /// double tap must still be waited for to flip it to `true` — gating on the
    /// current value would see `false` from the very first tick and never wait.
    fn deferred_submit_held(&self) -> bool {
        if matches!(self.press, PressState::Down { .. }) {
            return true;
        }
        match self.gesture {
            GestureState::ConfirmArmed {
                stop_tap_at,
                double_pending,
                ..
            } => double_pending || within_double_tap(stop_tap_at, Instant::now()),
            _ => false,
        }
    }

    /// Apply ONE physical Caps transition (`down` = pressed, `!down` = released) stamped
    /// at `at`. The edge half of the "tap to dictate, hold to cancel" gesture, shared by
    /// both the event-driven (Windows hook) and polled (macOS/Linux) feeds in [`tick`]:
    ///   * DOWN — begin a press; the decision defers to the release (tap) or the
    ///     long-press threshold (hold), both handled in [`check_long_press`].
    ///   * UP — a release NOT consumed by a long-press is a TAP → toggle dictation
    ///     (start when idle, stop+submit when recording).
    ///
    /// The Caps-held mirror feeds `PasteBuf::caps_held` so model_status suppresses the
    /// finalized transcript while a press is IN FLIGHT (a hold-cancel must not flash the
    /// bubble before it dismisses).
    fn apply_caps_edge(&mut self, down: bool, at: Instant) {
        if let Ok(mut p) = self.paste.lock() {
            p.caps_held = down;
        }
        if down {
            self.press = PressState::Down {
                since: at,
                long_press_fired: false,
            };
            // A press beginning inside the stop tap's double-tap window is the SECOND
            // tap of the flip double (or a long-press cancel of the pending paste) —
            // never the start of a new recording. Anchored on the PRESS so a slow
            // release can't misread the gesture as a new dictation and wipe the
            // still-unpasted transcript. Unconditional — no config gates this: consuming
            // the tap always protects the pending transcript from being wiped by an
            // accidental new recording. (The not-armed case needs no stale-clear
            // anymore: `double_pending` only exists inside `ConfirmArmed`.)
            if let GestureState::ConfirmArmed {
                stop_tap_at,
                double_pending,
                ..
            } = &mut self.gesture
            {
                *double_pending = within_double_tap(*stop_tap_at, at);
            }
            self.record_caps("press");
        } else {
            // The light only ever moves via `start_recording`/`stop_recording`'s own
            // `sync_caps_led()` call — never here directly. For an IMMEDIATE tap (not
            // speaking), `handle_tap` below calls `toggle_dictation` synchronously, so
            // the LED is already correct by the time this function returns. A DEFERRED
            // tap (speaking) instead resolves later from `check_pending_tap`, with no
            // release edge left by then — which is exactly why the write had to move
            // into `start_recording`/`stop_recording` themselves rather than staying
            // here. `Up ⇒ tap` matches the old `!self.long_press_fired` (which read
            // true even with no press latched — e.g. a release edge observed after a
            // gate off→on cycle reset the latch but not `caps_phys_prev`).
            let was_tap = matches!(
                self.press,
                PressState::Down {
                    long_press_fired: false,
                    ..
                }
            );
            self.press = PressState::Up;
            self.record_caps("release");
            if was_tap {
                self.handle_tap(at);
            }
        }
    }

    /// A Caps TAP (quick release). While speech is PLAYING, the tap is DEFERRED up to
    /// [`DOUBLE_TAP_MS`] to see whether a SECOND tap follows: two quick taps = skip the
    /// current message and advance to the next ([`TtsQueue::skip_current`]); a lone tap =
    /// the normal [`toggle_dictation`](Self::toggle_dictation), fired from [`tick`] once the
    /// window lapses. While NOT speaking there is nothing to skip, so the tap acts
    /// IMMEDIATELY — starting dictation from silence keeps zero added latency.
    fn handle_tap(&mut self, at: Instant) {
        // Second tap of the stop double → FLIP the armed outcome (see
        // `enter_after_paste`). Checked before the speak-skip logic — the stop tap
        // resumed the voice, so speech may already be playing again by this release.
        if let GestureState::ConfirmArmed {
            stop_tap_at,
            double_pending,
            enter_after_paste,
        } = &mut self.gesture
            && *double_pending
        {
            *double_pending = false;
            *enter_after_paste = !*enter_after_paste;
            *stop_tap_at = None;
            // Match `confirm_paste`'s actual gate (`enter_after_paste`).
            let submits = *enter_after_paste;
            if submits {
                log::debug!(target: "engine", "double-tap on stop — will submit (paste + auto-Enter)");
            } else {
                log::debug!(target: "engine", "double-tap on stop — insert only (paste, no auto-Enter)");
            }
            return;
        }
        let speaking = self.ttsq.as_ref().is_some_and(|q| q.is_tts_active());
        let window = Duration::from_millis(DOUBLE_TAP_MS);
        match tap_decision(speaking, self.pending_tap_at, at, window) {
            // Not speaking (or no prior tap to pair) — act now, no added latency.
            TapAction::Immediate => {
                self.pending_tap_at = None;
                self.toggle_dictation();
            }
            // Second tap inside the window → DOUBLE-TAP: skip the current message.
            TapAction::Skip => {
                self.pending_tap_at = None;
                if let Some(q) = &self.ttsq {
                    q.skip_current();
                }
                log::debug!(target: "engine", "double-tap — skipped current message, advancing to next");
            }
            // First tap while speaking → defer; the single fires from `tick` if no
            // second tap arrives within the window.
            TapAction::Defer => self.pending_tap_at = Some(at),
        }
    }

    /// Fire a DEFERRED single tap once its [`DOUBLE_TAP_MS`] window has elapsed with no
    /// second tap. Skipped while a Caps press is in flight (`press` is `Down`) — that
    /// could be the second tap of a double, or a hold becoming a long-press — so the single
    /// never fires mid-gesture. Run once per [`tick`].
    fn check_pending_tap(&mut self) {
        if matches!(self.press, PressState::Down { .. }) {
            return;
        }
        if self.pending_tap_at.is_some() && !within_double_tap(self.pending_tap_at, Instant::now())
        {
            self.pending_tap_at = None;
            self.toggle_dictation();
        }
    }

    /// Fire a deferred submit's Enter once `paste_submit_delay_ms` has elapsed. Run
    /// once per [`tick`], regardless of gesture state, so a delay armed by
    /// `confirm_paste` still fires even if the gesture moves on in the meantime.
    fn check_pending_enter(&mut self) {
        if self
            .pending_enter_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.pending_enter_at = None;
            self.press_deferred_enter();
        }
    }

    /// Press Enter for a confirmed submit and mark it so `MarkActive` doesn't
    /// double-count its own echo as a separate submit. Called either immediately
    /// (zero delay) or once `check_pending_enter`'s timer elapses.
    fn press_deferred_enter(&mut self) {
        self.plat.press_enter();
        if let Some(q) = &self.ttsq {
            // Mark the voice submit's own auto-Enter so the `MarkActive` path doesn't
            // double-count it as a separate submit. This must stay pinned to the REAL
            // Enter keystroke (not the earlier paste/cancel step) since
            // `note_voice_submit`'s de-dup window is a few seconds wide.
            q.note_voice_submit();
        }
    }

    /// The time-based half of the gesture, run once per [`tick`] regardless of edges:
    /// a Caps hold past `long_press_ms` force-resets to idle (CANCEL — discard
    /// dictation + silence voice), exactly once per press; and the Caps LED is
    /// re-pinned to the recording state for as long as the key is held (counters the
    /// OS's own hold-delay latch flip on the polled ports — a no-op on Windows, which
    /// suppresses the key outright).
    fn check_long_press(&mut self) {
        let PressState::Down {
            since,
            long_press_fired,
        } = self.press
        else {
            return;
        };
        if !long_press_fired && since.elapsed() >= Duration::from_millis(self.long_press_ms) {
            self.cancel_all();
            self.press = PressState::Down {
                since,
                long_press_fired: true,
            };
        }
        self.sync_caps_led();
    }

    /// Start dictation — called from `toggle_dictation`, either immediately (a tap's
    /// RELEASE while nothing is speaking) or later (a speaking-deferred tap, resolved
    /// from `check_pending_tap` on a subsequent tick once the double-tap window
    /// lapses with no second tap — see `handle_tap`'s doc). No-op if already
    /// recording. ClaudeNative posts the focus-gated initial Ctrl+G key-DOWN. A
    /// long-press (`cancel_all`) cancels everything.
    ///
    /// COEXIST (full-duplex): a dictation tap runs the listen ALONGSIDE an in-flight
    /// reply — the warm helper does concurrent speak+listen (engine stdout demux)
    /// and the VPIO AEC keeps the playback out of the mic. So in full-duplex we do
    /// NOT barge here; only a long-press (`cancel_all`) cancels the speech.
    /// Half-duplex keeps interrupt-and-dictate: ONE tap barges any TTS (clears the
    /// warm queue + kills the cold-path speaker) and opens the mic, because the
    /// device cannot capture and render at once there.
    fn start_recording(&mut self) {
        if self.is_recording() {
            return;
        }
        // stt_engine = off: never open the mic. The Caps tap's voice pause/resume is
        // handled in `toggle_dictation` (so a tap HOLDS the voice, same as the dictation
        // path), so this guard is only reached defensively — just don't record.
        if self.cfg.resolved_stt().is_none() {
            return;
        }
        // A Caps tap = "I have the floor": PAUSE the in-process queue in BOTH duplex
        // modes (it resumes on stop) so the voice never talks over your dictation.
        // `tts.stop()` is a playback-stop control message, not a child kill, so it is
        // safe in full-duplex — the open VPIO mic stays live for the dictation.
        // Hands-free always-listening never calls this path, so it keeps coexisting.
        if let Some(q) = &self.ttsq {
            q.pause_for_record();
        }
        // Half-duplex only: barge the COLD external speak-hook (the engine-down
        // fallback), which can't be paused. No cold path exists in full-duplex.
        if !self.is_full_duplex()
            && let Some(pgid) = ds_proc::barge_in(&self.pidfile)
        {
            log::debug!(target: "engine", "barge-in: killed TTS pgid={pgid}");
        }
        // Entering `Recording` structurally drops any armed confirm sub-state (the
        // old disarm-before-record contract, now a single assignment).
        self.gesture = GestureState::Recording;
        // Sync the LED HERE, not left to the caller: `toggle_dictation` (this
        // function's only production caller) can fire either immediately from a
        // tap's release edge, or later from `check_pending_tap` once a
        // speaking-deferred tap's double-tap window lapses — in the deferred case
        // there is no release edge left to snap the LED, so this must own it.
        self.sync_caps_led();
        // Capture the paste target (the app that's ALREADY focused) + clear any stale
        // preview so the confirm panel opens fresh, labeled with where the text will
        // land. We never steal focus here — the transcript pastes into whatever the
        // user is in (the focus-gated Ctrl+G / paste targets the current frontmost app).
        let target = self.plat.frontmost_app_name();
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.final_state = FinalState::Idle;
            p.target = target;
            // A real recording is starting: drop any still-live refusal cue (the model
            // may have become ready inside the window) so the panel shows the normal
            // listening state, not the warning wash.
            p.refused_until = None;
            // Fresh session: bump the epoch so a prior `stop` joiner that hasn't
            // deposited yet is invalidated and can't clobber this recording (see
            // PasteBuf::epoch). HelperStt::start bumps again under the same lock; both
            // bumps just advance the counter, so the net effect is one new session.
            p.epoch = p.epoch.wrapping_add(1);
        }
        // Publish the recording flag BEFORE opening the mic. The half-duplex
        // barge-watcher reads `stt_active` to distinguish our OWN dictation mic from a
        // foreign recorder (its `!ours` gate); setting it first means the watcher can
        // never observe our mic as foreign in the gap before the flag — the gate is
        // race-free by construction, not merely by poll timing. (HelperStt::start opens
        // the mic asynchronously via the warm helper, so no capture is lost by ordering
        // the flag first.)
        self.set_stt_active(true);
        self.stt.start();
        self.record_caps("start");
        log::debug!(target: "engine", "LED ON — stt.start()");
    }

    /// Stop dictation (full-mirror ON→OFF edge). No-op if not recording. Ends the
    /// listen (`stt.stop`) and arms the deferred submit for local-transcript engines.
    fn stop_recording(&mut self) {
        if !self.is_recording() {
            return;
        }
        self.gesture = GestureState::Idle;
        // Sync now, for the same reason `start_recording` does: this may be reached
        // from the deferred-tap path with no release edge left to snap the LED. The
        // possible re-assignment to `ConfirmArmed` just below is still "not
        // recording", so this write stays correct either way.
        self.sync_caps_led();
        self.stt.stop();
        self.set_stt_active(false);
        // Mic freed: resume the TTS queue paused on the start-tap (half-duplex). No-op
        // in full-duplex (never paused) and when nothing was paused/playing.
        if let Some(q) = &self.ttsq {
            q.resume();
        }
        // The STOPPING press IS the submit gesture in BOTH modes — a quick release
        // submits, a HELD press cancels (the long-press → discard/reset). There is no
        // separate confirm tap (half-duplex used to need a second tap, which desynced
        // the Caps LED out of band). Local-transcript engines deposit their final
        // ASYNCHRONOUSLY, so we can't gate on `awaiting_confirm()` here (pending isn't
        // ready yet): arm on this stop gesture and let the poll loop paste once the
        // final lands, or disarm if it's empty. The inline path (ClaudeNative) submits
        // via Ctrl+G and never defers, so it doesn't arm.
        if self.stt.defers_paste() {
            // Arm the deferred submit, anchoring the flip double-tap window on this
            // stop gesture: a second tap within DOUBLE_TAP_MS flips the armed outcome
            // (see handle_tap / deferred_submit_held). `double_tap_submits` off
            // (default) arms a lone tap to submit; on, it arms a lone tap to
            // insert-only and a double to submit.
            self.gesture = GestureState::ConfirmArmed {
                stop_tap_at: Some(Instant::now()),
                double_pending: false,
                enter_after_paste: !self.cfg.double_tap_submits,
            };
            // Mirror into the shared buffer BEFORE the panel's next status read (see
            // `FinalState::Armed`) — keeps the panel up across the gap between this
            // synchronous `recording → false` flip and the async final landing.
            // `arm()` never downgrades a final the detached joiner already deposited
            // between `stt.stop()` above and this lock (see its doc).
            if let Ok(mut p) = self.paste.lock() {
                p.arm();
            }
        }
        self.record_caps("stop");
        log::debug!(target: "engine", "LED OFF — stt.stop()");
    }

    /// Never leave a key down on shutdown.
    pub(crate) fn shutdown(&mut self) {
        self.teardown_hold();
        self.listener = None;
        self.pending_tap_at = None;
        self.pending_enter_at = None;
        self.press = PressState::Up;
        self.set_caps_gate(false);
        self.plat.set_caps_lock(false);
        self.plat.release_caps_key();
        log::info!(target: "engine", "dontspeakd stopped");
    }
}

/// What a completed Caps TAP does — UNIFIED across dictation on/off so the gesture means
/// the same thing either way: a tap PAUSES the voice (held, never dropped), the next tap
/// RESUMES it. With dictation on the pause/resume rides the record start/stop; with it off
/// the mic never opens but the voice still pauses/resumes. On-but-NOT-READY is its own
/// case ([`Refused`](CapsTap::Refused)) — neither a record toggle nor a voice pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapsTap {
    /// Dictation on + ready, idle → begin recording (which pauses the voice).
    StartRecord,
    /// Dictation on, recording → stop + submit (which resumes the voice).
    StopRecord,
    /// Dictation on but NOT READY (model still downloading/loading), idle → refuse the
    /// start: a strict no-op for the voice (the caller arms the visual refusal cue).
    /// Deliberately NOT the OFF pause/resume gesture — a pause here has no matching
    /// resume when the model comes ready, so it silenced TTS for the whole download
    /// window (issue #1).
    Refused,
    /// Dictation off, voice playing/idle → pause the voice (hold; nothing dropped).
    PauseVoice,
    /// Dictation off, voice paused → resume the voice.
    ResumeVoice,
}

/// Decide a Caps tap's action from `(dictation on?, engine ready?, currently recording?,
/// voice paused?)`. `dictation_on` (an engine is selected) and `ready` (it can transcribe
/// RIGHT NOW) are deliberately separate inputs: collapsing them into one gate is exactly
/// what made on-but-not-ready borrow the OFF pause/resume path (issue #1). Pure — the
/// engine wires the result to the queue, and this is exhaustively unit-tested.
pub(crate) fn caps_tap_action(
    dictation_on: bool,
    ready: bool,
    recording: bool,
    voice_paused: bool,
) -> CapsTap {
    match (dictation_on, ready, recording, voice_paused) {
        // Recording ignores `ready`: the model was ready when the recording started, and
        // a tap must always still be able to STOP it.
        (true, _, true, _) => CapsTap::StopRecord,
        (true, true, false, _) => CapsTap::StartRecord,
        (true, false, false, _) => CapsTap::Refused,
        (false, _, _, false) => CapsTap::PauseVoice,
        (false, _, _, true) => CapsTap::ResumeVoice,
    }
}

/// What a Caps tap should do — the pure time-and-state core of [`Engine::handle_tap`].
#[derive(Debug, PartialEq, Eq)]
enum TapAction {
    /// Act now (the normal toggle). Not speaking, or a stale prior tap → treat as a new one.
    Immediate,
    /// First tap while speaking — hold the action until the double-tap window lapses.
    Defer,
    /// Second tap within the window → skip the current message.
    Skip,
}

/// Whether `t0` (if any) is still within [`DOUBLE_TAP_MS`] of `now` — the "recent tap"
/// test shared by the stop-tap flip gesture's three call sites
/// ([`Engine::deferred_submit_held`], [`Engine::apply_caps_edge`],
/// [`Engine::check_pending_tap`]). Pure.
fn within_double_tap(t0: Option<Instant>, now: Instant) -> bool {
    t0.is_some_and(|t0| now.saturating_duration_since(t0) <= Duration::from_millis(DOUBLE_TAP_MS))
}

/// Decide a tap from `(speech playing?, the pending deferred tap, this tap's time, window)`.
/// Not speaking ⇒ `Immediate` (zero added latency on starting dictation from silence).
/// Speaking ⇒ `Skip` if a prior tap is within `window`, else `Defer` (incl. a stale prior
/// tap, which the caller has already fired from `tick`). Pure — exhaustively unit-tested.
fn tap_decision(
    speaking: bool,
    pending: Option<Instant>,
    now: Instant,
    window: Duration,
) -> TapAction {
    if !speaking {
        return TapAction::Immediate;
    }
    match pending {
        Some(t0) if now.saturating_duration_since(t0) <= window => TapAction::Skip,
        _ => TapAction::Defer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_live_tracks_the_deadline() {
        // No refusal armed ⇒ never live, regardless of clock.
        let now = Instant::now();
        assert!(!refusal_live(None, now));
        // Deadline still ahead ⇒ live.
        assert!(refusal_live(Some(now + Duration::from_millis(10)), now));
        // Deadline already passed ⇒ no longer live (strict `<`, so `now == t` also expires).
        assert!(!refusal_live(Some(now - Duration::from_millis(1)), now));
        assert!(!refusal_live(Some(now), now));
    }

    #[test]
    fn tap_decision_immediate_when_not_speaking() {
        // Not speaking ⇒ act now regardless of any pending tap (dictation-start stays instant).
        let now = Instant::now();
        let w = Duration::from_millis(280);
        assert_eq!(tap_decision(false, None, now, w), TapAction::Immediate);
        assert_eq!(tap_decision(false, Some(now), now, w), TapAction::Immediate);
    }

    #[test]
    fn tap_decision_defers_then_skips_while_speaking() {
        let t0 = Instant::now();
        let w = Duration::from_millis(280);
        // Speaking, no prior tap → DEFER (first tap of a possible double).
        assert_eq!(tap_decision(true, None, t0, w), TapAction::Defer);
        // Second tap INSIDE the window → SKIP the current message.
        let inside = t0 + Duration::from_millis(100);
        assert_eq!(tap_decision(true, Some(t0), inside, w), TapAction::Skip);
        // At the exact window boundary still counts as a double (<=).
        assert_eq!(tap_decision(true, Some(t0), t0 + w, w), TapAction::Skip);
        // Second tap BEYOND the window → DEFER again (a stale prior tap already fired as a
        // single from `tick`; this one starts a fresh deferral, not a skip).
        let beyond = t0 + Duration::from_millis(281);
        assert_eq!(tap_decision(true, Some(t0), beyond, w), TapAction::Defer);
    }

    #[test]
    fn caps_tap_action_is_pause_resume_in_both_modes() {
        // Dictation ON + ready: tap toggles recording (start pauses, stop resumes) — never
        // clears.
        assert_eq!(
            caps_tap_action(true, true, false, false),
            CapsTap::StartRecord
        );
        assert_eq!(
            caps_tap_action(true, true, true, false),
            CapsTap::StopRecord
        );
        // voice_paused is irrelevant while dictation is on (record state drives it).
        assert_eq!(
            caps_tap_action(true, true, false, true),
            CapsTap::StartRecord
        );
        assert_eq!(caps_tap_action(true, true, true, true), CapsTap::StopRecord);

        // Dictation OFF: a tap PAUSES (held, not cleared/dropped), the next tap RESUMES —
        // the same pause/resume gesture as dictation-on, so Caps is consistent and no
        // narration is ever silenced; it's queued while paused. `ready` can't be true with
        // no engine selected, but the decision must not depend on it either way.
        for ready in [false, true] {
            assert_eq!(
                caps_tap_action(false, ready, false, false),
                CapsTap::PauseVoice
            );
            assert_eq!(
                caps_tap_action(false, ready, false, true),
                CapsTap::ResumeVoice
            );
            // recording can't be true when dictation is off; both states still pause/resume.
            assert_eq!(
                caps_tap_action(false, ready, true, false),
                CapsTap::PauseVoice
            );
            assert_eq!(
                caps_tap_action(false, ready, true, true),
                CapsTap::ResumeVoice
            );
        }
    }

    #[test]
    fn caps_tap_action_refuses_on_but_not_ready_without_touching_the_voice() {
        // Dictation ON but the engine can't start yet (model downloading/loading): the tap
        // is REFUSED — not a record toggle, and crucially NOT the off-mode voice pause,
        // which had no matching resume when the model came ready and so silenced TTS for
        // the whole download window (issue #1). voice_paused must not change the verdict:
        // a refused tap neither pauses nor resumes.
        assert_eq!(caps_tap_action(true, false, false, false), CapsTap::Refused);
        assert_eq!(caps_tap_action(true, false, false, true), CapsTap::Refused);
        // Already recording: readiness is moot — the tap still STOPS (the model was ready
        // when the recording started; a transient not-ready must never wedge a stop).
        assert_eq!(
            caps_tap_action(true, false, true, false),
            CapsTap::StopRecord
        );
        assert_eq!(
            caps_tap_action(true, false, true, true),
            CapsTap::StopRecord
        );
    }

    #[test]
    fn caps_tap_pauses_then_resumes_with_dictation_off() {
        // End-to-end through the real gesture path: with dictation off, a Caps tap PAUSES
        // the voice (held — so any narration that arrives stays queued, not dropped) and
        // the next tap RESUMES it. ttsq is None in tests, so the queue calls are no-ops;
        // we assert the engine's pause STATE flips (the decision that replaced the old
        // silence/clear behavior). Recording never starts (the mic stays shut).
        let mut d = mk(600);
        d.cfg.stt_engine_ladder = Vec::new(); // dictation off
        MockPlatform::tap(&mut d);
        assert!(d.voice_paused, "first tap pauses the voice");
        assert!(!d.is_recording(), "dictation off → the mic never opens");
        MockPlatform::tap(&mut d);
        assert!(!d.voice_paused, "second tap resumes the voice");
    }

    #[test]
    fn refused_start_on_but_not_ready_never_pauses_the_voice() {
        // Regression for issue #1: dictation ON (`built_in` selected) but the model still
        // downloading/loading. Reaching that state needs a REAL `ttsq` —
        // `stt_ready_to_dictate` short-circuits to plain on/off whenever `ttsq` is `None`,
        // which is why no other engine-level test could ever hit this branch. The stub's
        // fresh `TtsManager` reports Parakeet not resident, so the tap is refused — and a
        // refused tap must leave the voice strictly alone: the old shared pause path had
        // no matching resume when the model came ready, silencing TTS for the whole
        // download window.
        let mut d = mk(600);
        d.cfg.stt_engine = Some(vec![ds_config::SttEngine::BuiltIn]);
        let q = crate::ttsq::TtsQueue::test_stub();
        d.ttsq = Some(q.clone());
        MockPlatform::tap(&mut d);
        assert!(!d.is_recording(), "not ready → the mic must not open");
        assert!(
            !d.voice_paused,
            "a refused start must not flip the pause latch"
        );
        assert!(
            !q.is_paused(),
            "a refused start must not pause the TTS queue"
        );
        assert!(
            d.paste.lock().unwrap().refused_until.is_some(),
            "the visual refusal cue still fires (unchanged behavior)"
        );
        // Tap-happy user during the download: every further tap stays a pure refusal —
        // in particular the second tap must not RESUME-toggle its way into weird states.
        MockPlatform::tap(&mut d);
        assert!(!d.voice_paused && !q.is_paused() && !d.is_recording());
    }
    use crate::config_gate::DEFAULT_LONG_PRESS_MS;
    use ds_platform::{CapsEdge, CapsKeyMonitor, FrontmostWindow, KeyInjector, PreflightError};
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    /// A controllable mock Platform for the §F long-press logic. All state is
    /// `Cell` so the `&self` trait methods can record/return without unsafe.
    #[derive(Default)]
    struct MockPlatform {
        // inputs the test drives
        caps_phys_down: Cell<bool>,
        lock_state: Cell<bool>,
        terminal_frontmost: Cell<bool>,
        /// Whether an editable field is "focused" — backs `has_paste_target`, which
        /// the engine samples live to drive the dictation "no target" glow. Defaults
        /// false (Cell default).
        paste_target: Cell<bool>,
        /// Whether `preflight()` should report Accessibility NOT trusted (`Err`) instead
        /// of the default `Ok` — lets tests exercise `refresh_caps_gate`'s live AX
        /// re-probe transitions. Defaults to `false` (Ok), preserving every existing
        /// test's assumption that a freshly-built engine starts with caps enabled.
        preflight_denied: Cell<bool>,
        /// Opts this mock into the EVENT-DRIVEN feed (`tick`'s Windows-hook branch)
        /// instead of the polled `caps_phys_down` sampling every other test relies on.
        /// Defaults `false` (Cell default) so all existing tests are unaffected.
        event_driven: Cell<bool>,
        /// Queued transitions drained by `drain_caps_events` — lets a test replay a
        /// down+up (or more) pair inside a SINGLE `tick()`, mirroring what the Windows
        /// low-level hook can deliver when a tap lands faster than the poll gap.
        caps_event_queue: RefCell<VecDeque<CapsEdge>>,
        // outputs the test asserts on
        tap_down_calls: Cell<u32>,
        tap_up_calls: Cell<u32>,
        set_caps_off_calls: Cell<u32>,
        /// Count of `type_text` (paste) calls — lets the focus-check tests assert
        /// whether a confirm tap actually pasted.
        type_text_calls: Cell<u32>,
        /// Count of `press_enter` (auto-submit) calls — lets the insert-only
        /// double-tap tests assert whether the paste also pressed Enter.
        press_enter_calls: Cell<u32>,
        /// Count of shared acquisition attempts / `release_caps_key` calls — lets tests assert the
        /// engine takes/gives up physical key ownership on exactly the right
        /// construction/ON/OFF transitions (`caps_key_ownership_*` tests below), not just
        /// that `caps_enabled`/`gesture` end up in the right state.
        acquire_caps_key_calls: Cell<u32>,
        normalize_caps_lock_calls: Cell<u32>,
        release_caps_key_calls: Cell<u32>,
        /// Records every `set_extra_terminals`/`set_extra_custom_text_editors` call, in
        /// order — lets `reload_pushes_extra_paste_target_lists_to_the_platform` assert
        /// both `Engine::assemble`'s initial push AND `Engine::reload`'s subsequent one.
        set_extra_terminals_calls: RefCell<Vec<Vec<String>>>,
        set_extra_custom_text_editors_calls: RefCell<Vec<Vec<String>>>,
        /// Every chord actually tapped, in order — lets tests assert WHICH chord
        /// `ClaudeNative` taps (e.g. distinguishing an injected-`Paths`-derived chord
        /// from the default), not just how many taps happened.
        tapped_chords: RefCell<Vec<ds_platform::KeyChord>>,
    }

    impl KeyInjector for MockPlatform {
        // A `tap_key` is one discrete press+release, so it bumps BOTH the down and up
        // counters the caps-state-machine tests assert on — keeping every existing
        // assertion valid now that ClaudeNative taps a chord instead of Ctrl+G down/up.
        fn tap_key(&self, _chord: &ds_platform::KeyChord) {
            self.tapped_chords.borrow_mut().push(_chord.clone());
            self.tap_down_calls.set(self.tap_down_calls.get() + 1);
            self.tap_up_calls.set(self.tap_up_calls.get() + 1);
        }
        fn type_text(&self, _text: &str) {
            self.type_text_calls.set(self.type_text_calls.get() + 1);
        }
        fn press_enter(&self) {
            self.press_enter_calls.set(self.press_enter_calls.get() + 1);
        }
    }
    impl FrontmostWindow for MockPlatform {
        fn is_terminal_frontmost(&self) -> bool {
            self.terminal_frontmost.get()
        }
        fn has_paste_target(&self) -> bool {
            self.paste_target.get()
        }
        fn set_extra_terminals(&self, extra: Vec<String>) {
            self.set_extra_terminals_calls.borrow_mut().push(extra);
        }
        fn set_extra_custom_text_editors(&self, extra: Vec<String>) {
            self.set_extra_custom_text_editors_calls
                .borrow_mut()
                .push(extra);
        }
    }
    impl CapsKeyMonitor for MockPlatform {
        fn is_caps_physically_down(&self) -> bool {
            self.caps_phys_down.get()
        }
        fn set_caps_lock(&self, on: bool) {
            self.lock_state.set(on);
            if !on {
                self.set_caps_off_calls
                    .set(self.set_caps_off_calls.get() + 1);
            }
        }
        fn is_caps_event_driven(&self) -> bool {
            self.event_driven.get()
        }
        fn drain_caps_events(&self) -> Vec<CapsEdge> {
            self.caps_event_queue.borrow_mut().drain(..).collect()
        }
        fn begin_caps_key_acquisition(&self) -> bool {
            self.acquire_caps_key_calls
                .set(self.acquire_caps_key_calls.get() + 1);
            true
        }
        fn normalize_caps_lock(&self) {
            self.normalize_caps_lock_calls
                .set(self.normalize_caps_lock_calls.get() + 1);
            self.set_caps_lock(false);
        }
        fn release_caps_key(&self) {
            self.release_caps_key_calls
                .set(self.release_caps_key_calls.get() + 1);
        }
    }
    impl Platform for MockPlatform {
        fn preflight(&self) -> Result<(), PreflightError> {
            if self.preflight_denied.get() {
                Err(PreflightError("mock: Accessibility not trusted".into()))
            } else {
                Ok(())
            }
        }
    }

    fn mk(long_press_ms: u64) -> Engine<MockPlatform> {
        let mut d = Engine::new(
            MockPlatform::default(),
            std::path::PathBuf::from("/tmp/ds-test-nonexistent.pid"),
            long_press_ms,
        );
        // Zero the paste-submit delay so submit-path tests can assert
        // `press_enter_calls` synchronously instead of waiting out a real
        // Instant-based timer — mirrors `listener.rs`'s `for_test` doing the same.
        d.cfg.paste_submit_delay_ms = 0;
        d
    }

    /// Arm ClaudeNative's paired toggle and reset the setup taps so assertions measure only
    /// the tested abort or stop.
    fn arm_claude_native(d: &mut Engine<MockPlatform>) {
        d.plat.terminal_frontmost.set(true);
        d.stt.start();
        d.plat.tap_down_calls.set(0);
        d.plat.tap_up_calls.set(0);
    }

    impl MockPlatform {
        /// One physical Caps TAP: a DOWN tick (press) then an UP tick (release), with no
        /// hold in between — the gesture toggles dictation on the RELEASE. The LED is a
        /// pure output, so tests assert on `lock_state` AFTER, never drive it.
        fn tap(d: &mut Engine<MockPlatform>) {
            d.plat.caps_phys_down.set(true);
            d.tick();
            d.plat.caps_phys_down.set(false);
            d.tick();
        }

        /// A physical Caps HOLD past the long-press threshold: press, wait, tick (fires
        /// the cancel), then release (a no-op — the hold consumed the press). Requires a
        /// tiny `long_press_ms` so the sleep is short.
        fn hold(d: &mut Engine<MockPlatform>) {
            d.plat.caps_phys_down.set(true);
            d.tick(); // down edge
            std::thread::sleep(Duration::from_millis(12));
            d.tick(); // past threshold → cancel_all
            d.plat.caps_phys_down.set(false);
            d.tick(); // release: NOT a tap (consumed by the hold)
        }

        /// Queue one edge for the EVENT-DRIVEN feed (`is_caps_event_driven` +
        /// `drain_caps_events`), drained on the next `tick()`. Callers flip
        /// `event_driven` on first so `tick` takes the drain branch instead of
        /// sampling `caps_phys_down`.
        fn queue_event(d: &Engine<MockPlatform>, down: bool, at: Instant) {
            d.plat
                .caps_event_queue
                .borrow_mut()
                .push_back(CapsEdge { down, at });
        }
    }

    /// Age the stop tap's insert-only double-tap window past [`DOUBLE_TAP_MS`] so the
    /// deferred submit stops waiting — tests would otherwise have to sleep out the
    /// real window on every paste.
    fn lapse_stop_window(d: &mut Engine<MockPlatform>) {
        if let GestureState::ConfirmArmed { stop_tap_at, .. } = &mut d.gesture {
            *stop_tap_at = stop_tap_at.map(|t| {
                t.checked_sub(Duration::from_millis(DOUBLE_TAP_MS + 1))
                    .expect("machine uptime exceeds the double-tap window")
            });
        }
    }

    /// Minimal Stt that DEFERS its paste (mirrors the local-transcript helper): start/stop
    /// are no-ops and `defers_paste` is true, so `stop_recording` arms the deferred submit
    /// and the test drives the async final landing by hand.
    struct DeferStt;
    impl ds_stt::Stt for DeferStt {
        fn start(&mut self) -> bool {
            true
        }
        fn stop(&mut self) {}
        fn defers_paste(&self) -> bool {
            true
        }
    }

    #[test]
    fn cancel_all_clears_state_silences_and_drives_led_off() {
        // The HOLD action: discard the active dictation (abort), silence the voice, LED off.
        let mut d = mk(600);
        arm_claude_native(&mut d);
        d.gesture = GestureState::Recording;
        d.plat.lock_state.set(true);
        d.cancel_all();
        assert!(!d.is_recording(), "recording state cleared");
        assert_eq!(
            d.plat.tap_down_calls.get(),
            1,
            "aborted the active dictation (one keypress to end it)"
        );
        assert!(!d.plat.lock_state.get(), "LED driven off");
    }

    #[test]
    fn dictation_preview_gates_the_landed_final_on_caps_held() {
        // Released, finalized transcript landed → surfaced + panel in confirm mode.
        assert_eq!(
            dictation_preview(&FinalState::Ready("hello world".into()), "hel", false),
            ("hello world".to_string(), true)
        );
        // HELD: never surface the finalized transcript (the press might still become a
        // long-press cancel) — show the live partial, NOT in confirm mode (held always
        // wins, whatever the finalize state).
        assert_eq!(
            dictation_preview(&FinalState::Ready("hello world".into()), "hel", true),
            ("hel".to_string(), false)
        );
        // Idle (no final landed, not armed) → the live partial, not in confirm mode.
        assert_eq!(
            dictation_preview(&FinalState::Idle, "part", false),
            ("part".to_string(), false)
        );
        assert_eq!(
            dictation_preview(&FinalState::Idle, "part", true),
            ("part".to_string(), false)
        );
        // The final landed EMPTY while armed: still "awaiting" (the old mirror stayed
        // true between the deposit and the tick's disarm — the row that was only
        // implicit before this enum made it a named state).
        assert_eq!(
            dictation_preview(&FinalState::Empty, "", false),
            (String::new(), true)
        );
    }

    #[test]
    fn dictation_preview_armed_bridges_the_async_final_gap() {
        // The reported flicker: the stop tap has fired (`Armed`) and `recording` already
        // flipped false, but the local engine's async final hasn't landed yet. Without
        // `Armed` counting as awaiting, this reads identically to "not dictating at all"
        // and the panel would hide, then reappear once the final lands. `Armed` keeps
        // `awaiting_confirm` true across the gap, showing the still-fresh partial.
        assert_eq!(
            dictation_preview(&FinalState::Armed, "the last thing you said", false),
            ("the last thing you said".to_string(), true)
        );
        // A press beginning in flight (second tap of a double, or a long-press cancel)
        // still suppresses it — `caps_held` wins over `Armed` too.
        assert_eq!(
            dictation_preview(&FinalState::Armed, "the last thing you said", true),
            ("the last thing you said".to_string(), false)
        );
    }

    #[test]
    fn paste_buf_arm_does_not_downgrade_a_landed_final() {
        // The benign race `arm()` must preserve: the detached joiner can deposit the
        // final in the instants between `stt.stop()` and `stop_recording`'s arm. The
        // old unconditional mirror-set never clobbered `pending`; the enum must not
        // downgrade `Ready(text)` (or `Empty`) back to `Armed` either.
        let mut p = PasteBuf {
            final_state: FinalState::Ready("landed".into()),
            ..Default::default()
        };
        p.arm();
        assert_eq!(p.final_state, FinalState::Ready("landed".into()));
        p.final_state = FinalState::Empty;
        p.arm();
        assert_eq!(
            p.final_state,
            FinalState::Empty,
            "an early empty final must survive the arm so the tick still disarms on it"
        );
        // The normal path: the stop tap arms an idle buffer.
        p.final_state = FinalState::Idle;
        p.arm();
        assert_eq!(p.final_state, FinalState::Armed);
    }

    #[test]
    fn paste_buf_disarm_clears_armed_and_empty_but_leaves_ready() {
        let mut p = PasteBuf {
            final_state: FinalState::Armed,
            ..Default::default()
        };
        p.disarm();
        assert_eq!(p.final_state, FinalState::Idle);
        p.final_state = FinalState::Empty;
        p.disarm();
        assert_eq!(p.final_state, FinalState::Idle);
        // `Ready` is left alone — the strictly-equivalent mapping of the old
        // `disarm_confirm`, which cleared only the mirror bool while a landed
        // `pending` alone kept `awaiting` true (every real call site has already
        // taken/cleared the text first).
        p.final_state = FinalState::Ready("kept".into());
        p.disarm();
        assert_eq!(p.final_state, FinalState::Ready("kept".into()));
    }

    #[test]
    fn caps_held_mirrored_and_suppresses_finalized_transcript() {
        // The reported bug: a transcript is finalized (pending) and the user is HOLDING
        // Caps toward a long-press cancel. The poll loop must mirror the physical held
        // state, and the preview must NOT flash the finalized text while held — so the
        // long-press just dismisses instead of "reappear then dismiss".
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.paste.lock().unwrap().final_state = FinalState::Ready("discard me".into());

        // Physical press (long-press in flight): one tick mirrors the held state.
        d.plat.caps_phys_down.set(true);
        d.tick();
        {
            let p = d.paste.lock().unwrap();
            assert!(p.caps_held, "down edge mirrors caps_held=true");
            let (text, awaiting) = dictation_preview(&p.final_state, &p.partial, p.caps_held);
            assert!(!awaiting, "held: finalized transcript is NOT surfaced");
            assert_eq!(text, "", "held: shows the partial, not the landed final");
        }

        // Release clears the held state (the pending is then revealed/submitted by the
        // confirm path, not flashed mid-press).
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(
            !d.paste.lock().unwrap().caps_held,
            "up edge mirrors caps_held=false"
        );
    }

    #[test]
    fn press_alone_does_not_start_or_light() {
        // THE FIX: a key-DOWN never starts recording and never lights the LED — the
        // gesture is decided on RELEASE (tap) or at the hold threshold (cancel).
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(!d.is_recording(), "press alone does not start recording");
        assert!(
            !d.plat.lock_state.get(),
            "press alone does not light the LED"
        );
        assert_eq!(
            d.plat.tap_down_calls.get(),
            0,
            "no dictation keypress on a press"
        );
    }

    #[test]
    fn tap_starts_dictation_on_release_and_lights() {
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);

        // DOWN edge: still idle, still dark.
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(!d.is_recording(), "not recording until release");
        assert!(!d.plat.lock_state.get(), "dark until release");

        // UP edge (a tap): recording starts and the LED lights — ON RELEASE.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(d.is_recording(), "tap starts dictation on release");
        assert!(d.plat.lock_state.get(), "LED lit on release");
        assert_eq!(d.plat.tap_down_calls.get(), 1, "start posted its keypress");
        assert!(matches!(d.press, PressState::Up), "press latch released");
    }

    #[test]
    fn second_tap_stops_and_extinguishes_on_release_not_press() {
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::tap(&mut d);
        assert!(d.is_recording(), "first tap recording");

        // Second tap's PRESS-DOWN: still recording, LED STAYS lit (no dark-on-press).
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(
            d.is_recording(),
            "still recording while the stop press is held"
        );
        assert!(
            d.plat.lock_state.get(),
            "LED stays lit on the stop press-down"
        );

        // RELEASE: stop + LED off — the light extinguishes on release, not on press.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(!d.is_recording(), "second tap stops dictation on release");
        assert!(!d.plat.lock_state.get(), "LED extinguished on release");
        // The default engine (ClaudeNative) does NOT defer its paste, so the stop
        // lands squarely in `Idle` — never `ConfirmArmed`.
        assert!(
            matches!(d.gesture, GestureState::Idle),
            "non-deferring stop lands Idle, never ConfirmArmed"
        );
    }

    #[test]
    fn press_never_flips_the_light_either_direction() {
        // Idle: the OS momentarily flips the latch ON on a press; we re-assert it OFF.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.plat.lock_state.set(true); // OS toggled the latch ON on key-down
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(
            !d.plat.lock_state.get(),
            "idle press re-asserts the LED OFF"
        );

        // Recording: the OS flips the latch OFF on a press; we re-assert it ON.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::tap(&mut d);
        assert!(
            d.is_recording() && d.plat.lock_state.get(),
            "recording, lit"
        );
        d.plat.lock_state.set(false); // OS toggled the latch OFF on key-down
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(
            d.plat.lock_state.get(),
            "recording press re-asserts the LED ON"
        );
    }

    #[test]
    fn hold_from_idle_cancels_without_recording_or_light() {
        let mut d = mk(5);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::hold(&mut d);
        assert!(!d.is_recording(), "hold never starts recording");
        assert!(!d.plat.lock_state.get(), "hold never lights the LED");
        assert_eq!(
            d.plat.tap_down_calls.get(),
            0,
            "no dictation keypress on a hold"
        );
    }

    #[test]
    fn long_press_cancel_from_idle_keeps_led_off_despite_os_latch_toggle() {
        // macOS's caps-lock hold-delay toggles the OS latch ON partway through a long
        // hold — AFTER the press edge. A long-press cancel from idle must not leave the
        // LED lit out of sync with the recording state; the held-tick re-assert pins it
        // back off.
        let mut d = mk(5);
        d.plat.terminal_frontmost.set(true);

        d.plat.caps_phys_down.set(true);
        d.tick(); // down edge — idle, no light
        assert!(!d.plat.lock_state.get(), "no light on the press");

        // macOS toggles the OS caps-lock latch ON mid-hold.
        d.plat.lock_state.set(true);
        std::thread::sleep(Duration::from_millis(12));
        d.tick(); // past threshold → cancel_all; the held re-assert pins the LED OFF
        assert!(!d.is_recording(), "cancel from idle never records");
        assert!(
            !d.plat.lock_state.get(),
            "LED re-asserted OFF despite the OS toggle"
        );

        // Even if the OS flips it again while still held, the next tick pins it back.
        d.plat.lock_state.set(true);
        d.tick();
        assert!(!d.plat.lock_state.get(), "stays off while held");

        // Release: no toggle; idle with the LED off, fully in sync.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(
            !d.is_recording() && !d.plat.lock_state.get(),
            "idle + LED off after release"
        );
    }

    #[test]
    fn deferred_tap_while_speaking_still_syncs_the_led_on_start_and_stop() {
        // Regression: a Caps tap that lands while TTS is speaking is DEFERRED
        // (`handle_tap` → `TapAction::Defer`) and only resolves later, from
        // `check_pending_tap` on a subsequent tick, once the double-tap window
        // lapses with no second tap. That later `toggle_dictation()` call used to
        // reach `start_recording`/`stop_recording` with NO LED write at all — only
        // the immediate (not-speaking) tap path was covered, via the release-edge
        // snap `apply_caps_edge` no longer has. `start_recording`/`stop_recording`
        // now own their own `sync_caps_led()` call, so both ends must light/
        // extinguish correctly with no release edge in sight.
        let mut d = mk(600);
        // ClaudeCode "delegates" and is always ready (see `stt_ready_to_dictate`'s
        // doc) — avoids the unrelated readiness gate that a `BuiltIn`/`test_stub`
        // combination would hit (the stub's fresh `TtsManager` reports Parakeet not
        // resident, refusing the tap before it ever reaches `start_recording` — see
        // `refused_start_on_but_not_ready_never_pauses_the_voice`).
        d.cfg.stt_engine = Some(vec![ds_config::SttEngine::ClaudeCode]);
        let q = crate::ttsq::TtsQueue::test_stub();
        d.ttsq = Some(q.clone());
        q.set_active_for_test(true); // "speaking"

        // Tap while speaking: deferred, not immediate.
        MockPlatform::tap(&mut d);
        assert!(
            !d.is_recording(),
            "deferred tap does not start recording yet"
        );
        assert!(
            !d.plat.lock_state.get(),
            "LED stays off while the tap is deferred"
        );
        assert!(
            d.pending_tap_at.is_some(),
            "tap parked pending the double-tap window"
        );

        // Age the deferred tap past DOUBLE_TAP_MS with no second tap, then tick:
        // this is the check_pending_tap → toggle_dictation → start_recording path
        // the immediate-tap tests never exercise.
        d.pending_tap_at = d.pending_tap_at.map(|t| {
            t.checked_sub(Duration::from_millis(DOUBLE_TAP_MS + 1))
                .expect("machine uptime exceeds the double-tap window")
        });
        d.tick();
        assert!(
            d.is_recording(),
            "deferred single tap still starts dictation"
        );
        assert!(
            d.plat.lock_state.get(),
            "THE BUG: LED must light even though no release-edge snap ran this tick"
        );

        // Stop tap, ALSO deferred (`tap_decision` only looks at `speaking` and any
        // recent pending tap, not recording state — re-arm "speaking" since
        // `start_recording`'s `pause_for_record()` may have cleared it for real).
        q.set_active_for_test(true);
        MockPlatform::tap(&mut d);
        assert!(d.is_recording(), "stop tap deferred — still recording");
        assert!(
            d.plat.lock_state.get(),
            "LED still lit while the stop tap is deferred"
        );

        d.pending_tap_at = d.pending_tap_at.map(|t| {
            t.checked_sub(Duration::from_millis(DOUBLE_TAP_MS + 1))
                .expect("machine uptime exceeds the double-tap window")
        });
        d.tick();
        assert!(
            !d.is_recording(),
            "deferred single tap still stops dictation"
        );
        assert!(
            !d.plat.lock_state.get(),
            "LED must go dark on a deferred stop too"
        );
    }

    #[test]
    fn event_driven_drains_a_sub_poll_tap_in_one_tick() {
        // The only real `is_caps_event_driven` implementor is the Windows low-level hook
        // (`ds_platform::windows`); every other platform (and, until now, every test)
        // exercises just the polled branch. This pins the OTHER branch in `tick`: a
        // down+up pair that both land inside a single poll gap must be replayed as two
        // real edges from one `tick()` call — the exact case the polled sampler would
        // have missed (a tap faster than the ~30ms poll).
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.plat.event_driven.set(true);

        let t0 = Instant::now();
        MockPlatform::queue_event(&d, true, t0);
        MockPlatform::queue_event(&d, false, t0 + Duration::from_millis(5));
        // The event-driven port suppresses the OS toggle and reports the settled
        // (released) physical state, exactly like the real Windows hook post-tap.
        d.plat.caps_phys_down.set(false);

        d.tick();

        assert!(
            d.is_recording(),
            "a down+up drained in one tick still starts dictation — the tap isn't dropped"
        );
        assert!(
            d.plat.lock_state.get(),
            "LED lights on the drained release edge"
        );
        assert_eq!(
            d.plat.tap_down_calls.get(),
            1,
            "start posted its keypress from the drained pair"
        );
        assert!(
            matches!(d.press, PressState::Up),
            "press latch released after the drained up edge"
        );
        assert!(
            !d.caps_phys_prev,
            "the polled mirror still tracks the live latched state on the event-driven path"
        );

        // A second down+up pair, again drained inside a single tick, stops the
        // recording — same outcome as the polled two-tick `MockPlatform::tap`.
        let t1 = Instant::now();
        MockPlatform::queue_event(&d, true, t1);
        MockPlatform::queue_event(&d, false, t1 + Duration::from_millis(5));
        d.tick();

        assert!(!d.is_recording(), "second drained tap stops dictation");
        assert!(
            !d.plat.lock_state.get(),
            "LED off after the second drained tap"
        );
        assert!(
            !d.caps_phys_prev,
            "polled mirror still coherent after the second drained pair"
        );
    }

    #[test]
    fn hold_while_recording_discards_and_release_does_not_re_toggle() {
        let mut d = mk(5);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::tap(&mut d); // start recording
        assert!(d.is_recording());
        let starts = d.plat.tap_down_calls.get();

        // Hold past the threshold → discard (abort the listen), LED off.
        d.plat.caps_phys_down.set(true);
        d.tick();
        std::thread::sleep(Duration::from_millis(12));
        d.tick();
        assert!(!d.is_recording(), "hold discards the active dictation");
        assert!(!d.plat.lock_state.get(), "LED off after a discard");
        assert_eq!(
            d.plat.tap_down_calls.get(),
            starts + 1,
            "aborted the listen once (ClaudeNative abort→stop = one keypress)"
        );

        // Extra polls past the threshold must NOT re-fire the cancel.
        d.tick();
        d.tick();
        assert_eq!(
            d.plat.tap_down_calls.get(),
            starts + 1,
            "cancel fires exactly once per press"
        );

        // The release that ENDS the hold is NOT a tap.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(!d.is_recording(), "release after a hold stays idle");
        assert!(
            !d.plat.lock_state.get(),
            "still dark after the hold release"
        );
        assert!(
            matches!(d.press, PressState::Up),
            "long-press release leaves the press latch Up (tap suppressed, nothing lingers)"
        );
    }

    #[test]
    fn deferred_submit_pastes_when_async_final_lands() {
        // The local-transcript path: the stop tap arms a deferred submit; the engine
        // deposits the FINAL asynchronously, and the poll loop pastes once it lands.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.plat.paste_target.set(true);
        d.stt = Box::new(DeferStt);

        MockPlatform::tap(&mut d); // start
        assert!(d.is_recording(), "recording");
        MockPlatform::tap(&mut d); // stop — defers
        assert!(!d.is_recording(), "stopped on release");
        assert!(d.is_confirm_armed(), "deferred submit armed");
        assert!(
            !d.plat.lock_state.get(),
            "LED already off on the stop release"
        );
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "nothing pasted while the final is pending"
        );

        // The async final lands: deposit it, then (once the insert-only double-tap
        // window has lapsed) tick → paste + submit.
        d.paste.lock().unwrap().final_state = FinalState::Ready("hello world".into());
        lapse_stop_window(&mut d);
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            1,
            "pasted once the final landed"
        );
        assert_eq!(
            d.plat.press_enter_calls.get(),
            1,
            "single tap → Enter pressed"
        );
        assert!(!d.is_confirm_armed(), "disarmed after the deferred submit");
    }

    #[test]
    fn frontmost_terminal_is_a_paste_target_even_without_ax_focus() {
        // Regression: the "no target" glow used only the AX focused-element probe
        // (`has_paste_target`). Terminals — the app's main dictation target —
        // often don't expose an AX-settable editable element, so the probe read
        // false and the bar glowed orange even though a Cmd+V paste lands fine.
        // A frontmost terminal must itself count as a paste target.
        let mut d = mk(600);
        // Panel up: the live probe only runs while recording or awaiting confirm.
        d.stt_active = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )));

        // AX probe blind (no editable field exposed), but a terminal IS frontmost.
        d.plat.paste_target.set(false);
        d.plat.terminal_frontmost.set(true);
        d.tick();
        assert!(
            d.paste.lock().unwrap().has_paste_target,
            "frontmost terminal is a paste target even when the AX probe sees no editable field"
        );

        // Neither signal: genuinely nowhere to paste → glow on.
        d.plat.terminal_frontmost.set(false);
        d.tick();
        assert!(
            !d.paste.lock().unwrap().has_paste_target,
            "no editable field and no terminal ⇒ no paste target"
        );

        // A focused editable field in a non-terminal app still counts on its own.
        d.plat.paste_target.set(true);
        d.tick();
        assert!(
            d.paste.lock().unwrap().has_paste_target,
            "a focused editable field is a paste target on its own"
        );
    }

    #[test]
    fn deferred_empty_final_disarms_without_pasting() {
        // The deferred final comes back EMPTY (silence): disarm, paste nothing.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.stt = Box::new(DeferStt);
        MockPlatform::tap(&mut d);
        MockPlatform::tap(&mut d);
        assert!(d.is_confirm_armed(), "armed");

        d.paste.lock().unwrap().final_state = FinalState::Empty; // the final landed empty
        lapse_stop_window(&mut d);
        d.tick();
        assert!(!d.is_confirm_armed(), "disarmed on an empty final");
        assert!(
            matches!(d.paste.lock().unwrap().final_state, FinalState::Idle),
            "the PasteBuf mirror must clear too, or the dictation bar stays stuck showing 'awaiting'"
        );
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "nothing pasted for an empty final"
        );
    }

    /// Arm the deferred submit via start+stop, optionally landing a SECOND tap inside
    /// the double-tap window before returning — the setup shared by the four
    /// lone-tap/double-tap × default/inverted `double_tap_submits` tests below, so the
    /// arm+tap choreography can't drift between the two config directions.
    fn arm_stop_tap(double_tap_submits: bool, second_tap: bool) -> Engine<MockPlatform> {
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.stt = Box::new(DeferStt);
        d.cfg.double_tap_submits = double_tap_submits;
        MockPlatform::tap(&mut d); // start
        MockPlatform::tap(&mut d); // stop — arms the deferred submit
        if second_tap {
            MockPlatform::tap(&mut d); // second tap inside the window — flips the outcome
        }
        d
    }

    /// Deposit the async final the deferred submit (from [`arm_stop_tap`]) is waiting on.
    fn deposit_final(d: &Engine<MockPlatform>, text: &str) {
        d.paste.lock().unwrap().final_state = FinalState::Ready(text.into());
    }

    #[test]
    fn deferred_submit_waits_out_the_stop_double_tap_window() {
        // A FAST final (landing inside DOUBLE_TAP_MS of the stop tap) must NOT
        // paste+Enter before the flip gesture has a chance to land: the tick holds the
        // paste until the window lapses.
        let mut d = arm_stop_tap(false, false);
        deposit_final(&d, "fast final");
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "paste held while the double-tap window is open"
        );
        lapse_stop_window(&mut d);
        d.tick();
        assert_eq!(d.plat.type_text_calls.get(), 1, "pasted once it lapsed");
        assert_eq!(d.plat.press_enter_calls.get(), 1, "single tap → auto-Enter");
    }

    #[test]
    fn double_tap_on_stop_pastes_without_enter() {
        // The stop tap, then a SECOND tap inside the window → INSERT-ONLY: the
        // transcript pastes but the auto-Enter is suppressed for this one submit.
        let mut d = arm_stop_tap(false, true);
        assert!(
            matches!(
                d.gesture,
                GestureState::ConfirmArmed {
                    enter_after_paste: false,
                    ..
                }
            ),
            "double tap latched insert-only"
        );
        assert!(
            !d.is_recording(),
            "the second tap did NOT start a new recording"
        );
        deposit_final(&d, "no enter");
        d.tick();
        assert_eq!(d.plat.type_text_calls.get(), 1, "pasted");
        assert_eq!(
            d.plat.press_enter_calls.get(),
            0,
            "no Enter on the insert-only double tap"
        );
        // The old assert here ("insert-only consumed — the next dictation submits
        // normally") checked `enter_after_paste` reset to its default; that reset is
        // now STRUCTURAL — the flag lives inside `ConfirmArmed`, which the paste drops.
        assert!(
            matches!(d.gesture, GestureState::Idle),
            "disarmed after the paste (insert-only consumed with the variant)"
        );
    }

    #[test]
    fn double_tap_submits_inverts_a_lone_tap_to_insert_only() {
        // With double_tap_submits ON, a LONE stop tap (no second tap lands) is
        // insert-only — the opposite of the default gesture. Also proves the fast-final
        // race is closed: the paste must still HOLD out the double-tap window even
        // though the armed outcome starts as insert-only (see `deferred_submit_held`).
        let mut d = arm_stop_tap(true, false);
        assert!(
            matches!(
                d.gesture,
                GestureState::ConfirmArmed {
                    enter_after_paste: false,
                    ..
                }
            ),
            "armed insert-only when inverted"
        );
        deposit_final(&d, "lone tap, inverted");
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "paste held while the double-tap window is open, even though armed false"
        );
        lapse_stop_window(&mut d);
        d.tick();
        assert_eq!(d.plat.type_text_calls.get(), 1, "pasted once it lapsed");
        assert_eq!(
            d.plat.press_enter_calls.get(),
            0,
            "lone tap → insert only when double_tap_submits is on"
        );
    }

    #[test]
    fn double_tap_submits_inverts_a_double_tap_to_submit() {
        // With double_tap_submits ON, a genuine double tap on the stop gesture SUBMITS
        // (paste + Enter) instead of the default insert-only.
        let mut d = arm_stop_tap(true, true);
        assert!(
            matches!(
                d.gesture,
                GestureState::ConfirmArmed {
                    enter_after_paste: true,
                    ..
                }
            ),
            "double tap flipped to submit"
        );
        assert!(
            !d.is_recording(),
            "the second tap did NOT start a new recording"
        );
        deposit_final(&d, "double tap, inverted");
        d.tick();
        assert_eq!(d.plat.type_text_calls.get(), 1, "pasted");
        assert_eq!(
            d.plat.press_enter_calls.get(),
            1,
            "double tap submits when double_tap_submits is on"
        );
    }

    #[test]
    fn hold_after_stop_cancels_the_pending_paste() {
        // Stop, then a HOLD (long-press) before the final pastes → the universal
        // CANCEL discards the pending transcript instead of submitting it.
        let mut d = mk(10); // tiny long-press so the test hold is short
        d.plat.terminal_frontmost.set(true);
        d.stt = Box::new(DeferStt);
        MockPlatform::tap(&mut d); // start
        MockPlatform::tap(&mut d); // stop — deferred submit armed
        d.paste.lock().unwrap().final_state = FinalState::Ready("discard me".into());
        MockPlatform::hold(&mut d); // long-press → cancel_all
        assert!(!d.is_confirm_armed(), "cancel disarmed the deferred submit");
        assert!(
            matches!(d.paste.lock().unwrap().final_state, FinalState::Idle),
            "cancel also clears the panel-visibility mirror — the panel must not \
             stay up or reappear after a long-press cancel"
        );
        lapse_stop_window(&mut d); // no-op (cancel cleared the window) — belt & braces
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "cancel discarded the pending transcript — nothing pasted"
        );
        assert_eq!(d.plat.press_enter_calls.get(), 0, "and no Enter");
    }

    #[test]
    fn double_tap_on_stop_consumes_the_second_tap_and_preserves_the_transcript() {
        // The second tap of the stop gesture must always be CONSUMED — never falls
        // through to start a new recording — regardless of what it does to the
        // submit outcome. Falling through would silently destroy the still-unpasted
        // transcript.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.stt = Box::new(DeferStt);
        MockPlatform::tap(&mut d); // start
        MockPlatform::tap(&mut d); // stop
        MockPlatform::tap(&mut d); // double tap (µs later — inside the window)
        assert!(
            !d.is_recording(),
            "the second tap did NOT start a new recording"
        );
        d.paste.lock().unwrap().final_state = FinalState::Ready("kept".into());
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            1,
            "transcript pasted, not destroyed"
        );
        assert_eq!(
            d.plat.press_enter_calls.get(),
            0,
            "double tap on stop (default double_tap_submits=false) is insert-only"
        );
    }

    #[test]
    fn tap_after_the_window_starts_a_new_recording_and_drops_the_paste() {
        // A tap AFTER the double-tap window is a NEW dictation, not a late double —
        // and the deferred submit never fires mid-press: the pending transcript is
        // deliberately wiped by the new session, never pasted under the press.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.stt = Box::new(DeferStt);
        MockPlatform::tap(&mut d); // start
        MockPlatform::tap(&mut d); // stop — armed
        d.paste.lock().unwrap().final_state = FinalState::Ready("late".into());
        lapse_stop_window(&mut d); // the double-tap window is long gone
        d.plat.caps_phys_down.set(true);
        d.tick(); // press in flight → the deferred submit is HELD, not pasted
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "no paste under an in-flight press"
        );
        d.plat.caps_phys_down.set(false);
        d.tick(); // release → a plain tap → new recording (wipes the pending transcript)
        assert!(
            d.is_recording(),
            "tap after the window starts a new recording"
        );
        assert!(
            matches!(d.paste.lock().unwrap().final_state, FinalState::Idle),
            "the unpasted transcript is dropped by the new session"
        );
        d.tick();
        assert_eq!(d.plat.type_text_calls.get(), 0, "never pasted");
        assert_eq!(d.plat.press_enter_calls.get(), 0, "never submitted");
    }

    // ── §E.4 Engine::reload over MockPlatform ───────────────────────────────

    #[test]
    fn reload_clears_state_aborts_inflight_and_drives_led_off() {
        let mut d = mk(600);
        arm_claude_native(&mut d);
        // Simulate an in-flight dictation on the outgoing engine.
        d.gesture = GestureState::Recording;
        // Ignore the acquisition-time OFF normalization; this assertion measures only
        // the engine-changing reload below.
        d.plat.set_caps_off_calls.set(0);

        // Reload to a config that CHANGES the RESOLVED STT engine, forcing a rebuild (the
        // surgical reload only aborts + swaps when the engine actually changes). Disabling
        // dictation (empty ladder) flips the resolved engine on EVERY platform — unlike
        // naming a specific engine, which on a machine whose default ladder already resolves
        // to that engine would be a no-op.
        let cfg = VoiceConfig {
            stt_engine_ladder: Vec::new(),
            ..Default::default()
        };
        d.reload(&cfg);

        // The outgoing in-flight HOLD was released via abort() (ClaudeNative
        // abort == ctrl_g_up); the new engine starts from idle.
        assert!(
            !d.is_recording(),
            "recording cleared after engine-changing reload"
        );
        assert_eq!(
            d.plat.tap_up_calls.get(),
            1,
            "in-flight HOLD released via engine abort"
        );
        // Reload DOES drive the LED off now, via `teardown_hold`'s `sync_caps_led()`:
        // an internal reason for ending dictation (here, an engine-changing reload)
        // is still the dictation ending, and the LED must not lie that it's still
        // recording when nothing physically released the key to correct it later.
        // This does not fabricate a spurious tap — gesture detection is edge-based
        // on the physical key only and never reads the LED back.
        assert_eq!(
            d.plat.set_caps_off_calls.get(),
            1,
            "reload must drive the Caps LED off when it ends an in-flight recording"
        );
    }

    #[test]
    fn reload_noop_change_preserves_inflight_hold() {
        // Surgical reload: a change that touches only per-call params (here the
        // voice id) must NOT rebuild the engine or interrupt an in-flight HOLD.
        let mut d = mk(600);
        d.gesture = GestureState::Recording;

        let cfg = VoiceConfig {
            tts_built_in_voices: vec!["am_michael".into()],
            ..Default::default()
        };
        d.reload(&cfg);

        assert!(
            d.is_recording(),
            "a per-call-only change must not drop the HOLD"
        );
        assert_eq!(
            d.plat.tap_up_calls.get(),
            0,
            "no abort on a no-op (per-call) reload"
        );
        assert_eq!(
            d.cfg.current_voice(),
            "am_michael",
            "new config recorded for next diff"
        );
    }

    #[test]
    fn reload_caps_toggle_off_ends_hold() {
        // Flipping caps_enabled OFF mid-hold must end the HOLD cleanly (abort).
        let mut d = mk(600);
        arm_claude_native(&mut d);
        d.gesture = GestureState::Recording;
        let cfg = VoiceConfig {
            caps_enabled: false,
            ..Default::default()
        };
        d.reload(&cfg);
        assert!(!d.caps_enabled, "caps loop disabled");
        assert!(!d.is_recording(), "HOLD ended when caps disabled mid-hold");
        assert_eq!(d.plat.tap_up_calls.get(), 1, "in-flight HOLD released");
    }

    #[test]
    fn reload_caps_toggle_bumps_status_gate_only_on_real_transition() {
        // set_caps_gate must bump the status-push gate on a REAL caps-enabled
        // transition — so the app's dot updates live across e.g. an Accessibility
        // grant flipping `refresh_caps_gate`'s re-probe (the bug this closes: the
        // backend value updated instantly but a blocked WaitModelStatus never woke,
        // so the app's dot stayed stale until an unrelated status change or a
        // relaunch). It must NOT bump on a no-op reload, since `reload` calls
        // `set_caps_gate` unconditionally on every config apply.
        let mut d = mk(600);
        let gate = StatusGate::new();
        d.status_gate = Some(gate.clone());
        d.caps_active = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
        d.caps_enabled = false; // known baseline, set directly (no bump fired yet)

        let on_cfg = VoiceConfig {
            caps_enabled: true,
            ..Default::default()
        };
        d.reload(&on_cfg);
        assert!(d.caps_enabled, "caps loop turned on");
        let seq_after_on = gate.seq();
        assert_ne!(seq_after_on, 0, "a real OFF->ON transition bumps the gate");

        // A second reload with the SAME caps_enabled value must not bump again.
        d.reload(&on_cfg);
        assert_eq!(
            gate.seq(),
            seq_after_on,
            "an unchanged caps state must not bump the gate again"
        );

        // ON->OFF is a real transition too.
        let off_cfg = VoiceConfig {
            caps_enabled: false,
            ..Default::default()
        };
        d.reload(&off_cfg);
        assert!(!d.caps_enabled, "caps loop turned off");
        assert_ne!(
            gate.seq(),
            seq_after_on,
            "a real ON->OFF transition bumps the gate again"
        );
    }

    #[test]
    fn assemble_acquires_the_caps_key_only_when_starting_enabled() {
        // `mk`/`Engine::new` start with caps enabled (preflight Ok, default config) —
        // construction must acquire exactly once, never release (nothing to release yet).
        let d = mk(600);
        assert!(d.caps_enabled, "starts enabled by default");
        assert_eq!(
            d.plat.acquire_caps_key_calls.get(),
            1,
            "acquired once at construction"
        );
        assert_eq!(
            d.plat.normalize_caps_lock_calls.get(),
            1,
            "normalized once at construction"
        );
        assert_eq!(
            d.plat.release_caps_key_calls.get(),
            0,
            "nothing to release at construction"
        );
    }

    #[test]
    fn assemble_does_not_acquire_the_caps_key_when_starting_disabled() {
        // Denied preflight (mirrors AX not yet trusted) must never install the platform's
        // key suppression at all — the bug this whole fix closes for the STARTUP path.
        let plat = MockPlatform {
            preflight_denied: Cell::new(true),
            ..Default::default()
        };
        let d = Engine::new(
            plat,
            std::path::PathBuf::from("/tmp/ds-test-nonexistent.pid"),
            600,
        );
        assert!(!d.caps_enabled, "preflight denied — caps loop starts off");
        assert_eq!(
            d.plat.acquire_caps_key_calls.get(),
            0,
            "must never acquire when starting disabled"
        );
        assert_eq!(d.plat.normalize_caps_lock_calls.get(), 0);
        assert_eq!(d.plat.release_caps_key_calls.get(), 0);
    }

    #[test]
    fn reload_caps_toggle_acquires_and_releases_the_physical_key_on_real_transitions_only() {
        // The core regression this diff closes: OFF must actually release ownership
        // (restoring native OS behavior / discarding any backlog), and ON must
        // re-acquire it — but only on REAL transitions, matching the existing
        // status-gate test's "no-op reload doesn't re-bump" contract.
        let mut d = mk(600);
        assert_eq!(
            d.plat.acquire_caps_key_calls.get(),
            1,
            "acquired at construction"
        );

        let off_cfg = VoiceConfig {
            caps_enabled: false,
            ..Default::default()
        };
        d.reload(&off_cfg);
        assert!(!d.caps_enabled);
        assert_eq!(
            d.plat.release_caps_key_calls.get(),
            1,
            "OFF releases the key"
        );
        assert_eq!(
            d.plat.acquire_caps_key_calls.get(),
            1,
            "still just the one from construction"
        );

        // A second OFF reload is a no-op transition — must not release again.
        d.reload(&off_cfg);
        assert_eq!(
            d.plat.release_caps_key_calls.get(),
            1,
            "unchanged OFF doesn't re-release"
        );

        let on_cfg = VoiceConfig {
            caps_enabled: true,
            ..Default::default()
        };
        d.reload(&on_cfg);
        assert!(d.caps_enabled);
        assert_eq!(
            d.plat.acquire_caps_key_calls.get(),
            2,
            "ON->reload re-acquires"
        );
        assert_eq!(
            d.plat.normalize_caps_lock_calls.get(),
            2,
            "startup and OFF->ON acquisition both normalize Caps Lock"
        );
        assert_eq!(
            d.plat.release_caps_key_calls.get(),
            1,
            "unchanged from the OFF above"
        );
    }

    #[test]
    fn caps_gate_off_clears_a_stale_in_flight_press_latch_and_forces_the_led_off() {
        // A physical press straddling the OFF edge must not leave the press latch
        // stale: release_caps_key resets the platform's own press tracking (on Windows,
        // it wipes the edge queue, so the matching release becomes unobservable), so
        // without this the first tick after a LATER re-enable would read a huge elapsed
        // time and fire a spurious long-press `cancel_all()`. The LED must also be
        // forced off here — unlike `cancel_all`/the confirm-paste path, nothing else on
        // this route drives it low.
        let mut d = mk(600);
        d.press = PressState::Down {
            since: Instant::now(),
            long_press_fired: false,
        };
        d.plat.set_caps_off_calls.set(0);
        let off_cfg = VoiceConfig {
            caps_enabled: false,
            ..Default::default()
        };
        d.reload(&off_cfg);
        assert!(
            matches!(d.press, PressState::Up),
            "stale in-flight press latch cleared on release"
        );
        assert_eq!(
            d.plat.set_caps_off_calls.get(),
            1,
            "LED forced off when the key is released"
        );
    }

    #[test]
    fn tick_discards_a_queued_backlog_while_always_listen_mode_bypasses_the_gesture() {
        // Always-listen mode and `caps_enabled` are independent axes — the platform can
        // still be event-driven and queuing physical presses (e.g. Windows, still
        // acquired because `caps_enabled` is true) while Always mode bypasses the
        // gesture entirely. Without discarding, that backlog would replay in a burst
        // the instant `listen_mode` switches back, corrupting the tap/double-tap state
        // machine exactly like the bug `release_caps_key` otherwise closes.
        let mut d = mk(600);
        d.plat.event_driven.set(true);
        d.cfg.listen_mode = ds_config::ListenMode::Always;
        d.plat.caps_event_queue.borrow_mut().push_back(CapsEdge {
            down: true,
            at: Instant::now(),
        });
        d.plat.caps_event_queue.borrow_mut().push_back(CapsEdge {
            down: false,
            at: Instant::now(),
        });
        d.tick();
        assert!(
            d.plat.caps_event_queue.borrow().is_empty(),
            "queued edges must be drained (and discarded) even while bypassed by Always mode"
        );
        assert!(
            !d.is_recording(),
            "the discarded backlog must never be applied as a real gesture"
        );
    }

    #[test]
    fn reload_engine_swap_mid_hold_clears_recording_icon() {
        // A reload that SWAPS the STT engine mid-dictation must reset the published
        // `stt_active` (the menu-bar recording icon) + the preview buffer — not leave the
        // icon stuck "recording" with no live listen on the fresh engine.
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        d.stt_active = Some(active.clone());
        d.gesture = GestureState::Recording;
        d.paste.lock().unwrap().final_state = FinalState::Ready("stale".into());

        // Disabling dictation (empty ladder) flips the resolved engine → stt_changed → rebuilt
        // (platform-independent, unlike naming an engine the default may already resolve to).
        let cfg = VoiceConfig {
            stt_engine_ladder: Vec::new(),
            ..Default::default()
        };
        d.reload(&cfg);
        assert!(!d.is_recording(), "hold ended on engine swap");
        assert!(
            !active.load(Ordering::Relaxed),
            "recording icon (stt_active) cleared on the swap"
        );
        assert!(
            matches!(d.paste.lock().unwrap().final_state, FinalState::Idle),
            "stale preview cleared"
        );
    }

    #[test]
    fn reload_applies_long_press_and_normalizes_zero() {
        let mut d = mk(600);
        // A config with an explicit long_press_ms takes effect.
        let cfg = VoiceConfig {
            long_press_ms: 900,
            ..Default::default()
        };
        d.reload(&cfg);
        assert_eq!(d.long_press_ms, 900, "explicit long_press applied");

        // long_press_ms = 0 normalizes to the default on reload (same as startup).
        let cfg0 = VoiceConfig {
            long_press_ms: 0,
            ..Default::default()
        };
        d.reload(&cfg0);
        assert_eq!(
            d.long_press_ms, DEFAULT_LONG_PRESS_MS,
            "zero long_press normalizes to default on reload"
        );
    }

    #[test]
    fn reload_pushes_extra_paste_target_lists_to_the_platform() {
        let mut d = mk(600);
        // assemble() already pushed once, with the default (empty) config.
        assert_eq!(
            d.plat.set_extra_terminals_calls.borrow().as_slice(),
            &[Vec::<String>::new()]
        );
        assert_eq!(
            d.plat
                .set_extra_custom_text_editors_calls
                .borrow()
                .as_slice(),
            &[Vec::<String>::new()]
        );

        let cfg = VoiceConfig {
            extra_terminals: vec!["myterm".into()],
            extra_custom_text_editors: vec!["myeditor.exe".into()],
            ..Default::default()
        };
        d.reload(&cfg);
        assert_eq!(
            d.plat.set_extra_terminals_calls.borrow().last(),
            Some(&vec!["myterm".to_string()])
        );
        assert_eq!(
            d.plat.set_extra_custom_text_editors_calls.borrow().last(),
            Some(&vec!["myeditor.exe".to_string()])
        );
    }

    #[test]
    fn reload_while_idle_does_not_abort() {
        // A reload when NOT recording must not call abort() (nothing in flight).
        let mut d = mk(600);
        // Ignore the acquisition-time OFF normalization; this assertion measures only
        // the no-op reload below.
        d.plat.set_caps_off_calls.set(0);
        assert!(!d.is_recording());
        d.reload(&VoiceConfig::default());
        assert_eq!(
            d.plat.tap_up_calls.get(),
            0,
            "idle reload does not release a key"
        );
        assert_eq!(
            d.plat.set_caps_off_calls.get(),
            0,
            "idle reload no LED drive"
        );
    }

    #[test]
    fn refresh_caps_gate_reacts_to_live_ax_trust_changes() {
        // `refresh_caps_gate` (the periodic Accessibility re-probe) previously had zero
        // coverage: `MockPlatform::preflight` always returned `Ok(())`, so its one live
        // branch — an actual trust FLIP — never ran. Drive both edges with the now-
        // settable `preflight_denied`, and assert both the gate flag AND the shared
        // `caps_active`/`status_gate` publications it drives.
        let mut d = mk(600);
        let caps_active = Arc::new(AtomicBool::new(true));
        d.caps_active = Some(caps_active.clone());
        let gate = StatusGate::new();
        d.status_gate = Some(gate.clone());
        assert!(d.caps_enabled, "starts enabled — preflight Ok by default");

        // AX trust revoked: the re-probe must flip the gate off live, without a reload.
        d.plat.preflight_denied.set(true);
        let seq0 = gate.seq();
        d.refresh_caps_gate();
        assert!(!d.caps_enabled, "AX revoked → caps disabled live");
        assert!(
            !caps_active.load(Ordering::Relaxed),
            "shared caps_active (RPC running.caps) flipped off"
        );
        assert_ne!(gate.seq(), seq0, "status gate bumped on the OFF transition");

        // A re-probe with no actual change must NOT bump again (only real transitions push).
        let seq1 = gate.seq();
        d.refresh_caps_gate();
        assert_eq!(gate.seq(), seq1, "no-op re-probe does not bump the gate");

        // AX trust regranted: the re-probe flips back on live.
        d.plat.preflight_denied.set(false);
        d.refresh_caps_gate();
        assert!(d.caps_enabled, "AX regranted → caps re-enabled live");
        assert!(
            caps_active.load(Ordering::Relaxed),
            "shared caps_active flipped back on"
        );
        assert_ne!(gate.seq(), seq1, "status gate bumped on the ON transition");
    }

    #[test]
    fn reload_always_mode_lifecycle_builds_bypasses_caps_and_tears_down_on_exit() {
        // Always-listening had zero coverage: no test ever set `listen_mode = Always`, so
        // neither the `self.listener` build/drop lifecycle in `reload` nor the tick-dispatch
        // bypass of the Caps PTT gesture machine ran anywhere in the crate.
        let mut d = mk(600);
        assert!(
            d.listener.is_none(),
            "starts without a listener (RecordSubmit is the default mode)"
        );

        // An in-flight PTT hold before the mode flips to Always must be torn down — Always
        // bypasses the Caps gesture machine entirely, so a stranded HOLD would sit in stasis
        // and paste a stale transcript once the mode later flips back.
        d.gesture = GestureState::Recording;

        let cfg = VoiceConfig {
            listen_mode: ds_config::ListenMode::Always,
            ..Default::default()
        };
        d.reload(&cfg);
        assert!(
            d.listener.is_some(),
            "entering Always mode builds the listener"
        );
        assert!(
            !d.is_recording(),
            "in-flight PTT hold torn down when Always takes over"
        );

        // tick() while Always is active must dispatch to the listener and bypass the Caps
        // PTT gesture machine entirely — even with the physical key held.
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(
            !d.caps_phys_prev,
            "caps_phys_prev untouched: the PTT sampling branch never runs while Always is active"
        );
        assert_eq!(
            d.plat.tap_down_calls.get(),
            0,
            "the Caps PTT chord never fires while Always is active"
        );
        d.plat.caps_phys_down.set(false);

        // Simulate a stray hold present when Always is left (same hazard as the caps gate
        // going off mid-hold): leaving the mode must tear it down too.
        d.gesture = GestureState::Recording;

        d.reload(&VoiceConfig::default());
        assert!(
            d.listener.is_none(),
            "leaving Always mode drops the listener"
        );
        assert!(
            !d.is_recording(),
            "leaving Always tears down any in-flight hold"
        );
    }

    #[test]
    fn reload_always_mode_rebuilds_the_listener_on_a_live_stt_provider_switch() {
        // Finding #5's acceptance criterion — "the selected STT provider is honored... in
        // always-listening mode" — only held at CONSTRUCTION: `Listener::new` resolves its
        // provider once (`helper_stt_provider`), and nothing rebuilt it on a LATER provider
        // switch while Always mode stayed running. `local_avail_flipped` doesn't catch this
        // case: Parakeet presence stays false→false on both sides in this test host (no real
        // model files), same as it would stay true→true on a real host where both the old
        // and new provider are already locally usable. Only `change.stt_changed` sees the
        // switch, so `reload`'s rebuild condition must include it too.
        let mut d = mk(600);
        // Only mutated in the provider-switch block below, which is skipped on Intel macOS.
        #[cfg_attr(
            all(target_os = "macos", not(target_arch = "aarch64")),
            allow(unused_mut)
        )]
        let mut cfg = VoiceConfig {
            listen_mode: ds_config::ListenMode::Always,
            stt_engine: Some(vec![ds_config::SttEngine::BuiltIn]),
            provider: vec![ds_config::Provider::OrtCpu],
            ..Default::default()
        };
        d.reload(&cfg);
        assert_eq!(
            d.listener
                .as_ref()
                .expect("Always mode builds a listener")
                .provider(),
            "cpu"
        );

        // Provider preference only — same engine selection, so this isolates
        // `change.stt_changed` as the one thing that differs from the first reload. `OrtCuda`
        // is a no-op ladder switch on macOS (`Provider::stt_usable_on` gates it to
        // windows/linux, so it silently re-resolves to `OrtCpu` there — caught on real macOS
        // hardware, see #31); `Ane` is the macOS-genuine transition instead (gated to
        // macOS+aarch64, which is this crate's macOS CI/dev floor). On Intel macOS neither
        // rung is STT-usable (`ane_usable_on` is arm64-only, see ds-config), so there is no
        // other provider to switch to — skip there rather than assert a transition that can't
        // happen, matching how ds-config's own `provider_resolution_walks_the_ladder_per_platform`
        // test gates its Apple-Silicon-only assertions.
        #[cfg(any(not(target_os = "macos"), target_arch = "aarch64"))]
        {
            #[cfg(target_os = "macos")]
            let (new_provider, want_token) = (ds_config::Provider::Ane, "ane");
            #[cfg(not(target_os = "macos"))]
            let (new_provider, want_token) = (ds_config::Provider::OrtCuda, "cuda");
            cfg.provider = vec![new_provider];
            assert_ne!(
                cfg.resolved_stt_provider(),
                d.cfg.resolved_stt_provider(),
                "test setup must force a real provider transition"
            );
            d.reload(&cfg);
            assert!(
                d.listener.is_some(),
                "Always mode stays on across the reload"
            );
            assert_eq!(
                d.listener.as_ref().unwrap().provider(),
                want_token,
                "a live provider switch while Always-listening is running must rebuild the \
                 listener, not keep serving the stale provider"
            );
        }
    }

    #[test]
    fn reload_claude_code_stt_reads_keybindings_from_injected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        std::fs::create_dir_all(paths.settings_json.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.settings_json,
            r#"{"voice": {"enabled": true, "mode": "tap"}}"#,
        )
        .unwrap();
        std::fs::write(
            &paths.keybindings_json,
            r#"{"bindings": [{"context": "Chat", "bindings": {"ctrl+g": "voice:pushToTalk"}}]}"#,
        )
        .unwrap();

        let mut d = mk(600);
        d.paths = Some(paths);
        let mut cfg = d.cfg.clone();
        cfg.stt_engine = Some(vec![ds_config::SttEngine::ClaudeCode]);
        // Sanity: this reload must actually enter the STT-rebuild branch, else the
        // assertion below would pass vacuously without exercising the fix.
        assert_ne!(
            cfg.resolved_stt(),
            d.cfg.resolved_stt(),
            "test setup must force a real STT transition"
        );
        d.reload(&cfg);

        assert_eq!(d.stt.kind(), "claude_code");
        d.plat.terminal_frontmost.set(true);
        d.stt.start();
        let taps = d.plat.tapped_chords.borrow();
        assert_eq!(
            taps.last(),
            Some(&ds_platform::KeyChord::parse("ctrl+g")),
            "the claude_code engine built via reload must tap the chord read from the INJECTED Paths"
        );
    }

    #[test]
    fn reload_claude_code_stt_falls_back_to_default_chord_without_injected_paths() {
        // Proves the OTHER direction: with paths left at None (no injection), the same
        // ClaudeCode selection builds the DEFAULT chord, not whatever a stray real
        // ~/.claude/keybindings.json on the test host happens to contain. Uses a SEPARATE
        // engine (paths never set to Some on this instance) rather than flipping the same
        // instance's `paths` back to None and reloading again — reload's STT-rebuild branch
        // is gated on `change.stt_changed || local_avail_flipped`, so a second reload with
        // an IDENTICAL resolved engine would never re-enter the rebuild branch and the
        // still-live first instance's engine would be asserted on instead, silently testing
        // nothing.
        let mut d2 = mk(600);
        assert!(
            d2.paths.is_none(),
            "fresh mk() engine must start with no injected Paths"
        );
        let mut cfg = d2.cfg.clone();
        cfg.stt_engine = Some(vec![ds_config::SttEngine::ClaudeCode]);
        assert_ne!(
            cfg.resolved_stt(),
            d2.cfg.resolved_stt(),
            "test setup must force a real STT transition"
        );
        d2.reload(&cfg);

        assert_eq!(d2.stt.kind(), "claude_code");
        d2.plat.terminal_frontmost.set(true);
        d2.stt.start();
        let taps = d2.plat.tapped_chords.borrow();
        assert_eq!(taps.last(), Some(&ds_platform::KeyChord::default()));
    }
}
