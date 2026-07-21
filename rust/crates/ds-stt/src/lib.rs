//! ds-stt — pluggable speech-to-text engines for dontspeak.
//!
//! One trait [`Stt`] behind dynamic dispatch, selected by config via the
//! `ds-engines` factory. The engine boundary lives INSIDE the Caps-Lock state
//! machine: the same OFF→ON / ON→OFF edges drive whichever engine is boxed.
//!
//!   * [`claude_native::ClaudeNative`] — DEFAULT: Claude Code's voice (TAP mode).
//!   * [`system::SystemStt`] — inert in-process placeholder; live macOS
//!     `SFSpeechRecognizer` runs in the warm helper.
//!   * [`parakeet::ParakeetTranscriber`] — LOCAL on-device: mic → 16 kHz → the Parakeet
//!     transducer ([`streaming`]) over shared `ort`; paste via `KeyInjector`.
//!
//! `Stt` is intentionally NOT `Send`: engine drives it from its single poll
//! thread, and `ClaudeNative` borrows the engine-owned platform (macOS
//! CGEventSource is `!Send`). Avoids forcing `unsafe impl Sync` on the platform.

/// Live utterance segmentation (speech→silence) for streaming dictation.
pub mod boundary;
pub mod claude_native;
/// Speaker diarization ("who spoke when") — trait + MLX backend (macOS).
pub mod diarize;
pub mod local;
/// MLX Audio Parakeet STT. macOS only.
#[cfg(target_os = "macos")]
pub mod mlx;
pub mod parakeet;
pub mod separate;
/// Offline Parakeet transducer over `ort`, decoded one speech segment at a time.
pub mod streaming;
/// System STT over macOS `SFSpeechRecognizer`. macOS only.
#[cfg(target_os = "macos")]
pub mod sysspeech;
pub mod system;

pub use boundary::VadBoundaryDetector;
pub use claude_native::ClaudeNative;
pub use local::LocalTranscriber;
pub use parakeet::{Capture, ParakeetTranscriber, resample, resample_to_16k};
pub use separate::Separator;
pub use streaming::{OnnxStreamer, StreamSession, StreamingStt};
pub use system::SystemStt;

#[cfg(target_os = "macos")]
pub use sysspeech::{SystemState, SystemStreamer};
/// System STT usability for the status dot (mirrors Parakeet present/warming/ready).
/// Off macOS this is a stub.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemState {
    Ready,
    Preparing,
    Unavailable,
}

/// System STT usability. Probe only (no prompt) — safe for model-status poll.
/// Always [`SystemState::Unavailable`] off macOS.
#[cfg(target_os = "macos")]
pub fn system_state() -> SystemState {
    sysspeech::state()
}
#[cfg(not(target_os = "macos"))]
pub fn system_state() -> SystemState {
    SystemState::Unavailable
}

/// Usable now (ready OR preparing — model downloads on demand)? `build_stt` gate.
/// Always false off macOS.
#[cfg(target_os = "macos")]
pub fn system_available() -> bool {
    sysspeech::available()
}
#[cfg(not(target_os = "macos"))]
pub fn system_available() -> bool {
    false
}

/// Request Speech Recognition authorization (prompts on first use), BLOCKING, then
/// re-check. Called on explicit `stt_engine=system` opt-in AND at boot/reload when
/// the ladder resolves to System without that opt-in — see
/// `dontspeakd::boot::authorize_system_stt_if_needed`. Always `Err` off macOS.
#[cfg(target_os = "macos")]
pub fn system_authorize() -> Result<(), String> {
    sysspeech::authorize()
}
#[cfg(not(target_os = "macos"))]
pub fn system_authorize() -> Result<(), String> {
    Err("system speech recognition is macOS-only".into())
}

/// Speech-to-text backend. Object-safe so the factory returns `Box<dyn Stt>`.
///
/// Caps-Lock edges: OFF→ON ⇒ `start()`; ON→OFF ⇒ `stop()`; §F reset ⇒ `abort()` then engine resets.
pub trait Stt {
    /// Caps-ON. Returns whether the engine considered itself started (informational).
    fn start(&mut self) -> bool;

    /// Caps-OFF: end capture / inject as appropriate for the backend.
    fn stop(&mut self);

    /// §F long-press force-reset: discard capture (no inject). Default → `stop()`;
    /// local engines override to discard (§F.1).
    fn abort(&mut self) {
        self.stop();
    }

    /// Debug tag for tests / logs.
    fn kind(&self) -> &'static str {
        "stt"
    }

    /// DEFERS paste: final transcript lands in the shared dictation buffer
    /// ASYNCHRONOUSLY (`stop()` never blocks the poll thread on the final pass);
    /// engine auto-submits once it lands. True for local-transcript (Parakeet helper).
    /// DEFAULT false — ClaudeNative submits inline via Ctrl+G.
    fn defers_paste(&self) -> bool {
        false
    }
}
