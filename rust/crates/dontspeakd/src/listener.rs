//! Always-listening (hands-free) runtime glue — the I/O layer over the pure
//! [`crate::listen`] state machines. See docs/ALWAYS-LISTENING.md.
//!
//! Driven once per engine poll tick, but capture, endpointing, and model inference run on
//! dedicated workers. The poll thread only drains ordered text/control events, feeds the pure
//! TurnLogic, and executes `Paste`/`Submit` actions into the focused prompt.
//!
//! Half-duplex play-gate: while the TTS queue is busy (speaking or pending) the
//! mic is CLOSED; when it goes idle the mic reopens. The platform `Rc` remains on the engine
//! thread; no native recognizer or audio operation can stall that thread.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use ds_config::VoiceConfig;
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::{Capture, LocalTranscriber, resample_to_16k};

use crate::PasteState;
use crate::listen::{
    DEFAULT_ENERGY_THRESHOLD, EndpointEvent, Endpointer, TurnAction, TurnLogic, frame_rms,
};
use crate::status::StatusGate;
use crate::ttsq::TtsQueue;

/// Fallback frame duration (ms) for a tick that drained no new audio — matches
/// the engine poll interval so the confirm-window timer still advances.
const FALLBACK_DT_MS: u64 = 30;

enum RawListenerEvent {
    Tick {
        epoch: u64,
        dt_ms: u64,
    },
    SpeechOnset {
        epoch: u64,
    },
    Segment {
        epoch: u64,
        pcm: Vec<f32>,
        rate: u32,
    },
}

enum ListenerEvent {
    Tick { epoch: u64, dt_ms: u64 },
    SpeechOnset { epoch: u64 },
    Segment { epoch: u64, text: String },
}

struct ListenerWorker {
    enabled: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    rx: Receiver<ListenerEvent>,
}

impl ListenerWorker {
    fn spawn(provider: &str, model_dir: PathBuf, endpoint_silence_ms: u64) -> Self {
        let enabled = Arc::new(AtomicBool::new(false));
        let epoch = Arc::new(AtomicU64::new(1));
        let stop = Arc::new(AtomicBool::new(false));
        // Bounded so a wedged recognizer cannot grow memory forever, but deep enough to keep
        // capture draining through ordinary multi-second inference spikes.
        let (raw_tx, raw_rx) = sync_channel::<RawListenerEvent>(256);
        let (event_tx, event_rx) = std::sync::mpsc::channel::<ListenerEvent>();

        let capture_enabled = enabled.clone();
        let capture_epoch = epoch.clone();
        let capture_stop = stop.clone();
        std::thread::Builder::new()
            .name("ds-always-capture".into())
            .spawn(move || {
                capture_worker(
                    capture_enabled,
                    capture_epoch,
                    capture_stop,
                    raw_tx,
                    endpoint_silence_ms,
                )
            })
            .ok();

        let provider = provider.to_string();
        let infer_stop = stop.clone();
        let infer_epoch = epoch.clone();
        std::thread::Builder::new()
            .name("ds-always-infer".into())
            .spawn(move || {
                inference_worker(
                    provider,
                    model_dir,
                    infer_stop,
                    infer_epoch,
                    raw_rx,
                    event_tx,
                )
            })
            .ok();

        Self {
            enabled,
            epoch,
            stop,
            rx: event_rx,
        }
    }

    fn set_enabled(&self, enabled: bool) {
        if self.enabled.swap(enabled, Ordering::SeqCst) != enabled {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
    }
}

impl Drop for ListenerWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.enabled.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        // Native recognizer finalization can block in an OS framework. Do not join here:
        // dropping/reloading always-listening must never freeze the engine poll thread.
    }
}

fn send_raw(tx: &SyncSender<RawListenerEvent>, event: RawListenerEvent) -> bool {
    tx.send(event).is_ok()
}

fn capture_worker(
    enabled: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    tx: SyncSender<RawListenerEvent>,
    endpoint_silence_ms: u64,
) {
    let mut capture: Option<Capture> = None;
    let mut endpointer = Endpointer::new(DEFAULT_ENERGY_THRESHOLD, endpoint_silence_ms);
    let mut segment = Vec::new();
    let mut active_epoch = epoch.load(Ordering::SeqCst);
    while !stop.load(Ordering::SeqCst) {
        let current_epoch = epoch.load(Ordering::SeqCst);
        if current_epoch != active_epoch || !enabled.load(Ordering::SeqCst) {
            capture = None;
            segment.clear();
            endpointer.reset();
            active_epoch = current_epoch;
            std::thread::sleep(Duration::from_millis(FALLBACK_DT_MS));
            continue;
        }
        if capture.is_none() {
            match Capture::open() {
                Ok(c) => capture = Some(c),
                Err(e) => {
                    log::warn!(target: "engine", "always-listen mic open: {e}");
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(FALLBACK_DT_MS));
        let capture_ref = capture.as_ref().expect("capture open");
        let rate = capture_ref.input_rate().max(1);
        let chunk = capture_ref.drain_new();
        if chunk.is_empty() {
            if !send_raw(
                &tx,
                RawListenerEvent::Tick {
                    epoch: active_epoch,
                    dt_ms: FALLBACK_DT_MS,
                },
            ) {
                return;
            }
            continue;
        }
        let energy = frame_rms(&chunk);
        let dt_ms = ((chunk.len() as u64 * 1000) / rate as u64).max(1);
        segment.extend_from_slice(&chunk);
        let event = match endpointer.step(energy, dt_ms) {
            Some(EndpointEvent::SpeechOnset) => RawListenerEvent::SpeechOnset {
                epoch: active_epoch,
            },
            Some(EndpointEvent::SegmentClosed) => RawListenerEvent::Segment {
                epoch: active_epoch,
                pcm: std::mem::take(&mut segment),
                rate,
            },
            None => RawListenerEvent::Tick {
                epoch: active_epoch,
                dt_ms,
            },
        };
        if !send_raw(&tx, event) {
            return;
        }
    }
}

fn inference_worker(
    provider: String,
    model_dir: PathBuf,
    stop: Arc<AtomicBool>,
    current_epoch: Arc<AtomicU64>,
    rx: Receiver<RawListenerEvent>,
    tx: std::sync::mpsc::Sender<ListenerEvent>,
) {
    let mut transcriber = LocalTranscriber::for_provider(&provider, model_dir);
    while !stop.load(Ordering::SeqCst) {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let event_epoch = match &event {
            RawListenerEvent::Tick { epoch, .. }
            | RawListenerEvent::SpeechOnset { epoch }
            | RawListenerEvent::Segment { epoch, .. } => *epoch,
        };
        if event_epoch != current_epoch.load(Ordering::SeqCst) {
            continue;
        }
        let event = match event {
            RawListenerEvent::Tick { epoch, dt_ms } => ListenerEvent::Tick { epoch, dt_ms },
            RawListenerEvent::SpeechOnset { epoch } => ListenerEvent::SpeechOnset { epoch },
            RawListenerEvent::Segment { epoch, pcm, rate } => {
                let pcm16 = resample_to_16k(&pcm, rate);
                let text = transcriber.transcribe_pcm_16k(&pcm16).unwrap_or_default();
                ListenerEvent::Segment { epoch, text }
            }
        };
        if tx.send(event).is_err() {
            return;
        }
    }
}

/// The cross-thread engine state a [`Listener`] needs, bundled into one struct so
/// [`Listener::new`] takes a single value instead of re-flattening the SAME four
/// handles the `Engine`/daemon already stores individually at both call sites
/// (`engine::Engine::reload` and `boot::engine_run`).
pub struct ListenerShared {
    /// Shared dictation buffer + the recording flag — driven so the SAME confirm
    /// pill shows the live hands-free transcript (start word → pill → submit/cancel).
    pub paste: PasteState,
    pub stt_active: Arc<AtomicBool>,
    /// The engine TTS queue, so a hands-free SUBMIT can drop this window's pending
    /// speech per `input_clears`. `None` in tests.
    pub ttsq: Option<Arc<TtsQueue>>,
    /// The shared status-push gate: a hands-free recording start/stop bumps it so a
    /// blocked `WaitModelStatus` sees `stt_active` flip immediately (the confirm pill
    /// follows the same signal the engine's PTT path publishes). `None` in tests.
    pub gate: Option<Arc<StatusGate>>,
}

/// The hands-free listener. Generic over the platform so it can share the engine-owned
/// `Rc<P>` while its provider-aware audio/inference worker remains off-thread.
pub struct Listener<P: KeyInjector + FrontmostWindow> {
    plat: Rc<P>,
    worker: Option<ListenerWorker>,
    turn: TurnLogic,
    /// Shared dictation buffer + the recording flag — driven so the SAME confirm pill
    /// shows the live hands-free transcript (start word → pill → submit/cancel).
    paste: PasteState,
    stt_active: Arc<AtomicBool>,
    /// Parakeet model present at construction — false ⇒ the loop no-ops (logged).
    available: bool,
    /// Delay (ms) between the clipboard paste and the Enter keypress on submit, so
    /// the async paste settles before Enter arrives. Mirrors the engine's Caps path.
    /// Live-updated by `set_paste_submit_delay_ms` — deliberately NOT baked into
    /// `endpointer`/`turn` at construction, so changing it doesn't need a rebuild.
    paste_submit_delay_ms: u64,
    /// A confirmed submit's Enter press, deferred by `paste_submit_delay_ms`. Polled
    /// once per [`tick`] via [`crate::timer::deferred_ready`] instead of blocking
    /// this thread with `std::thread::sleep` — mirrors `Engine::pending_enter_at`.
    pending_enter_at: Option<Instant>,
    /// The engine TTS queue, so a hands-free SUBMIT can drop this window's pending
    /// speech per `input_clears`. `None` in tests.
    ttsq: Option<Arc<TtsQueue>>,
    /// The shared status-push gate: a hands-free recording start/stop bumps it so a
    /// blocked `WaitModelStatus` sees `stt_active` flip immediately (the confirm pill
    /// follows the same signal the engine's PTT path publishes). `None` in tests.
    gate: Option<Arc<StatusGate>>,
    /// The resolved STT provider token this listener was built with (see
    /// `config_gate::helper_stt_provider`) — retained ONLY so `Engine::reload`'s
    /// rebuild-on-`stt_changed` trigger is observable/testable even when `available` is
    /// false (no live model on the test host); production code never reads it back
    /// (`ListenerWorker::spawn` already took its own copy at construction).
    #[cfg(test)]
    provider: String,
}

impl<P: KeyInjector + FrontmostWindow> Listener<P> {
    /// Build a listener from the live config. Cheap — the Parakeet model loads
    /// lazily on the first transcription, and the mic opens on the first idle tick.
    pub fn new(cfg: &VoiceConfig, plat: Rc<P>, model_dir: PathBuf, shared: ListenerShared) -> Self {
        let ListenerShared {
            paste,
            stt_active,
            ttsq,
            gate,
        } = shared;
        let available = crate::config_gate::local_stt_available(cfg);
        let provider = crate::config_gate::helper_stt_provider(cfg);
        let hf = &cfg.hands_free;
        if !available {
            log::warn!(
                target: "engine",
                "always-listening needs the selected local STT backend to be ready; \
                 the loop is idle until it becomes available"
            );
        } else {
            log::info!(
                target: "engine",
                "always-listening ENABLED (start={:?} submit={:?} cancel={:?} \
                 confirm={}ms endpoint={}ms)",
                hf.start, hf.submit, hf.cancel, cfg.submit_confirm_ms, cfg.endpoint_silence_ms
            );
        }
        Self {
            plat,
            worker: available
                .then(|| ListenerWorker::spawn(provider, model_dir, cfg.endpoint_silence_ms)),
            turn: TurnLogic::new(hf, cfg.submit_confirm_ms),
            paste,
            stt_active,
            available,
            paste_submit_delay_ms: cfg.paste_submit_delay_ms,
            pending_enter_at: None,
            ttsq,
            gate,
            #[cfg(test)]
            provider: provider.to_string(),
        }
    }

    /// Live-update the paste→Enter delay without rebuilding the listener (see the
    /// field's doc) — called from `Engine::reload` on every config apply.
    pub(crate) fn set_paste_submit_delay_ms(&mut self, ms: u64) {
        self.paste_submit_delay_ms = ms;
    }

    /// The resolved STT provider token this listener instance was built with — see the
    /// field's doc.
    #[cfg(test)]
    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    /// Test-only constructor: skips the Parakeet-availability probe and starts with no
    /// open mic (`capture: None`, matching every test's inability to drive real audio
    /// hardware) so `sync_pill`/`exec`/`gate_off` are exercised directly. `ttsq` is
    /// always `None` here (mirrors `Listener::new`'s "`None` in tests" contract); `gate`
    /// is caller-supplied so gate-bump tests can pass a real `StatusGate`.
    #[cfg(test)]
    fn for_test(plat: Rc<P>, turn: TurnLogic, gate: Option<Arc<StatusGate>>) -> Self {
        Self {
            plat,
            worker: None,
            turn,
            paste: Arc::new(std::sync::Mutex::new(crate::PasteBuf::default())),
            stt_active: Arc::new(AtomicBool::new(false)),
            available: true,
            paste_submit_delay_ms: 0,
            pending_enter_at: None,
            ttsq: None,
            gate,
            provider: String::new(),
        }
    }

    /// One poll tick. `tts_busy` is the half-duplex play-gate (queue speaking or
    /// pending): when true the mic stays closed so speech never feeds back and the
    /// queue can play; when false the mic is open and we drive the VAD + turn loop.
    /// `cancel_current`/`cancel_other` mirror the live `input_clears` config.
    pub fn tick(&mut self, tts_busy: bool, cancel_current: bool, cancel_other: bool) {
        // A submit's deferred Enter must still fire even if a reply starts speaking
        // (tts_busy) during the delay window, so poll it before the play-gate below.
        self.check_pending_enter();
        if tts_busy {
            self.gate_off();
            return;
        }
        if !self.available {
            return;
        }
        let Some(worker) = &self.worker else { return };
        worker.set_enabled(true);
        let epoch = worker.epoch.load(Ordering::SeqCst);
        let events: Vec<_> = worker.rx.try_iter().collect();
        for event in events {
            let event_epoch = match &event {
                ListenerEvent::Tick { epoch, .. }
                | ListenerEvent::SpeechOnset { epoch }
                | ListenerEvent::Segment { epoch, .. } => *epoch,
            };
            if event_epoch != epoch {
                continue;
            }
            let actions = match event {
                ListenerEvent::Tick { dt_ms, .. } => self.turn.on_tick(dt_ms),
                ListenerEvent::SpeechOnset { .. } => self.turn.on_speech_onset(),
                ListenerEvent::Segment { text, .. } => self.turn.on_segment(&text),
            };
            self.sync_pill();
            for action in actions {
                self.exec(action, cancel_current, cancel_other);
            }
        }
    }

    /// Drive the shared dictation buffer + recording flag from the turn state so the
    /// SAME confirm pill shows the live hands-free transcript (start word → submit/cancel).
    fn sync_pill(&self) {
        let capturing = self.turn.capturing();
        // Push a recording start/stop to a blocked `WaitModelStatus` immediately; only on
        // a real transition so the per-tick sync never wakes waiters while idle.
        if self.stt_active.swap(capturing, Ordering::SeqCst) != capturing
            && let Some(gate) = &self.gate
        {
            gate.bump();
        }
        if let Ok(mut p) = self.paste.lock() {
            if capturing {
                p.partial = self.turn.buffer().to_string();
                if p.target.is_none() {
                    p.target = self.plat.frontmost_app_name();
                }
            } else {
                p.partial.clear();
                // Equivalent to the old pending/final_ready clear: the Caps PTT's
                // armed confirm state is always torn down before Always mode runs
                // (`reload` calls `teardown_hold` before building the listener), so
                // `final_state` here is never `Armed` — this only wipes a stale final.
                p.final_state = crate::FinalState::Idle;
                p.target = None;
            }
        }
    }

    /// Close the mic and discard any in-flight utterance (entering the TTS
    /// play-gate, or stopping the loop). Leaves the turn state intact — after a
    /// submit the turn is already reset, and TTS only plays post-submit.
    fn gate_off(&mut self) {
        if let Some(worker) = &self.worker {
            worker.set_enabled(false);
            while worker.rx.try_recv().is_ok() {}
        }
    }

    /// Execute one turn action into the focused prompt. Like `Engine::confirm_paste`,
    /// there's no focus refusal here: the stop-word confirm (mirroring the Caps confirm
    /// tap) IS the deliberate gate, so once armed a submit always pastes wherever is
    /// focused.
    fn exec(&mut self, action: TurnAction, cancel_current: bool, cancel_other: bool) {
        match action {
            // Paste the whole captured text + Enter — ALWAYS, like the Caps path's
            // `confirm_paste`. The stop-word-plus-confirm-silence gesture that got us here
            // is itself the user's explicit intent signal, so it pastes into whatever is
            // focused, never silently refused for focus.
            TurnAction::SubmitText(text) => {
                self.plat.type_text(&text);
                if let Some(q) = &self.ttsq {
                    // Apply `input_clears` immediately at submit, atomically per scope
                    // (see `cancel_for_submit`'s doc for why both scopes must share one
                    // resolved "active") — must not wait on the deferred Enter below.
                    // Only on SubmitText — never on Cancel.
                    q.cancel_for_submit(q.active_session(), cancel_current, cancel_other);
                }
                // Let the async paste settle before Enter lands — deferred via a polled
                // timer (`check_pending_enter`, run from `tick`) rather than a blocking
                // sleep, since this runs on the engine's single tick thread.
                if self.paste_submit_delay_ms > 0 {
                    self.pending_enter_at = Some(Instant::now());
                } else {
                    self.press_deferred_enter();
                }
            }
            // Discard: nothing to inject (sync_pill already hid the pill), and NO cancel —
            // cancelling your dictation must not silence the in-flight reply.
            TurnAction::Cancel => {}
        }
    }

    /// Fire a deferred submit's Enter once `paste_submit_delay_ms` has elapsed. Run
    /// once per [`tick`], mirrors `Engine::check_pending_enter`.
    fn check_pending_enter(&mut self) {
        if crate::timer::deferred_ready(&mut self.pending_enter_at, self.paste_submit_delay_ms) {
            self.press_deferred_enter();
        }
    }

    /// Press Enter for a confirmed submit and mark it so `MarkActive` doesn't
    /// double-count its own echo as a separate submit. Called either immediately
    /// (zero delay) or once `check_pending_enter`'s timer elapses.
    fn press_deferred_enter(&mut self) {
        self.plat.press_enter();
        if let Some(q) = &self.ttsq {
            // Mark the voice submit's auto-Enter so `MarkActive` de-dups it as this
            // same submit rather than a separate one. Must stay pinned to the REAL
            // Enter keystroke (not the earlier paste/cancel step) since
            // `note_voice_submit`'s de-dup window is a few seconds wide.
            q.note_voice_submit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_platform::{FrontmostWindow, KeyInjector};
    use std::cell::Cell;

    /// Minimal `KeyInjector`/`FrontmostWindow` fake — mirrors `engine.rs`'s
    /// `MockPlatform` pattern, scoped to just what `exec`/`sync_pill` touch.
    #[derive(Default)]
    struct MockPlatform {
        terminal_frontmost: Cell<bool>,
        type_text_calls: Cell<u32>,
        press_enter_calls: Cell<u32>,
    }

    impl KeyInjector for MockPlatform {
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
    }

    fn turn() -> TurnLogic {
        TurnLogic::new(&ds_config::HandsFreePhrases::default(), 1000)
    }

    #[test]
    fn sync_pill_mirrors_capturing_edge_and_bumps_gate_once() {
        let gate = StatusGate::new();
        let plat = Rc::new(MockPlatform::default());
        let mut l = Listener::for_test(plat, turn(), Some(gate.clone()));

        // Idle → sync_pill: no transition, gate untouched.
        l.sync_pill();
        assert_eq!(gate.seq(), 0);
        assert!(!l.stt_active.load(Ordering::SeqCst));

        // Start word opens the pill — a real capturing edge.
        l.turn.on_segment("hey computer add a button");
        assert!(l.turn.capturing());
        l.sync_pill();
        assert!(l.stt_active.load(Ordering::SeqCst));
        assert_eq!(gate.seq(), 1, "capturing edge bumps the gate once");
        assert_eq!(l.paste.lock().unwrap().partial, "add a button");

        // Still capturing — a repeated sync_pill must NOT re-bump the gate.
        l.sync_pill();
        assert_eq!(gate.seq(), 1);

        // Cancel closes the turn — the idle edge clears the pill + bumps again.
        l.turn.on_segment("cancel");
        l.turn.on_tick(1000);
        assert!(!l.turn.capturing());
        l.sync_pill();
        assert!(!l.stt_active.load(Ordering::SeqCst));
        assert_eq!(gate.seq(), 2, "idle edge bumps the gate once more");
        assert_eq!(l.paste.lock().unwrap().partial, "");
    }

    #[test]
    fn exec_always_pastes_submit_regardless_of_focus() {
        let plat = Rc::new(MockPlatform::default());
        let mut l = Listener::for_test(plat.clone(), turn(), None);

        // Not frontmost → still pastes and presses Enter: the stop-word confirm
        // itself is the gate, mirroring `confirm_paste`'s unconditional paste.
        plat.terminal_frontmost.set(false);
        l.exec(TurnAction::SubmitText("hello".into()), false, false);
        assert_eq!(plat.type_text_calls.get(), 1);
        assert_eq!(plat.press_enter_calls.get(), 1);

        // Frontmost → also pastes the text and presses Enter.
        plat.terminal_frontmost.set(true);
        l.exec(TurnAction::SubmitText("hello".into()), false, false);
        assert_eq!(plat.type_text_calls.get(), 2);
        assert_eq!(plat.press_enter_calls.get(), 2);
    }

    #[test]
    fn exec_cancel_is_always_a_noop() {
        let plat = Rc::new(MockPlatform::default());
        plat.terminal_frontmost.set(true); // even when focus WOULD allow a paste
        let mut l = Listener::for_test(plat.clone(), turn(), None);

        l.exec(TurnAction::Cancel, false, false);
        assert_eq!(plat.type_text_calls.get(), 0);
        assert_eq!(plat.press_enter_calls.get(), 0);
    }

    #[test]
    fn gate_off_is_a_noop_without_an_open_capture() {
        let plat = Rc::new(MockPlatform::default());
        let mut l = Listener::for_test(plat, turn(), None);

        l.gate_off(); // no worker in tests: safe no-op
        assert!(l.worker.is_none());
    }
}
