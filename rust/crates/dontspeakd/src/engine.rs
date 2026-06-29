//! The `Engine<P>` gesture state machine: the Caps-Lock "tap to dictate, hold to
//! cancel" loop, plus the shared dictation-preview buffer it drives.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ds_config::{DropSpeechKind, VoiceConfig};
use ds_platform::Platform;
use ds_stt::Stt;

use crate::config_gate::{
    build_stt, caps_loop_enabled, debug_enabled, full_duplex_wanted, helper_needed,
    helper_stt_provider, helper_uses_stt, normalize_long_press, reconcile_helper_models,
};
use crate::listener;
use crate::logging::log;
use crate::status::{CAPS_LOG_MAX, CapsEvent, CapsLog, StatusGate, now_ms};
use crate::tts::TtsManager;
use crate::ttsq::TtsQueue;

/// Max gap between two Caps taps to count as a DOUBLE-tap (skip the current message).
/// Only armed while speech is PLAYING, so it never delays starting dictation from silence.
const DOUBLE_TAP_MS: u64 = 280;

/// Shared dictation-preview buffer — the engine ↔ confirm-panel channel.
///
/// While recording, `HelperStt` mirrors each PARTIAL transcript into `partial`.
/// On Caps-OFF it deposits the finalized transcript into `pending` (Some ⇒ the
/// confirm panel is up). Confirm-before-paste is unconditional: the engine pastes
/// `pending` on the user's confirm tap and clears it on cancel (long-press).
/// `target` is the app frontmost when recording started, shown in
/// the panel. Surfaced to the app via `model_status` (the `dictation` object).
/// Manual `Default` (not derived) so `has_paste_target` starts true (fail-open).
pub(crate) struct PasteBuf {
    /// Live partial transcript, updated as the helper emits PARTIAL lines.
    pub partial: String,
    /// Finalized transcript awaiting the confirm tap (Some ⇒ panel in confirm mode).
    pub pending: Option<String>,
    /// Name of the app focused when recording started (the paste target).
    pub target: Option<String>,
    /// Set true by the async listen-joiner when the deferred final transcript has
    /// landed (text OR empty), so the poll loop can fire the armed auto-submit — or
    /// disarm it on an empty result — without ever blocking on the transcription.
    pub final_ready: bool,
    /// LIVE: whether an editable text field is currently focused to receive the paste
    /// (the engine poll thread samples `Platform::paste_target_present` each tick while
    /// the dictation panel is up). The app reads it in `model_status` and tints the
    /// dictation glow when false — "no input to submit into". Init true (fail-open: no
    /// spurious warning before the first sample).
    pub has_paste_target: bool,
    /// LIVE: is the Caps key physically held right now (mirrored from the poll loop each
    /// tick)? While held, `model_status` does NOT surface the finalized `pending`
    /// transcript: the press might still become a long-press CANCEL, and showing the
    /// finalized text only to discard it ~`long_press_ms` later is the "reappear then
    /// dismiss" flicker. The transcript is revealed/submitted on RELEASE (a confirmed
    /// tap), or discarded by the long-press reset — never flashed mid-press.
    pub caps_held: bool,
    /// Monotonic dictation-session counter. Bumped at every session boundary — a new
    /// `HelperStt::start`, an `abort`, a `teardown_hold` (engine hot-swap), a
    /// `cancel_all`, or a fresh `start_recording`. `HelperStt::stop` finalizes the
    /// transcript on a DETACHED joiner thread (the Parakeet final pass is slow), so by
    /// the time it lands the buffer may belong to a different session: the joiner stamps
    /// the epoch it started under and deposits `pending`/`final_ready` ONLY if it still
    /// matches — otherwise a stale final would overwrite a cleared buffer or a newer
    /// session's live partials.
    pub epoch: u64,
}

impl Default for PasteBuf {
    fn default() -> Self {
        Self {
            partial: String::new(),
            pending: None,
            target: None,
            final_ready: false,
            has_paste_target: true, // fail-open: no orange warning before the first probe
            caps_held: false,
            epoch: 0,
        }
    }
}

/// What the dictation panel should display, derived from the buffer state. Returns
/// `(text, awaiting_confirm)`. The finalized `pending` transcript is surfaced ONLY when
/// the Caps key is NOT held: a held press might still become a long-press CANCEL, so
/// flashing the finalized text for ~`long_press_ms` only to discard it (the "reappear then
/// dismiss" glitch) is suppressed — `pending` is revealed on RELEASE (a confirmed tap) or
/// discarded by the long-press reset. Otherwise the live `partial` is shown. PURE.
pub(crate) fn dictation_preview(
    pending: Option<&str>,
    partial: &str,
    caps_held: bool,
) -> (String, bool) {
    match pending {
        Some(text) if !caps_held => (text.to_string(), true),
        _ => (partial.to_string(), false),
    }
}

/// Shared handle to the dictation-preview buffer (engine poll thread writes it,
/// the listen thread mirrors partials, the IPC thread reads it for status).
pub(crate) type PasteState = Arc<Mutex<PasteBuf>>;

/// The engine's mutable state + dependencies.
///
/// `plat` is an `Rc<P>` so the boxed STT engine (ClaudeNative) can borrow the
/// SAME platform instance the engine polls (one keyboard/event source, no
/// `unsafe impl Sync`). The engine owns the `Rc` for its whole life; `Stt` is
/// non-`Send`, driven only from this single poll thread.
pub(crate) struct Engine<P: Platform + 'static> {
    pub(crate) plat: Rc<P>,
    /// The selected STT engine. Caps edges route through this (§A.2). Default
    /// (ClaudeNative) reproduces Phase-1 Ctrl+G dictation exactly.
    pub(crate) stt: Box<dyn Stt>,
    pidfile: std::path::PathBuf,
    debug: bool,

    /// Whether a dictation is active — toggled on the start TAP, off on the stop
    /// TAP. Tracked so the start tap can barge-in TTS and publish the live dot.
    holding: bool,
    /// Whether the voice is currently PAUSED by a Caps tap while dictation is OFF
    /// (`stt_engine = off`). With dictation off the mic never opens, but a tap still
    /// pauses the voice and the next tap resumes it — the SAME pause/resume gesture as
    /// the dictation path, so Caps means the same thing in both modes. Unused when
    /// dictation is on (the `holding` record state drives pause/resume there).
    voice_paused: bool,
    /// Last polled physical Caps state — the whole gesture machine (tap/hold/release)
    /// works off the down/up EDGES of this. The Caps LED is a pure OUTPUT we drive; it
    /// is never read back to decide state, so there's no latch/LED mirror to track.
    caps_phys_prev: bool,

    // ── §F long-press reset ─────────────────────────────────────────────────
    /// Physical Caps hold ≥ this (ms) force-resets to idle, LED off.
    long_press_ms: u64,
    /// When the physical Caps key first went down (None = up).
    caps_down_since: Option<Instant>,
    /// Latch so a single sustained hold fires the reset exactly once — and so the
    /// release that ENDS a long-press is not mistaken for a tap.
    long_press_fired: bool,
    /// A Caps TAP whose action is DEFERRED while speech plays, to detect a DOUBLE-tap
    /// (skip the current message → next). `Some(release_instant)` after a first tap;
    /// the single fires from `tick` once `DOUBLE_TAP_MS` elapses with no second tap.
    /// `None` when idle or not speaking (then a tap acts immediately — no added latency
    /// on starting dictation from silence). See [`apply_caps_edge`](Self::apply_caps_edge).
    pending_tap_at: Option<Instant>,

    // ── engine-owns-everything: last-applied config + subsystem gates ─────────
    /// The config the engine last APPLIED. Held so [`Engine::reload`] can diff
    /// (`VoiceConfig::changes_since`) and touch only what changed — no full reload.
    pub(crate) cfg: VoiceConfig,
    /// Whether the Caps-Lock dictation loop is live (from `caps_enabled`). When
    /// false, `tick()` is a no-op (no poll, no emit).
    pub(crate) caps_enabled: bool,
    /// The warm-Kokoro owner (Phase 2). `None` in tests; set in `main`. Used to
    /// barge-in on the caps OFF→ON edge and to start/stop on the tts toggle.
    pub(crate) tts: Option<Arc<TtsManager>>,
    /// The engine TTS queue. `None` in tests; set in `main`. The caps start-tap
    /// clears it (barge-in) so dictation never plays over a stale reply/narration.
    pub(crate) ttsq: Option<Arc<TtsQueue>>,
    /// Shared mirror of the EFFECTIVE caps state (caps_loop_enabled && AX trusted),
    /// published for the RPC `model_status` running.caps dot. `None` in tests.
    pub(crate) caps_active: Option<Arc<AtomicBool>>,
    /// Shared mirror of the live dictation (recording) state, published for the
    /// app's caps status panel via `model_status`. `None` in tests.
    pub(crate) stt_active: Option<Arc<AtomicBool>>,
    /// Shared bounded log of recent caps events (press/release/tap/reset), the
    /// engine → app status channel the Settings window renders. `None` in tests.
    pub(crate) caps_log: Option<CapsLog>,
    /// The always-listening (hands-free) runtime, present only while
    /// `listen_mode == Always`. Built/dropped on that config edge; `None` in the
    /// default Caps-Lock PTT mode and in tests.
    pub(crate) listener: Option<listener::Listener<P>>,

    // ── confirm-before-paste (dictation preview) ─────────────────────────────
    /// Shared dictation-preview buffer (live partials + the transcript awaiting
    /// confirmation + the paste target). `HelperStt` writes it; the engine pastes
    /// `pending` on the confirm tap and clears it on cancel. Always present (cheap
    /// default in tests, where no transcript is ever deposited).
    pub(crate) paste: PasteState,
    /// Latches when a Caps press BEGINS while a transcript is awaiting confirmation
    /// — that press is the confirm/cancel gesture (a quick tap confirms & pastes,
    /// a long-press cancels), NOT the start of a new recording. Distinguishes a
    /// fresh confirm tap from the release of the tap that just stopped recording.
    confirm_armed: bool,

    // ── status push gate (engine→app overlay PUSH) ───────────────────────────
    /// The shared push gate, bumped on each tick the dictation-overlay state changes
    /// so a blocked `WaitModelStatus` wakes immediately. `None` in tests; set in
    /// `engine_run` to the SAME `Arc` the IPC `EngineShared` holds.
    pub(crate) status_gate: Option<Arc<StatusGate>>,
    /// Digest of the last-published dictation-overlay state, so the tick bumps the
    /// gate only on an actual change (not every 30 ms tick).
    dict_digest: u64,
}

impl<P: Platform + 'static> Engine<P> {
    /// Construct with the Phase-1 default ClaudeNative STT engine (used by the
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
        Self::assemble(plat, stt, VoiceConfig::default(), pidfile, long_press_ms)
    }

    /// Construct selecting the STT engine from config via the factory
    /// (degrade-to-default-never-silent, §A.3).
    pub(crate) fn with_config(
        plat: P,
        cfg: &VoiceConfig,
        pidfile: std::path::PathBuf,
        long_press_ms: u64,
    ) -> Self {
        let plat = Rc::new(plat);
        let stt = ds_engines::make_stt(cfg, plat.clone());
        Self::assemble(plat, stt, cfg.clone(), pidfile, long_press_ms)
    }

    fn assemble(
        plat: Rc<P>,
        stt: Box<dyn Stt>,
        cfg: VoiceConfig,
        pidfile: std::path::PathBuf,
        long_press_ms: u64,
    ) -> Self {
        // Caps dictation needs Accessibility trust AND the config toggle(s).
        let caps_enabled = caps_loop_enabled(&cfg) && plat.preflight().is_ok();
        Self {
            plat,
            stt,
            pidfile,
            debug: debug_enabled(),
            holding: false,
            voice_paused: false,
            caps_phys_prev: false,
            long_press_ms,
            caps_down_since: None,
            long_press_fired: false,
            pending_tap_at: None,
            cfg,
            caps_enabled,
            tts: None,
            ttsq: None,
            caps_active: None,
            stt_active: None,
            caps_log: None,
            listener: None,
            paste: Arc::new(Mutex::new(PasteBuf::default())),
            confirm_armed: false,
            status_gate: None,
            dict_digest: 0,
        }
    }

    fn dbg(&self, s: &str) {
        if self.debug {
            log(s);
        }
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
        let (text, awaiting, has_target) = self
            .paste
            .lock()
            .map(|p| {
                let (t, a) = dictation_preview(p.pending.as_deref(), &p.partial, p.caps_held);
                (t, a, p.has_paste_target)
            })
            .unwrap_or((String::new(), false, true));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        awaiting.hash(&mut h);
        has_target.hash(&mut h);
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
    /// `holding`, the published `stt_active` (so the menu-bar icon doesn't stay "recording"
    /// with no actual listen), the confirm latch, and the preview buffer. Idempotent when
    /// already idle. Callers that rebuild `self.stt` must invoke this BEFORE the swap (it
    /// aborts the CURRENT engine).
    fn teardown_hold(&mut self) {
        if self.holding {
            self.stt.abort();
        }
        self.holding = false;
        self.set_stt_active(false);
        self.confirm_armed = false;
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.pending = None;
            p.target = None;
            p.final_ready = false;
            // New session boundary: invalidate any in-flight `stop` joiner so its
            // late final can't repopulate this just-cleared buffer (the engine
            // hot-swap drops the old HelperStt, but its detached joiner survives).
            p.epoch = p.epoch.wrapping_add(1);
        }
    }

    /// Whether a finalized transcript is waiting for the user's confirm tap (Some
    /// `pending` ⇒ the confirm panel is up and the Caps key means confirm/cancel).
    fn awaiting_confirm(&self) -> bool {
        self.paste
            .lock()
            .map(|p| p.pending.is_some())
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
    /// Return when the `auto_submit` config is ON (the default; off ⇒ insert only).
    /// Driven by the deferred-submit check once the stop tap's async final lands. The LED
    /// is already OFF (the stop tap drove it off on release); we ensure it here too.
    fn confirm_paste(&mut self) {
        // The confirm tap ALWAYS pastes — there's no focus refusal. The "is there a
        // paste target?" cue is a live glow on the panel (the engine samples
        // `paste_target_present` each tick → `has_paste_target` → the app tints it
        // orange when there's nowhere to land).
        let text = self.paste.lock().ok().and_then(|mut p| {
            p.partial.clear();
            p.target = None;
            p.pending.take()
        });
        if let Some(text) = text {
            // Paste into WHATEVER is focused (terminal, Notes, browser, chat, …). The
            // explicit confirm tap + the overlay's "→ <app>" target label are the
            // deliberate gate now, so the paste is no longer restricted to a terminal.
            self.plat.type_text(&text);
            // Auto-submit (press Return) per the `auto_submit` config: ON (default) submits
            // in ANY focused app — terminal, chat box, search field, editor; OFF just inserts
            // the transcript and the user presses Return themselves.
            let submit = self.cfg.auto_submit;
            if submit {
                self.plat.press_enter();
                if let Some(q) = &self.ttsq {
                    // Mark the voice submit's own auto-Enter so the text-drop path
                    // (`MarkActive`) doesn't double-count it as a text submit. Gated on
                    // `submit` — with `auto_submit=off` the engine never presses Enter, so a
                    // later manual Enter is a real text submit caught by `MarkActive`.
                    q.note_voice_submit();
                    // `drop_speech_on` contains `voice` → drop this window's now-stale
                    // pending speech before the new turn answers. Scoped to the active window.
                    if self.cfg.drop_speech_on.contains(&DropSpeechKind::Voice) {
                        q.clear_active_session();
                    }
                }
            }
        }
        self.plat.set_caps_lock(false);
        self.confirm_armed = false;
        self.record_caps("confirm");
        self.dbg("deferred submit — pasted pending transcript + Enter, LED off");
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
        if self.holding {
            self.stt.abort();
        }
        // Silence the voice / cancel generation: clear the warm queue + barge the cold
        // one-shot speaker. (Unlike a normal stop, a hold does NOT resume — it means
        // "stop talking".)
        if let Some(q) = &self.ttsq {
            q.clear();
        }
        let _ = ds_proc::barge_in(&self.pidfile);
        // Reset to idle, LED off. The LED is a pure output — we drive it off directly (no
        // read-back), so there's no latch-lag transient.
        self.plat.set_caps_lock(false);
        self.holding = false;
        self.set_stt_active(false);
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.pending = None;
            p.target = None;
            p.final_ready = false;
            // Invalidate any in-flight `stop` joiner (see PasteBuf::epoch) so a stale
            // final can't land after this cancel reset the buffer.
            p.epoch = p.epoch.wrapping_add(1);
        }
        self.confirm_armed = false;
        self.record_caps("cancel");
        self.dbg("HOLD cancel — dictation discarded, voice silenced, LED off, idle");
    }

    /// Whether dictation can START right now: the selected STT engine is on AND ready to
    /// transcribe. `BuiltIn` (Parakeet) needs its model resident + warm; `System` needs the OS
    /// recognizer ready (probed only when selected — the probe isn't free); `ClaudeCode`
    /// delegates so it's always ready; `Off` never. See [`crate::config_gate::stt_can_start`].
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
        crate::config_gate::stt_can_start(
            resolved.unwrap_or(SttEngine::Off),
            q.stt_loaded(),
            system_ready,
        )
    }

    /// A completed Caps TAP toggles dictation: start when idle, stop+submit when recording.
    /// Only flips `holding` — the tick re-asserts the Caps LED to match it (on this release
    /// and every held tick), so the light always reflects the real recording state and never
    /// changes on the press.
    fn toggle_dictation(&mut self) {
        // GUARD: dictation can START only if the selected STT engine is actually READY to
        // transcribe (BuiltIn/System model resident + warm; ClaudeCode delegates → always).
        // On-but-not-ready behaves like OFF — the tap pauses/resumes the voice but never opens
        // the mic/overlay (nothing to transcribe into yet). While ALREADY recording, keep the
        // plain on/off gate so a tap still STOPS it (the model was ready when it started).
        let gate = if self.holding {
            self.cfg.resolved_stt().is_some()
        } else {
            self.stt_ready_to_dictate()
        };
        match caps_tap_action(gate, self.holding, self.voice_paused) {
            CapsTap::StartRecord => self.start_recording(), // opens mic; pauses the voice
            CapsTap::StopRecord => self.stop_recording(),   // stops+submits; resumes the voice
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
    /// In-flight handling (mirrors `cancel_all`'s teardown, but deliberately
    /// WITHOUT driving the LED):
    ///   * If a dictation is active, `abort()` the OUTGOING engine first —
    ///     ClaudeNative releases Ctrl+G cleanly (nothing left held); Parakeet
    ///     discards the in-flight capture without injecting (§F.1).
    ///   * Swap in a fresh engine built on the SAME platform `Rc` (one event
    ///     source — never two engines fighting over the keyboard).
    ///   * Clear `holding` so the new engine starts idle.
    ///
    /// We do NOT call `set_caps_lock`: driving the LED is the gesture machine's job. A
    /// reload leaves the physical key untouched; since tap/long-press detection is
    /// edge-based on the physical key (not the LED), a reload never fabricates a
    /// spurious tap.
    pub(crate) fn reload(&mut self, cfg: &VoiceConfig) {
        // Diff against the last-applied config and touch ONLY what changed — the
        // "no extra reloads" contract (docs/DAEMON-REFACTOR.md). Per-call params
        // (voice/rate/narrate/region/vocab) need no action: the next call reads
        // them fresh from `self.cfg`, which we update unconditionally below.
        let change = cfg.changes_since(&self.cfg);

        // long_press is a cheap scalar latch — refresh it every reload.
        self.long_press_ms = normalize_long_press(cfg.long_press_ms);

        // STT engine: rebuild only when the engine selection (or enable) changed.
        // A rebuild ends any in-flight HOLD cleanly (clean release / discard-no-
        // inject) on the OUTGOING engine first, then swaps in a fresh engine on the
        // SAME platform Rc and resets the toggle bookkeeping so it starts idle.
        if change.stt_changed {
            // Reset the recording state (incl. the published `stt_active` icon) BEFORE the
            // swap — otherwise a reload mid-dictation leaves the menu-bar icon stuck
            // "recording" with no live listen on the fresh engine.
            self.teardown_hold();
            self.stt = build_stt(cfg, self.plat.clone(), self.tts.as_ref(), &self.paste);
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
        }

        // Warm helper lifecycle: it hosts BOTH engines now, so (re)gate it whenever
        // TTS or STT toggles/engine changes — run it iff either engine needs it.
        if (change.tts_toggled || change.stt_changed)
            && let Some(tts) = &self.tts
        {
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
        let listen_changed = cfg.listen_mode != self.cfg.listen_mode
            || cfg.hands_free != self.cfg.hands_free
            || cfg.submit_confirm_ms != self.cfg.submit_confirm_ms
            || cfg.endpoint_silence_ms != self.cfg.endpoint_silence_ms;
        if cfg.listen_mode == ds_config::ListenMode::Always {
            if self.listener.is_none() || listen_changed {
                self.listener = Some(listener::Listener::new(
                    cfg,
                    self.plat.clone(),
                    ds_model::parakeet_dir().unwrap_or_default(),
                    self.paste.clone(),
                    self.stt_active
                        .clone()
                        .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
                    self.ttsq.clone(),
                    self.status_gate.clone(),
                ));
            }
        } else if self.listener.is_some() {
            self.listener = None;
        }

        // Record the applied config so the NEXT reload diffs against it.
        self.cfg = cfg.clone();
        // NOTE: caps_down_since / long_press_fired are physical-key latches; a
        // config reload does not change the physical key, so leave them as-is.
        log(&format!(
            "dontspeakd reloaded config (caps={} stt={}{} tts={} long_press={}ms narrate={})",
            self.caps_enabled,
            cfg.resolved_stt().map(|e| e.as_str()).unwrap_or("off"),
            if change.stt_changed { " [rebuilt]" } else { "" },
            cfg.resolved_tts().map(|e| e.as_str()).unwrap_or("off"),
            self.long_press_ms,
            cfg.narrate_summary()
        ));
    }

    /// Apply the effective caps-loop gate (`caps_loop_enabled(cfg) && AX trusted`).
    /// If it just went OFF mid-hold, end the HOLD cleanly (no key left down / mic
    /// open). Publishes to the shared `caps_active` for the RPC running-dot.
    fn set_caps_gate(&mut self, now_on: bool) {
        if !now_on && self.holding {
            self.teardown_hold();
        }
        self.caps_enabled = now_on;
        if let Some(ca) = &self.caps_active {
            ca.store(now_on, Ordering::Relaxed);
        }
    }

    /// Periodic Accessibility re-probe (called from the loop): if AX trust changed
    /// since last time, flip the caps loop on/off live — so GRANTING Accessibility
    /// turns dictation green without a reload/restart, and revoking turns it off.
    pub(crate) fn refresh_caps_gate(&mut self) {
        let now_on = caps_loop_enabled(&self.cfg) && self.plat.preflight().is_ok();
        if now_on != self.caps_enabled {
            self.set_caps_gate(now_on);
            log(&format!(
                "caps loop {} (Accessibility re-probe)",
                if now_on { "ENABLED" } else { "disabled" }
            ));
        }
    }

    /// One poll, driving the "tap to dictate, hold to cancel" gesture machine off the
    /// PHYSICAL Caps key (down/up edges via `caps_phys_prev`), NOT the OS lock latch:
    ///   * a quick TAP (release before `long_press_ms`) toggles recording;
    ///   * a LONG-PRESS (hold ≥ `long_press_ms`) force-resets to idle;
    ///   * the Caps LED is a pure OUTPUT, re-asserted to `holding` — never read back.
    ///
    /// See the inline GESTURE MODEL block below for the full rationale.
    pub(crate) fn tick(&mut self) {
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
                self.plat.terminal_frontmost()
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
            // (custom-drawn TTY views), so `paste_target_present` reads "no target" even
            // though a synthetic Cmd+V lands fine. Treat a frontmost terminal as a paste
            // target too: that's exactly where the focus-gated Caps + voice-submit paths
            // inject (see `listener::exec`), so the glow must not warn there.
            let present = self.plat.paste_target_present() || self.plat.terminal_frontmost();
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
            // Voice submit drops this window's speech when `drop_speech_on` contains `voice`.
            let drop_on_voice = self.cfg.drop_speech_on.contains(&DropSpeechKind::Voice);
            if let Some(l) = self.listener.as_mut() {
                l.tick(busy, drop_on_voice);
            }
            return;
        }

        // caps_enabled gate: when the dictation loop is off, the engine does no
        // polling and no emits — it's a pure RPC host for the other subsystems.
        if !self.caps_enabled {
            return;
        }

        // ─────────────────────────────────────────────────────────────────────────
        // GESTURE MODEL — "tap to dictate, hold to cancel". Driven entirely off the
        // PHYSICAL Caps key (down / hold / up), NOT the OS lock latch:
        //   • DOWN    — nothing yet. Start the press timer; re-assert the LED to the
        //               real recording state so the OS's own latch-flip never changes
        //               the light on a press (the light only moves on RELEASE).
        //   • HOLD ≥ long_press_ms — CANCEL: discard any in-flight dictation AND silence
        //               the voice/generation, back to idle. Never records, never lights.
        //   • quick UP (released before the threshold) — a TAP toggles dictation: start
        //               when idle, stop+submit when recording. The LED flips HERE.
        // The Caps LED is a pure OUTPUT we drive on these edges — never read back to
        // decide state, so there is no latch/LED desync. (A sub-poll tap too fast for the
        // ~30 ms poll to even observe the key-down is missed — tap again; the old latch-
        // mirror caught those, at the cost of the desync bugs this model removes.)
        // Feed the gesture machine from whichever source the platform exposes:
        //   • EVENT-DRIVEN (Windows low-level hook) — drain the lossless queue and replay
        //     every real transition. A down+up that both fell inside one tick is two edges
        //     here, so a tap faster than the poll is NEVER dropped (the old miss).
        //   • POLLED (macOS / Linux / tests) — sample the held boolean and synthesize one
        //     edge when it changed since last tick, exactly as before.
        if self.plat.caps_event_driven() {
            for e in self.plat.drain_caps_events() {
                self.apply_caps_edge(e.down, e.at);
            }
            // Keep the polled mirror coherent (the event path doesn't read it, but other
            // code may inspect it); it tracks the live latched state.
            self.caps_phys_prev = self.plat.caps_physically_down();
        } else {
            let down = self.plat.caps_physically_down();
            let prev = self.caps_phys_prev;
            self.caps_phys_prev = down;
            if down != prev {
                self.apply_caps_edge(down, Instant::now());
            }
        }
        // Time-based half of the gesture: a sustained HOLD fires the long-press CANCEL even
        // when no new edge arrives this tick, and the Caps LED is re-pinned to `holding`
        // while the key is down (a no-op on the event-driven port, which owns/suppresses
        // the key and never drives the LED).
        self.check_long_press();
        // Fire a deferred single tap if its double-tap window lapsed with no second tap.
        self.check_pending_tap();

        // DEFERRED submit: the stop tap armed `confirm_armed`; the local-transcript engine
        // deposits its FINAL asynchronously, so paste once it lands (or disarm if empty).
        // The LED is already OFF (driven on the stop tap's release) — this only moves text.
        if self.confirm_armed {
            // Read pending + final_ready under ONE lock so the async joiner can't straddle
            // the two checks (deposit pending between them and get disarmed).
            let (has_pending, ready) = self
                .paste
                .lock()
                .map(|p| (p.pending.is_some(), p.final_ready))
                .unwrap_or((false, false));
            if has_pending {
                self.confirm_paste();
            } else if ready {
                // The deferred final landed EMPTY — nothing to submit. Disarm + drop the flag.
                self.confirm_armed = false;
                if let Ok(mut p) = self.paste.lock() {
                    p.final_ready = false;
                }
                self.record_caps("confirm");
            }
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
            self.caps_down_since = Some(at);
            self.long_press_fired = false;
            self.record_caps("press");
        } else {
            // The light flips HERE on release — never on the press — then we snap the LED
            // to the final `holding` so a long-press release (no toggle) also lands
            // consistent. On the key-owning Windows port `set_caps_lock` drives the
            // physical LED out-of-band (IOCTL) without toggling the logical Caps state.
            let was_tap = !self.long_press_fired;
            self.caps_down_since = None;
            self.record_caps("release");
            if was_tap {
                self.handle_tap(at);
            }
            self.long_press_fired = false;
            self.plat.set_caps_lock(self.holding);
        }
    }

    /// A Caps TAP (quick release). While speech is PLAYING, the tap is DEFERRED up to
    /// [`DOUBLE_TAP_MS`] to see whether a SECOND tap follows: two quick taps = skip the
    /// current message and advance to the next ([`TtsQueue::skip_current`]); a lone tap =
    /// the normal [`toggle_dictation`](Self::toggle_dictation), fired from [`tick`] once the
    /// window lapses. While NOT speaking there is nothing to skip, so the tap acts
    /// IMMEDIATELY — starting dictation from silence keeps zero added latency.
    fn handle_tap(&mut self, at: Instant) {
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
                self.dbg("double-tap — skipped current message, advancing to next");
            }
            // First tap while speaking → defer; the single fires from `tick` if no
            // second tap arrives within the window.
            TapAction::Defer => self.pending_tap_at = Some(at),
        }
    }

    /// Fire a DEFERRED single tap once its [`DOUBLE_TAP_MS`] window has elapsed with no
    /// second tap. Skipped while a Caps press is in flight (`caps_down_since` set) — that
    /// could be the second tap of a double, or a hold becoming a long-press — so the single
    /// never fires mid-gesture. Run once per [`tick`].
    fn check_pending_tap(&mut self) {
        if self.caps_down_since.is_some() {
            return;
        }
        if let Some(t0) = self.pending_tap_at
            && t0.elapsed() > Duration::from_millis(DOUBLE_TAP_MS)
        {
            self.pending_tap_at = None;
            self.toggle_dictation();
        }
    }

    /// The time-based half of the gesture, run once per [`tick`] regardless of edges:
    /// a Caps hold past `long_press_ms` force-resets to idle (CANCEL — discard
    /// dictation + silence voice), exactly once per press; and the Caps LED is
    /// re-pinned to `holding` for as long as the key is held (counters the OS's own
    /// hold-delay latch flip on the polled ports — a no-op on Windows, which
    /// suppresses the key outright).
    fn check_long_press(&mut self) {
        let Some(t) = self.caps_down_since else {
            return;
        };
        if !self.long_press_fired && t.elapsed() >= Duration::from_millis(self.long_press_ms) {
            self.cancel_all();
            self.long_press_fired = true;
        }
        self.plat.set_caps_lock(self.holding);
    }

    /// Start dictation — called from `toggle_dictation` on a tap's RELEASE. No-op if
    /// already recording. ClaudeNative posts the focus-gated initial Ctrl+G key-DOWN. A
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
        if self.holding {
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
            self.dbg(&format!("barge-in: killed TTS pgid={pgid}"));
        }
        self.holding = true;
        // Capture the paste target (the app that's ALREADY focused) + clear any stale
        // preview so the confirm panel opens fresh, labeled with where the text will
        // land. We never steal focus here — the transcript pastes into whatever the
        // user is in (the focus-gated Ctrl+G / paste targets the current frontmost app).
        let target = self.plat.frontmost_app_name();
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.pending = None;
            p.target = target;
            p.final_ready = false;
            // Fresh session: bump the epoch so a prior `stop` joiner that hasn't
            // deposited yet is invalidated and can't clobber this recording (see
            // PasteBuf::epoch). HelperStt::start bumps again under the same lock; both
            // bumps just advance the counter, so the net effect is one new session.
            p.epoch = p.epoch.wrapping_add(1);
        }
        self.confirm_armed = false;
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
        self.dbg("LED ON — stt.start()");
    }

    /// Stop dictation (full-mirror ON→OFF edge). No-op if not recording. Ends the
    /// listen (`stt.stop`) and arms the deferred submit for local-transcript engines.
    fn stop_recording(&mut self) {
        if !self.holding {
            return;
        }
        self.holding = false;
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
            self.confirm_armed = true;
        }
        self.record_caps("stop");
        self.dbg("LED OFF — stt.stop()");
    }

    /// Never leave a key down on shutdown.
    pub(crate) fn shutdown(&mut self) {
        if self.holding {
            self.stt.stop();
        }
        log("dontspeakd stopped");
    }
}

/// What a completed Caps TAP does — UNIFIED across dictation on/off so the gesture means
/// the same thing either way: a tap PAUSES the voice (held, never dropped), the next tap
/// RESUMES it. With dictation on the pause/resume rides the record start/stop; with it off
/// the mic never opens but the voice still pauses/resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapsTap {
    /// Dictation on, idle → begin recording (which pauses the voice).
    StartRecord,
    /// Dictation on, recording → stop + submit (which resumes the voice).
    StopRecord,
    /// Dictation off, voice playing/idle → pause the voice (hold; nothing dropped).
    PauseVoice,
    /// Dictation off, voice paused → resume the voice.
    ResumeVoice,
}

/// Decide a Caps tap's action from `(dictation on?, currently recording?, voice paused?)`.
/// Pure — the engine wires the result to the queue, and this is exhaustively unit-tested.
pub(crate) fn caps_tap_action(stt_on: bool, recording: bool, voice_paused: bool) -> CapsTap {
    match (stt_on, recording, voice_paused) {
        (true, false, _) => CapsTap::StartRecord,
        (true, true, _) => CapsTap::StopRecord,
        (false, _, false) => CapsTap::PauseVoice,
        (false, _, true) => CapsTap::ResumeVoice,
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
        // Dictation ON: tap toggles recording (start pauses, stop resumes) — never clears.
        assert_eq!(caps_tap_action(true, false, false), CapsTap::StartRecord);
        assert_eq!(caps_tap_action(true, true, false), CapsTap::StopRecord);
        // voice_paused is irrelevant while dictation is on (record state drives it).
        assert_eq!(caps_tap_action(true, false, true), CapsTap::StartRecord);
        assert_eq!(caps_tap_action(true, true, true), CapsTap::StopRecord);

        // Dictation OFF: a tap PAUSES (held, not cleared/dropped), the next tap RESUMES —
        // the same pause/resume gesture as dictation-on, so Caps is consistent and no
        // narration is ever silenced; it's queued while paused.
        assert_eq!(caps_tap_action(false, false, false), CapsTap::PauseVoice);
        assert_eq!(caps_tap_action(false, false, true), CapsTap::ResumeVoice);
        // recording can't be true when dictation is off; both states still pause/resume.
        assert_eq!(caps_tap_action(false, true, false), CapsTap::PauseVoice);
        assert_eq!(caps_tap_action(false, true, true), CapsTap::ResumeVoice);
    }

    #[test]
    fn caps_tap_pauses_then_resumes_with_dictation_off() {
        // End-to-end through the real gesture path: with dictation off, a Caps tap PAUSES
        // the voice (held — so any narration that arrives stays queued, not dropped) and
        // the next tap RESUMES it. ttsq is None in tests, so the queue calls are no-ops;
        // we assert the engine's pause STATE flips (the decision that replaced the old
        // silence/clear behavior). Recording never starts (the mic stays shut).
        let mut d = mk(600);
        d.cfg.stt_engine = Vec::new(); // dictation off
        MockPlatform::tap(&mut d);
        assert!(d.voice_paused, "first tap pauses the voice");
        assert!(!d.holding, "dictation off → the mic never opens");
        MockPlatform::tap(&mut d);
        assert!(!d.voice_paused, "second tap resumes the voice");
    }
    use crate::config_gate::DEFAULT_LONG_PRESS_MS;
    use ds_platform::{
        CapsKeyMonitor, CapsLockReader, FrontmostWindow, KeyInjector, PreflightError,
    };
    use std::cell::Cell;

    /// A controllable mock Platform for the §F long-press logic. All state is
    /// `Cell` so the `&self` trait methods can record/return without unsafe.
    #[derive(Default)]
    struct MockPlatform {
        // inputs the test drives
        caps_phys_down: Cell<bool>,
        lock_state: Cell<bool>,
        terminal_frontmost: Cell<bool>,
        /// Whether an editable field is "focused" — backs `paste_target_present`, which
        /// the engine samples live to drive the dictation "no target" glow. Defaults
        /// false (Cell default).
        paste_target: Cell<bool>,
        // outputs the test asserts on
        ctrl_g_down_calls: Cell<u32>,
        ctrl_g_up_calls: Cell<u32>,
        set_caps_off_calls: Cell<u32>,
        /// Count of `type_text` (paste) calls — lets the focus-check tests assert
        /// whether a confirm tap actually pasted.
        type_text_calls: Cell<u32>,
    }

    impl CapsLockReader for MockPlatform {
        fn read(&self) -> Option<bool> {
            Some(self.lock_state.get())
        }
        /// The latched LED the full-mirror tick follows. Tests drive it via
        /// `lock_state` (and `set_caps_lock` writes the same Cell), so a force-reset's
        /// LED-OFF write is reflected here too.
        fn caps_lock_on(&self) -> bool {
            self.lock_state.get()
        }
    }
    impl KeyInjector for MockPlatform {
        // A `tap_key` is one discrete press+release, so it bumps BOTH the down and up
        // counters the caps-state-machine tests assert on — keeping every existing
        // assertion valid now that ClaudeNative taps a chord instead of Ctrl+G down/up.
        fn tap_key(&self, _chord: &ds_platform::KeyChord) {
            self.ctrl_g_down_calls.set(self.ctrl_g_down_calls.get() + 1);
            self.ctrl_g_up_calls.set(self.ctrl_g_up_calls.get() + 1);
        }
        fn type_text(&self, _text: &str) {
            self.type_text_calls.set(self.type_text_calls.get() + 1);
        }
    }
    impl FrontmostWindow for MockPlatform {
        fn terminal_frontmost(&self) -> bool {
            self.terminal_frontmost.get()
        }
        fn paste_target_present(&self) -> bool {
            self.paste_target.get()
        }
    }
    impl CapsKeyMonitor for MockPlatform {
        fn caps_physically_down(&self) -> bool {
            self.caps_phys_down.get()
        }
        fn set_caps_lock(&self, on: bool) {
            self.lock_state.set(on);
            if !on {
                self.set_caps_off_calls
                    .set(self.set_caps_off_calls.get() + 1);
            }
        }
    }
    impl Platform for MockPlatform {
        fn preflight(&self) -> Result<(), PreflightError> {
            Ok(())
        }
    }

    fn mk(long_press_ms: u64) -> Engine<MockPlatform> {
        Engine::new(
            MockPlatform::default(),
            std::path::PathBuf::from("/tmp/ds-test-nonexistent.pid"),
            long_press_ms,
        )
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
        d.plat.terminal_frontmost.set(true);
        d.holding = true;
        d.plat.lock_state.set(true);
        d.cancel_all();
        assert!(!d.holding, "holding cleared");
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
            1,
            "aborted the active dictation (one keypress to end it)"
        );
        assert!(!d.plat.lock_state.get(), "LED driven off");
    }

    #[test]
    fn dictation_preview_gates_pending_on_caps_held() {
        // Released, finalized transcript present → surfaced + panel in confirm mode.
        assert_eq!(
            dictation_preview(Some("hello world"), "hel", false),
            ("hello world".to_string(), true)
        );
        // HELD: never surface the finalized transcript (the press might still become a
        // long-press cancel) — show the live partial, NOT in confirm mode.
        assert_eq!(
            dictation_preview(Some("hello world"), "hel", true),
            ("hel".to_string(), false)
        );
        // No pending → always the live partial, regardless of the held state.
        assert_eq!(
            dictation_preview(None, "part", false),
            ("part".to_string(), false)
        );
        assert_eq!(
            dictation_preview(None, "part", true),
            ("part".to_string(), false)
        );
    }

    #[test]
    fn caps_held_mirrored_and_suppresses_finalized_transcript() {
        // The reported bug: a transcript is finalized (pending) and the user is HOLDING
        // Caps toward a long-press cancel. The poll loop must mirror the physical held
        // state, and the preview must NOT flash the finalized text while held — so the
        // long-press just dismisses instead of "reappear then dismiss".
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        d.paste.lock().unwrap().pending = Some("discard me".into());

        // Physical press (long-press in flight): one tick mirrors the held state.
        d.plat.caps_phys_down.set(true);
        d.tick();
        {
            let p = d.paste.lock().unwrap();
            assert!(p.caps_held, "down edge mirrors caps_held=true");
            let (text, awaiting) = dictation_preview(p.pending.as_deref(), &p.partial, p.caps_held);
            assert!(!awaiting, "held: finalized transcript is NOT surfaced");
            assert_eq!(text, "", "held: shows the partial, not the pending");
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
        assert!(!d.holding, "press alone does not start recording");
        assert!(
            !d.plat.lock_state.get(),
            "press alone does not light the LED"
        );
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
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
        assert!(!d.holding, "not recording until release");
        assert!(!d.plat.lock_state.get(), "dark until release");

        // UP edge (a tap): recording starts and the LED lights — ON RELEASE.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(d.holding, "tap starts dictation on release");
        assert!(d.plat.lock_state.get(), "LED lit on release");
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
            1,
            "start posted its keypress"
        );
        assert!(d.caps_down_since.is_none(), "press latch released");
    }

    #[test]
    fn second_tap_stops_and_extinguishes_on_release_not_press() {
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::tap(&mut d);
        assert!(d.holding, "first tap recording");

        // Second tap's PRESS-DOWN: still recording, LED STAYS lit (no dark-on-press).
        d.plat.caps_phys_down.set(true);
        d.tick();
        assert!(d.holding, "still recording while the stop press is held");
        assert!(
            d.plat.lock_state.get(),
            "LED stays lit on the stop press-down"
        );

        // RELEASE: stop + LED off — the light extinguishes on release, not on press.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(!d.holding, "second tap stops dictation on release");
        assert!(!d.plat.lock_state.get(), "LED extinguished on release");
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
        assert!(d.holding && d.plat.lock_state.get(), "recording, lit");
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
        assert!(!d.holding, "hold never starts recording");
        assert!(!d.plat.lock_state.get(), "hold never lights the LED");
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
            0,
            "no dictation keypress on a hold"
        );
    }

    #[test]
    fn long_press_cancel_from_idle_keeps_led_off_despite_os_latch_toggle() {
        // macOS's caps-lock hold-delay toggles the OS latch ON partway through a long
        // hold — AFTER the press edge. A long-press cancel from idle must not leave the
        // LED lit out of sync with `holding`; the held-tick re-assert pins it back off.
        let mut d = mk(5);
        d.plat.terminal_frontmost.set(true);

        d.plat.caps_phys_down.set(true);
        d.tick(); // down edge — idle, no light
        assert!(!d.plat.lock_state.get(), "no light on the press");

        // macOS toggles the OS caps-lock latch ON mid-hold.
        d.plat.lock_state.set(true);
        std::thread::sleep(Duration::from_millis(12));
        d.tick(); // past threshold → cancel_all; the held re-assert pins the LED OFF
        assert!(!d.holding, "cancel from idle never records");
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
            !d.holding && !d.plat.lock_state.get(),
            "idle + LED off after release"
        );
    }

    #[test]
    fn hold_while_recording_discards_and_release_does_not_re_toggle() {
        let mut d = mk(5);
        d.plat.terminal_frontmost.set(true);
        MockPlatform::tap(&mut d); // start recording
        assert!(d.holding);
        let starts = d.plat.ctrl_g_down_calls.get();

        // Hold past the threshold → discard (abort the listen), LED off.
        d.plat.caps_phys_down.set(true);
        d.tick();
        std::thread::sleep(Duration::from_millis(12));
        d.tick();
        assert!(!d.holding, "hold discards the active dictation");
        assert!(!d.plat.lock_state.get(), "LED off after a discard");
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
            starts + 1,
            "aborted the listen once (ClaudeNative abort→stop = one keypress)"
        );

        // Extra polls past the threshold must NOT re-fire the cancel.
        d.tick();
        d.tick();
        assert_eq!(
            d.plat.ctrl_g_down_calls.get(),
            starts + 1,
            "cancel fires exactly once per press"
        );

        // The release that ENDS the hold is NOT a tap.
        d.plat.caps_phys_down.set(false);
        d.tick();
        assert!(!d.holding, "release after a hold stays idle");
        assert!(
            !d.plat.lock_state.get(),
            "still dark after the hold release"
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
        assert!(d.holding, "recording");
        MockPlatform::tap(&mut d); // stop — defers
        assert!(!d.holding, "stopped on release");
        assert!(d.confirm_armed, "deferred submit armed");
        assert!(
            !d.plat.lock_state.get(),
            "LED already off on the stop release"
        );
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "nothing pasted while the final is pending"
        );

        // The async final lands: deposit it + flag ready, then tick → paste + submit.
        {
            let mut p = d.paste.lock().unwrap();
            p.pending = Some("hello world".into());
            p.final_ready = true;
        }
        d.tick();
        assert_eq!(
            d.plat.type_text_calls.get(),
            1,
            "pasted once the final landed"
        );
        assert!(!d.confirm_armed, "disarmed after the deferred submit");
    }

    #[test]
    fn frontmost_terminal_is_a_paste_target_even_without_ax_focus() {
        // Regression: the "no target" glow used only the AX focused-element probe
        // (`paste_target_present`). Terminals — the app's main dictation target —
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
        assert!(d.confirm_armed, "armed");

        d.paste.lock().unwrap().final_ready = true; // empty: ready but no pending
        d.tick();
        assert!(!d.confirm_armed, "disarmed on an empty final");
        assert_eq!(
            d.plat.type_text_calls.get(),
            0,
            "nothing pasted for an empty final"
        );
    }

    // ── §E.4 Engine::reload over MockPlatform ───────────────────────────────

    #[test]
    fn reload_clears_state_aborts_inflight_and_never_drives_led() {
        let mut d = mk(600);
        d.plat.terminal_frontmost.set(true);
        // Simulate an in-flight dictation on the outgoing engine.
        d.holding = true;

        // Reload to a config that CHANGES the RESOLVED STT engine, forcing a rebuild (the
        // surgical reload only aborts + swaps when the engine actually changes). Disabling
        // dictation (empty ladder) flips the resolved engine on EVERY platform — unlike
        // naming a specific engine, which on a machine whose default ladder already resolves
        // to that engine would be a no-op.
        let cfg = VoiceConfig {
            stt_engine: Vec::new(),
            ..Default::default()
        };
        d.reload(&cfg);

        // The outgoing in-flight HOLD was released via abort() (ClaudeNative
        // abort == ctrl_g_up); the new engine starts from idle.
        assert!(!d.holding, "holding cleared after engine-changing reload");
        assert_eq!(
            d.plat.ctrl_g_up_calls.get(),
            1,
            "in-flight HOLD released via engine abort"
        );
        // Reload must NOT drive the LED (that is the gesture machine's job and
        // would itself create a synthetic edge).
        assert_eq!(
            d.plat.set_caps_off_calls.get(),
            0,
            "reload must never drive the Caps LED off"
        );
    }

    #[test]
    fn reload_noop_change_preserves_inflight_hold() {
        // Surgical reload: a change that touches only per-call params (here the
        // voice id) must NOT rebuild the engine or interrupt an in-flight HOLD.
        let mut d = mk(600);
        d.holding = true;

        let cfg = VoiceConfig {
            tts_built_in_voices: vec!["am_michael".into()],
            ..Default::default()
        };
        d.reload(&cfg);

        assert!(d.holding, "a per-call-only change must not drop the HOLD");
        assert_eq!(
            d.plat.ctrl_g_up_calls.get(),
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
        d.plat.terminal_frontmost.set(true);
        d.holding = true;
        let cfg = VoiceConfig {
            caps_enabled: false,
            ..Default::default()
        };
        d.reload(&cfg);
        assert!(!d.caps_enabled, "caps loop disabled");
        assert!(!d.holding, "HOLD ended when caps disabled mid-hold");
        assert_eq!(d.plat.ctrl_g_up_calls.get(), 1, "in-flight HOLD released");
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
        d.holding = true;
        d.paste.lock().unwrap().pending = Some("stale".into());

        // Disabling dictation (empty ladder) flips the resolved engine → stt_changed → rebuilt
        // (platform-independent, unlike naming an engine the default may already resolve to).
        let cfg = VoiceConfig {
            stt_engine: Vec::new(),
            ..Default::default()
        };
        d.reload(&cfg);
        assert!(!d.holding, "hold ended on engine swap");
        assert!(
            !active.load(Ordering::Relaxed),
            "recording icon (stt_active) cleared on the swap"
        );
        assert!(
            d.paste.lock().unwrap().pending.is_none(),
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
    fn reload_while_idle_does_not_abort() {
        // A reload when NOT holding must not call abort() (nothing in flight).
        let mut d = mk(600);
        assert!(!d.holding);
        d.reload(&VoiceConfig::default());
        assert_eq!(
            d.plat.ctrl_g_up_calls.get(),
            0,
            "idle reload does not release a key"
        );
        assert_eq!(
            d.plat.set_caps_off_calls.get(),
            0,
            "idle reload no LED drive"
        );
    }
}
