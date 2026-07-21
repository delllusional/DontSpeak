//! Settings "test recognition": same warm-helper Parakeet as dictation.
//! `run()` (streaming [`Conn`]) listens + relays `Partial`; `stop()` (second conn)
//! ends listen so `run()` emits terminal `Transcript`. Write failures on `Conn`
//! also end the listen (client hang-up mid-session).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ds_ipc::{Conn, Response};

use crate::tts::TtsManager;

pub struct TestSession {
    /// Warm helper hosts both engines; STT runs there.
    tts: Arc<TtsManager>,
    /// Early-stop flag (same race as `HelperStt::stop_requested`): `run`/`stop` are
    /// different threads; reset at top of each `run` (long-lived instance; see `boot.rs`).
    stop_requested: Arc<AtomicBool>,
}

impl TestSession {
    pub fn new(tts: Arc<TtsManager>) -> Self {
        Self {
            tts,
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Run a recognition session, streaming responses over `conn`. Blocks on the
    /// calling (connection) thread until `stop()` ends the helper's listen — or
    /// until the client disconnects (an observed `Partial` write failure aborts
    /// the listen; see the module docs).
    pub fn run(&self, conn: &mut Conn) {
        // Provider-aware gate: on the ANE (Core ML) path the ONNX model files are never
        // downloaded, so the raw ONNX-only `parakeet_present()` would wrongly block here.
        let parakeet_ok = ds_config::Paths::resolve()
            .map(|p| crate::config_gate::parakeet_present_for(&ds_config::VoiceConfig::load(&p)))
            .unwrap_or(false);
        if !parakeet_ok {
            let _ = conn.send(&Response::error(
                "Parakeet model not installed — download it in Settings",
            ));
            return;
        }
        self.stop_requested.store(false, Ordering::SeqCst);
        if conn.send(&Response::Listening).is_err() {
            // The client is already gone — don't open the mic for nobody.
            return;
        }
        // The partial callback borrows `conn` only for the listen call; `conn` is
        // free again for the terminal response below.
        let mut disconnected = false;
        let result = {
            let mut on_partial = |t: &str| {
                if disconnected {
                    return;
                }
                if conn
                    .send(&Response::Partial {
                        text: t.to_string(),
                    })
                    .is_err()
                {
                    // The streaming client hung up mid-session: abort the helper's
                    // listen so this returns now, instead of holding the mic and
                    // streaming into the void until an external
                    // `TestRecognitionStop` arrives on a second connection.
                    disconnected = true;
                    self.tts.stop_listen();
                }
            };
            self.tts
                .listen_cancellable(&self.stop_requested, &mut on_partial)
        };
        if disconnected {
            return; // no client left to read a terminal line
        }
        let _ = match result {
            Ok(text) => conn.send(&Response::Transcript { text }),
            Err(e) => conn.send(&Response::error(format!("test recognition: {e}"))),
        };
    }

    /// Stop the active session: end the helper's listen so `run()` finishes and
    /// emits its `Transcript`. No-op if none is active. Uses `lstop` (not `stop`)
    /// so it ends the listen in BOTH modes — in full-duplex a plain `stop` cancels
    /// a speak but leaves the concurrent listen running.
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.tts.stop_listen();
    }
}
