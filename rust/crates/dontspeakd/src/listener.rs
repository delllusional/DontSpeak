//! Always-listening (hands-free) runtime glue — the I/O layer over the pure
//! [`crate::listen`] state machines. See docs/ALWAYS-LISTENING.md.
//!
//! Driven once per engine poll tick. Owns the mic capture, the Parakeet
//! transcriber, and the platform key-injection; feeds the pure Endpointer +
//! TurnLogic and executes their `Paste`/`Submit` actions into the focused prompt.
//!
//! Half-duplex play-gate: while the TTS queue is busy (speaking or pending) the
//! mic is CLOSED; when it goes idle the mic reopens. `!Send` (holds the cpal
//! stream + an `Rc` to the platform) — lives on the engine's single poll thread.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_config::VoiceConfig;
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::{Capture, ParakeetTranscriber, resample_to_16k};

use crate::PasteState;
use crate::listen::{
    DEFAULT_ENERGY_THRESHOLD, EndpointEvent, Endpointer, TurnAction, TurnLogic, frame_rms,
};
use crate::status::StatusGate;
use crate::ttsq::TtsQueue;

/// Fallback frame duration (ms) for a tick that drained no new audio — matches
/// the engine poll interval so the confirm-window timer still advances.
const FALLBACK_DT_MS: u64 = 30;

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

/// The hands-free listener. Generic over the platform so it can share the
/// engine-owned `Rc<P>` (the macOS paste/Enter path is `!Send`).
pub struct Listener<P: KeyInjector + FrontmostWindow> {
    plat: Rc<P>,
    transcriber: ParakeetTranscriber,
    /// `Some` while the mic is open (idle TTS); `None` while gated off (TTS busy).
    capture: Option<Capture>,
    endpointer: Endpointer,
    turn: TurnLogic,
    /// Shared dictation buffer + the recording flag — driven so the SAME confirm pill
    /// shows the live hands-free transcript (start word → pill → submit/cancel).
    paste: PasteState,
    stt_active: Arc<AtomicBool>,
    /// The current utterance's PCM at the device's native rate, resampled to
    /// 16 kHz only when the segment closes.
    segment: Vec<f32>,
    input_rate: u32,
    /// Parakeet model present at construction — false ⇒ the loop no-ops (logged).
    available: bool,
    /// The engine TTS queue, so a hands-free SUBMIT can drop this window's pending
    /// speech per `input_clears`. `None` in tests.
    ttsq: Option<Arc<TtsQueue>>,
    /// The shared status-push gate: a hands-free recording start/stop bumps it so a
    /// blocked `WaitModelStatus` sees `stt_active` flip immediately (the confirm pill
    /// follows the same signal the engine's PTT path publishes). `None` in tests.
    gate: Option<Arc<StatusGate>>,
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
        let available = crate::config_gate::parakeet_present_for(cfg);
        let hf = &cfg.hands_free;
        if !available {
            crate::log(
                "WARN: always-listening needs the Parakeet STT model — \
                 download it in Settings › Models; the loop is idle until then",
            );
        } else {
            crate::log(&format!(
                "always-listening ENABLED (start={:?} submit={:?} cancel={:?} \
                 confirm={}ms endpoint={}ms)",
                hf.start, hf.submit, hf.cancel, cfg.submit_confirm_ms, cfg.endpoint_silence_ms
            ));
        }
        Self {
            plat,
            transcriber: ParakeetTranscriber::new(model_dir),
            capture: None,
            endpointer: Endpointer::new(DEFAULT_ENERGY_THRESHOLD, cfg.endpoint_silence_ms),
            turn: TurnLogic::new(hf, cfg.submit_confirm_ms),
            paste,
            stt_active,
            segment: Vec::new(),
            input_rate: 16_000,
            available,
            ttsq,
            gate,
        }
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
            transcriber: ParakeetTranscriber::new(PathBuf::new()),
            capture: None,
            endpointer: Endpointer::new(DEFAULT_ENERGY_THRESHOLD, 700),
            turn,
            paste: Arc::new(std::sync::Mutex::new(crate::PasteBuf::default())),
            stt_active: Arc::new(AtomicBool::new(false)),
            segment: Vec::new(),
            input_rate: 16_000,
            available: true,
            ttsq: None,
            gate,
        }
    }

    /// One poll tick. `tts_busy` is the half-duplex play-gate (queue speaking or
    /// pending): when true the mic stays closed so speech never feeds back and the
    /// queue can play; when false the mic is open and we drive the VAD + turn loop.
    /// `cancel_current`/`cancel_other` mirror the live `input_clears` config.
    pub fn tick(&mut self, tts_busy: bool, cancel_current: bool, cancel_other: bool) {
        if tts_busy {
            self.gate_off();
            return;
        }
        if !self.available {
            return;
        }
        // Ensure the mic is open; the first opening tick just primes the stream.
        if self.capture.is_none() {
            match Capture::open() {
                Ok(c) => {
                    self.input_rate = c.input_rate().max(1);
                    self.capture = Some(c);
                }
                Err(e) => crate::log(&format!("WARN: always-listen mic open: {e}")),
            }
            return;
        }

        let chunk = self.capture.as_ref().expect("capture open").drain_new();
        let (event, dt_ms) = if chunk.is_empty() {
            (None, FALLBACK_DT_MS)
        } else {
            let energy = frame_rms(&chunk);
            let dt = ((chunk.len() as u64 * 1000) / self.input_rate as u64).max(1);
            self.segment.extend_from_slice(&chunk);
            (self.endpointer.step(energy, dt), dt)
        };

        let actions = match event {
            // Speech resumed → cancel a pending submit (the stopword was content).
            Some(EndpointEvent::SpeechOnset) => self.turn.on_speech_onset(),
            // Utterance over → transcribe the buffered segment and feed the turn.
            Some(EndpointEvent::SegmentClosed) => {
                let pcm16 = resample_to_16k(&self.segment, self.input_rate);
                self.segment.clear();
                let text = self
                    .transcriber
                    .transcribe_pcm_16k(&pcm16)
                    .unwrap_or_default();
                self.turn.on_segment(&text)
            }
            // Steady silence → advance the stopword confirmation window.
            None => self.turn.on_tick(dt_ms),
        };

        // Mirror the turn state into the dictation pill (live buffer while capturing,
        // hidden otherwise), then execute any submit/cancel.
        self.sync_pill();
        for a in actions {
            self.exec(a, cancel_current, cancel_other);
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
        if self.capture.take().is_some() {
            self.segment.clear();
            self.endpointer.reset();
        }
    }

    /// Execute one turn action into the focused prompt. Like `Engine::confirm_paste`,
    /// there's no focus refusal here: the stop-word confirm (mirroring the Caps confirm
    /// tap) IS the deliberate gate, so once armed a submit always pastes wherever is
    /// focused.
    fn exec(&self, action: TurnAction, cancel_current: bool, cancel_other: bool) {
        match action {
            // Paste the whole captured text + Enter — ALWAYS, like the Caps path's
            // `confirm_paste`. The stop-word-plus-confirm-silence gesture that got us here
            // is itself the user's explicit intent signal, so it pastes into whatever is
            // focused, never silently refused for focus.
            TurnAction::SubmitText(text) => {
                self.plat.type_text(&text);
                self.plat.press_enter();
                if let Some(q) = &self.ttsq {
                    // Mark the voice submit's auto-Enter so `MarkActive` de-dups it as
                    // this same submit rather than a separate one. Then apply
                    // `input_clears` per scope, atomically (see `cancel_for_submit`'s
                    // doc for why both scopes must share one resolved "active").
                    // Only on SubmitText — never on Cancel.
                    q.note_voice_submit();
                    q.cancel_for_submit(q.active_session(), cancel_current, cancel_other);
                }
            }
            // Discard: nothing to inject (sync_pill already hid the pill), and NO cancel —
            // cancelling your dictation must not silence the in-flight reply.
            TurnAction::Cancel => {}
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
        let l = Listener::for_test(plat.clone(), turn(), None);

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
        let l = Listener::for_test(plat.clone(), turn(), None);

        l.exec(TurnAction::Cancel, false, false);
        assert_eq!(plat.type_text_calls.get(), 0);
        assert_eq!(plat.press_enter_calls.get(), 0);
    }

    #[test]
    fn gate_off_is_a_noop_without_an_open_capture() {
        let plat = Rc::new(MockPlatform::default());
        let mut l = Listener::for_test(plat, turn(), None);

        l.segment.push(0.5);
        l.gate_off(); // capture is already None → nothing to close
        assert_eq!(
            l.segment,
            vec![0.5],
            "no open capture ⇒ segment/endpointer are left untouched"
        );
    }
}
