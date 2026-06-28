//! TestSession — the engine's "test recognition" / live-transcription owner.
//!
//! It runs the SAME local Parakeet engine the dictation path uses, through the
//! warm helper child (not in-process). It streams live `Partial` lines and a
//! terminal `Transcript`.
//!
//! Flow: `run()` (on the streaming connection's thread) tells the helper to
//! `listen` and relays its `PARTIAL`s as [`Response::Partial`]; `stop()` (from a
//! SECOND connection) ends the helper's listen, after which `run()` emits the
//! final [`Response::Transcript`].

use std::sync::Arc;

use ds_ipc::Response;

use crate::tts::TtsManager;

pub struct TestSession {
    /// The warm helper that hosts both engines (Kokoro + Parakeet). STT runs there.
    tts: Arc<TtsManager>,
}

impl TestSession {
    pub fn new(tts: Arc<TtsManager>) -> Self {
        Self { tts }
    }

    /// Run a recognition session, streaming responses via `emit`. Blocks on the
    /// calling (connection) thread until `stop()` ends the helper's listen.
    pub fn run(&self, emit: &mut dyn FnMut(&Response)) {
        // Provider-aware gate: on the ANE (Core ML) path the ONNX model files are never
        // downloaded, so the raw ONNX-only `parakeet_present()` would wrongly block here.
        let parakeet_ok = ds_config::Paths::resolve()
            .map(|p| crate::config_gate::parakeet_present_for(&ds_config::VoiceConfig::load(&p)))
            .unwrap_or(false);
        if !parakeet_ok {
            emit(&Response::error(
                "Parakeet model not installed — download it in Settings",
            ));
            return;
        }
        emit(&Response::Listening);
        // The partial callback borrows `emit` only for the listen call; `emit` is
        // free again for the terminal response below.
        let result = {
            let mut on_partial = |t: &str| {
                emit(&Response::Partial {
                    text: t.to_string(),
                })
            };
            self.tts.listen(&mut on_partial)
        };
        match result {
            Ok(text) => emit(&Response::Transcript { text }),
            Err(e) => emit(&Response::error(format!("test recognition: {e}"))),
        }
    }

    /// Stop the active session: end the helper's listen so `run()` finishes and
    /// emits its `Transcript`. No-op if none is active. Uses `lstop` (not `stop`)
    /// so it ends the listen in BOTH modes — in full-duplex a plain `stop` cancels
    /// a speak but leaves the concurrent listen running.
    pub fn stop(&self) {
        self.tts.stop_listen();
    }
}
