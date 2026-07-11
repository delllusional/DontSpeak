//! `HelperStt` — the dictation `Stt` engine backed by the warm helper child.
//!
//! Consolidation: Parakeet dictation no longer loads the model in-process. On
//! Caps-ON `start()` spawns a thread that tells the helper to `listen` (it opens
//! the mic + transcribes), streaming PARTIAL lines into the shared dictation
//! buffer for the live confirm panel; on Caps-OFF `stop()` ends the listen,
//! joins the FINAL transcript, and DEPOSITS it as `FinalState::Ready` for
//! confirmation — it no longer pastes directly. Confirm-before-paste is
//! unconditional: the ENGINE pastes the landed final on the user's confirm tap
//! (focus-gated) and discards it on cancel. `abort()` (§F long-press reset) ends
//! the listen and clears the buffer (no paste).
//!
//! The model lives in the one warm helper, not the engine; this type owns no
//! platform handle anymore (the engine performs the gated paste).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use ds_stt::Stt;

use crate::tts::TtsManager;
use crate::{FinalState, PasteBuf, PasteState};

/// Deposit a finalized transcript into the shared dictation buffer as
/// `FinalState::Ready` (the engine pastes it, focus-gated), but ONLY if the buffer is
/// still on the session `epoch` this listen started under. `stop` runs the slow
/// Parakeet final pass on a detached joiner, so by the time it lands a later
/// `start`/`abort`/`teardown`/`cancel` may have advanced the epoch — depositing then
/// would repopulate a cleared buffer or clobber a newer session's live partials. An
/// empty transcript deposits `FinalState::Empty` so the deferred-submit machinery
/// disarms. Returns whether the deposit was applied (the epoch matched). Extracted
/// from the `stop` joiner so the guard is unit-testable without spawning threads.
fn deposit_final(p: &mut PasteBuf, epoch: u64, text: &str) -> bool {
    if p.epoch != epoch {
        return false;
    }
    p.partial.clear();
    let trimmed = text.trim();
    p.final_state = if trimmed.is_empty() {
        FinalState::Empty
    } else {
        FinalState::Ready(trimmed.to_string())
    };
    true
}

pub struct HelperStt {
    tts: Arc<TtsManager>,
    /// Shared dictation preview buffer: live partials (while recording) + the
    /// finalized transcript awaiting the user's confirm tap. The engine reads it
    /// for `model_status` and performs the gated paste on confirm.
    paste: PasteState,
    /// The in-flight listen session's thread (returns the FINAL transcript).
    handle: Option<JoinHandle<std::io::Result<String>>>,
    /// The `PasteBuf::epoch` this session is recording under, stamped at `start`. The
    /// detached `stop` joiner re-checks it under the lock before depositing the final,
    /// so a slow final pass can't land in a buffer a later `start`/`abort`/teardown/
    /// cancel has already advanced past (see `PasteBuf::epoch`).
    epoch: u64,
    /// This session's early-stop flag, fresh per `start()` and passed into
    /// `TtsManager::listen_cancellable`: `stop`/`abort` set it BEFORE the spawned thread
    /// is guaranteed to have reached the helper's `listen()` call, so a Caps tap-then-
    /// release faster than thread scheduling can't lose the stop the way plain
    /// `stop_listen()` alone would (it has nothing to cancel until a generation is
    /// published, which only happens once that thread actually runs).
    stop_requested: Arc<AtomicBool>,
}

impl HelperStt {
    pub fn new(tts: Arc<TtsManager>, paste: PasteState) -> Self {
        Self {
            tts,
            paste,
            handle: None,
            epoch: 0,
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Stt for HelperStt {
    fn start(&mut self) -> bool {
        if self.handle.is_some() {
            return true; // already listening (stray double-start)
        }
        // Fresh capture: clear any stale preview text so the panel starts empty, and
        // open a new session epoch so this session's `stop` joiner can recognize whether
        // the buffer still belongs to it when its (slow) final lands. Deliberately does
        // NOT touch `final_state`: the engine's `start_recording` already reset it to
        // `Idle` under its own lock before calling this.
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.epoch = p.epoch.wrapping_add(1);
            self.epoch = p.epoch;
        }
        let tts = self.tts.clone();
        let paste = self.paste.clone();
        // Fresh per session — see the field doc on `stop_requested`.
        let stop_requested = Arc::new(AtomicBool::new(false));
        self.stop_requested = stop_requested.clone();
        // The listen blocks until stop()/the helper finishes; run it off the poll
        // thread. Each PARTIAL is mirrored into the shared buffer so the confirm
        // panel shows the running transcript live.
        self.handle = Some(std::thread::spawn(move || {
            tts.listen_cancellable(&stop_requested, &mut |partial| {
                if let Ok(mut p) = paste.lock() {
                    p.partial = partial.to_string();
                }
            })
        }));
        true
    }

    fn stop(&mut self) {
        // End the helper's listen (the `lstop` op) WITHOUT cancelling a concurrent
        // reply — full-duplex coexist lets dictation and TTS overlap. The final
        // Parakeet pass is SLOW (seconds of audio re-run through the model), so do
        // NOT join here — that would freeze the engine's poll thread. Instead a short
        // background joiner waits for it and deposits the result, while the poll loop
        // stays responsive (pill keeps updating, the deferred submit fires once the
        // deposited `final_state` reads `Ready`/`Empty`).
        self.stop_requested.store(true, Ordering::SeqCst);
        self.tts.stop_listen();
        let Some(handle) = self.handle.take() else {
            return;
        };
        let paste = self.paste.clone();
        let epoch = self.epoch;
        std::thread::spawn(move || {
            let text = match handle.join() {
                Ok(Ok(t)) => t,
                _ => String::new(),
            };
            if let Ok(mut p) = paste.lock() {
                deposit_final(&mut p, epoch, &text);
            }
        });
    }

    fn abort(&mut self) {
        // §F long-press reset: end the listen and DISCARD (no paste, no pending).
        self.stop_requested.store(true, Ordering::SeqCst);
        self.tts.stop_listen();
        if let Some(handle) = self.handle.take() {
            // Bounded wait, NOT an untimed `join()`: this runs on the daemon's single
            // poll thread, which `ds_engine_stop`'s quit path also joins — a live-but-
            // hung (not crashed) ds-helper child during dictation must never be able to
            // freeze the whole poll loop / app-quit path. `JoinHandle` has no timed-join,
            // so poll `is_finished()` against a deadline instead. If the thread is still
            // wedged when the deadline passes, give up waiting and drop the handle
            // (detaches the thread — it keeps running, or stays wedged, on its own) so
            // `abort()` always returns promptly either way.
            const JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
            const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);
            let deadline = std::time::Instant::now() + JOIN_TIMEOUT;
            let mut joined = false;
            while std::time::Instant::now() < deadline {
                if handle.is_finished() {
                    let _ = handle.join();
                    joined = true;
                    break;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            if !joined {
                log::warn!(
                    target: "engine",
                    "HelperStt::abort() gave up waiting {JOIN_TIMEOUT:?} for the \
                     listen thread to finish; detaching it instead of blocking"
                );
            }
        }
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            // Straight to `Idle` (the engine caller disarms its own side a few
            // instructions later on the same thread — under the old fields it also
            // cleared the mirror then, so this collapses only a microsecond-scale
            // window where a concurrent status read saw ("", true) vs ("", false)).
            p.final_state = FinalState::Idle;
            // Advance the session epoch so any earlier detached `stop` joiner still in
            // its final pass is invalidated and can't deposit into this cleared buffer.
            p.epoch = p.epoch.wrapping_add(1);
        }
    }

    fn is_available(&self) -> bool {
        // Provider-aware: ANE (Core ML) needs no ONNX model files, so the raw
        // `parakeet_present()` would wrongly report unavailable on that path.
        ds_config::Paths::resolve()
            .map(|p| crate::config_gate::parakeet_present_for(&ds_config::VoiceConfig::load(&p)))
            .unwrap_or(false)
    }

    fn kind(&self) -> &'static str {
        "parakeet-helper"
    }

    fn defers_paste(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::deposit_final;
    use crate::{FinalState, PasteBuf};

    /// Happy path: the session epoch is unchanged when the (async) final lands, so the
    /// transcript deposits as `Ready` for the deferred submit.
    #[test]
    fn deposits_when_epoch_matches() {
        let mut p = PasteBuf {
            epoch: 5,
            partial: "live".into(),
            ..Default::default()
        };
        assert!(deposit_final(&mut p, 5, "  hello world  "));
        assert_eq!(p.final_state, FinalState::Ready("hello world".into())); // trimmed
        assert!(p.partial.is_empty(), "partial cleared on deposit");
    }

    /// The race the epoch guard closes: a teardown/cancel/new-start advanced the epoch
    /// while the slow final pass ran. The stale final must NOT land — neither clobbering
    /// a cleared buffer nor a newer session's live partials.
    #[test]
    fn drops_stale_final_when_epoch_advanced() {
        let mut p = PasteBuf {
            epoch: 6, // a newer session owns the buffer now
            partial: "newer session partial".into(),
            ..Default::default()
        };
        assert!(!deposit_final(&mut p, 5, "stale final")); // joiner started under epoch 5
        assert_eq!(
            p.final_state,
            FinalState::Idle,
            "stale final must not deposit or signal ready"
        );
        assert_eq!(p.partial, "newer session partial", "live partial untouched");
    }

    /// An empty/whitespace final deposits `Empty` (never `Ready`) so the armed
    /// deferred-submit disarms instead of hanging.
    #[test]
    fn empty_final_signals_ready_without_pending() {
        let mut p = PasteBuf {
            epoch: 1,
            ..Default::default()
        };
        assert!(deposit_final(&mut p, 1, "   "));
        assert_eq!(p.final_state, FinalState::Empty);
    }
}
