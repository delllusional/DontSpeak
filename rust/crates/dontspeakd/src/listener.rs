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
    /// speech (`drop_speech_on` = voice_input/any_input). `None` in tests.
    ttsq: Option<Arc<TtsQueue>>,
    /// The shared status-push gate: a hands-free recording start/stop bumps it so a
    /// blocked `WaitModelStatus` sees `stt_active` flip immediately (the confirm pill
    /// follows the same signal the engine's PTT path publishes). `None` in tests.
    gate: Option<Arc<StatusGate>>,
}

impl<P: KeyInjector + FrontmostWindow> Listener<P> {
    /// Build a listener from the live config. Cheap — the Parakeet model loads
    /// lazily on the first transcription, and the mic opens on the first idle tick.
    pub fn new(
        cfg: &VoiceConfig,
        plat: Rc<P>,
        model_dir: PathBuf,
        paste: PasteState,
        stt_active: Arc<AtomicBool>,
        ttsq: Option<Arc<TtsQueue>>,
        gate: Option<Arc<StatusGate>>,
    ) -> Self {
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
            turn: TurnLogic::new(
                hf.start.clone(),
                hf.submit.clone(),
                hf.cancel.clone(),
                cfg.submit_confirm_ms,
            ),
            paste,
            stt_active,
            segment: Vec::new(),
            input_rate: 16_000,
            available,
            ttsq,
            gate,
        }
    }

    /// One poll tick. `tts_busy` is the half-duplex play-gate (queue speaking or
    /// pending): when true the mic stays closed so speech never feeds back and the
    /// queue can play; when false the mic is open and we drive the VAD + turn loop.
    pub fn tick(&mut self, tts_busy: bool, drop_on_voice_submit: bool) {
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
            self.exec(a, drop_on_voice_submit);
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
                p.pending = None;
                p.target = None;
                p.final_ready = false;
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

    /// Execute one turn action into the focused prompt, focus-gated like Parakeet
    /// PTT so a transcript or Enter never lands outside a terminal.
    fn exec(&self, action: TurnAction, drop_on_voice_submit: bool) {
        match action {
            // Paste the whole captured text + Enter — focus-gated to a terminal like
            // the Caps path, so a transcript never lands outside a prompt.
            TurnAction::SubmitText(text) => {
                if self.plat.terminal_frontmost() {
                    self.plat.type_text(&text);
                    self.plat.press_enter();
                    if let Some(q) = &self.ttsq {
                        // Mark the voice submit's auto-Enter so the keyboard-drop path de-dups
                        // it. Then, if `drop_speech_on` contains `voice`, drop this window's
                        // now-stale pending speech. Only on SubmitText — never on Cancel.
                        q.note_voice_submit();
                        if drop_on_voice_submit {
                            q.clear_active_session();
                        }
                    }
                }
            }
            // Discard: nothing to inject (sync_pill already hid the pill), and NO drop —
            // cancelling your dictation must not silence the in-flight reply.
            TurnAction::Cancel => {}
        }
    }
}
