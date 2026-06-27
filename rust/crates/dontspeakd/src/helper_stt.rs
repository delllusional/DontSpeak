//! `HelperStt` — the dictation `Stt` engine backed by the warm helper child.
//!
//! Consolidation: Parakeet dictation no longer loads the model in-process. On
//! Caps-ON `start()` spawns a thread that tells the helper to `listen` (it opens
//! the mic + transcribes), streaming PARTIAL lines into the shared dictation
//! buffer for the live confirm panel; on Caps-OFF `stop()` ends the listen,
//! joins the FINAL transcript, and DEPOSITS it as `pending` for confirmation —
//! it no longer pastes directly. Confirm-before-paste is unconditional: the
//! ENGINE pastes `pending` on the user's confirm tap (focus-gated) and discards
//! it on cancel. `abort()` (§F long-press reset) ends the listen and clears the
//! buffer (no paste).
//!
//! The model lives in the one warm helper, not the engine; this type owns no
//! platform handle anymore (the engine performs the gated paste).

use std::sync::Arc;
use std::thread::JoinHandle;

use ds_stt::Stt;

use crate::tts::TtsManager;
use crate::{PasteBuf, PasteState};

/// Deposit a finalized transcript into the shared dictation buffer as `pending` (the
/// engine pastes it, focus-gated), but ONLY if the buffer is still on the session
/// `epoch` this listen started under. `stop` runs the slow Parakeet final pass on a
/// detached joiner, so by the time it lands a later `start`/`abort`/`teardown`/`cancel`
/// may have advanced the epoch — depositing then would repopulate a cleared buffer or
/// clobber a newer session's live partials. An empty transcript deposits no `pending`
/// but still sets `final_ready` so the deferred-submit machinery disarms. Returns
/// whether the deposit was applied (the epoch matched). Extracted from the `stop` joiner
/// so the guard is unit-testable without spawning threads.
fn deposit_final(p: &mut PasteBuf, epoch: u64, text: &str) -> bool {
    if p.epoch != epoch {
        return false;
    }
    p.partial.clear();
    let trimmed = text.trim();
    p.pending = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    p.final_ready = true;
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
}

impl HelperStt {
    pub fn new(tts: Arc<TtsManager>, paste: PasteState) -> Self {
        Self {
            tts,
            paste,
            handle: None,
            epoch: 0,
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
        // the buffer still belongs to it when its (slow) final lands.
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.epoch = p.epoch.wrapping_add(1);
            self.epoch = p.epoch;
        }
        let tts = self.tts.clone();
        let paste = self.paste.clone();
        // The listen blocks until stop()/the helper finishes; run it off the poll
        // thread. Each PARTIAL is mirrored into the shared buffer so the confirm
        // panel shows the running transcript live.
        self.handle = Some(std::thread::spawn(move || {
            tts.listen(&mut |partial| {
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
        // stays responsive (pill keeps updating, the deferred submit fires on `final_ready`).
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
        // §F long-press reset: end the listen and DISCARD (no paste, no pending). The
        // reset path is not latency-critical, so join inline and clear everything.
        self.tts.stop_listen();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Ok(mut p) = self.paste.lock() {
            p.partial.clear();
            p.pending = None;
            p.final_ready = false;
            // Advance the session epoch so any earlier detached `stop` joiner still in
            // its final pass is invalidated and can't deposit into this cleared buffer.
            p.epoch = p.epoch.wrapping_add(1);
        }
    }

    fn is_available(&self) -> bool {
        // Provider-aware: ANE (Core ML) needs no ONNX model files, so the raw
        // `parakeet_present()` would wrongly report unavailable on that path.
        ds_config::Paths::resolve()
            .map(|p| {
                crate::config_gate::parakeet_present_for(&ds_config::VoiceConfig::load(&p))
            })
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
    use crate::PasteBuf;

    /// Happy path: the session epoch is unchanged when the (async) final lands, so the
    /// transcript deposits as `pending` for the deferred submit.
    #[test]
    fn deposits_when_epoch_matches() {
        let mut p = PasteBuf {
            epoch: 5,
            partial: "live".into(),
            ..Default::default()
        };
        assert!(deposit_final(&mut p, 5, "  hello world  "));
        assert_eq!(p.pending.as_deref(), Some("hello world")); // trimmed
        assert!(p.final_ready);
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
        assert!(p.pending.is_none(), "stale final must not deposit");
        assert!(!p.final_ready, "stale final must not signal ready");
        assert_eq!(p.partial, "newer session partial", "live partial untouched");
    }

    /// An empty/whitespace final deposits no `pending` but still flags `final_ready` so
    /// the armed deferred-submit disarms instead of hanging.
    #[test]
    fn empty_final_signals_ready_without_pending() {
        let mut p = PasteBuf {
            epoch: 1,
            ..Default::default()
        };
        assert!(deposit_final(&mut p, 1, "   "));
        assert!(p.pending.is_none());
        assert!(p.final_ready);
    }
}
