//! ds-stt — pluggable speech-to-text engines for dontspeak (ARCHITECTURE §A.2).
//!
//! One trait [`Stt`] behind dynamic dispatch, selected by config enum via the
//! `ds-engines` factory. The engine boundary lives INSIDE the engine's
//! Caps-Lock state machine: the same OFF→ON / ON→OFF edges drive whichever
//! engine is boxed.
//!
//!   * [`claude_native::ClaudeNative`] — the DEFAULT: delegates dictation to
//!     Claude Code's own voice (TAP mode). `start`/`stop` each tap its push-to-talk
//!     key once, focus-gated to a terminal.
//!   * [`system::SystemStt`] — inert in-process placeholder; the live macOS
//!     `SFSpeechRecognizer` path runs in the warm helper.
//!
//!   * [`parakeet::ParakeetTranscriber`] — LOCAL on-device STT: mic capture (cpal) → 16 kHz
//!     (rubato) → `transcribe-rs` `ParakeetModel` (TDT 0.6b v2 int8) over the shared `ort` runtime;
//!     pastes the transcript via `KeyInjector::type_text`.
//!
//! `Stt` is intentionally NOT `Send`: the engine drives it from its single poll
//! thread, and `ClaudeNative` borrows the engine-owned platform (whose macOS
//! CGEventSource is `!Send`). Keeping the trait non-`Send` avoids forcing an
//! `unsafe impl Sync` on the platform — see the engine's ownership note.

/// Live utterance segmentation (speech→silence boundaries) for streaming dictation.
pub mod boundary;
pub mod claude_native;
/// Apple-native (FluidAudio Parakeet Core ML / ANE) STT. macOS only.
#[cfg(target_os = "macos")]
pub mod coreml;
/// Speaker diarization ("who spoke when") — trait + segments, Core ML backend (macOS).
pub mod diarize;
pub mod local;
pub mod parakeet;
pub mod separate;
/// Shared loader for the FluidAudio shim dylib (libsmkokoro), used by the Parakeet STT
/// and system (SpeechAnalyzer) STT backends. macOS only.
#[cfg(target_os = "macos")]
pub mod shim;
/// System STT over macOS `SFSpeechRecognizer` (on-device). macOS only.
#[cfg(target_os = "macos")]
pub mod sysspeech;
pub mod system;

pub use boundary::VadBoundaryDetector;
pub use claude_native::ClaudeNative;
pub use local::LocalTranscriber;
pub use parakeet::{Capture, ParakeetTranscriber, resample, resample_to_16k};
pub use separate::Separator;
pub use system::SystemStt;

#[cfg(target_os = "macos")]
pub use sysspeech::SystemState;
/// Usability of the System STT engine (macOS 26 SpeechAnalyzer, en-US), mirroring the
/// Parakeet present/warming/ready split for the status dot. Off macOS it's just a stub.
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemState {
    Ready,
    Preparing,
    Unavailable,
}

/// Current usability of system STT (ready / preparing / unavailable). Probes WITHOUT
/// prompting (safe for the model-status poll). Always [`SystemState::Unavailable`] off macOS.
#[cfg(target_os = "macos")]
pub fn system_state() -> SystemState {
    sysspeech::state()
}
#[cfg(not(target_os = "macos"))]
pub fn system_state() -> SystemState {
    SystemState::Unavailable
}

/// Is on-device system STT usable at all right now (ready OR preparing — the model
/// downloads on demand)? The `build_stt` gate. Always false off macOS.
#[cfg(target_os = "macos")]
pub fn system_available() -> bool {
    sysspeech::available()
}
#[cfg(not(target_os = "macos"))]
pub fn system_available() -> bool {
    false
}

/// Request Speech Recognition authorization (prompts on first use), BLOCKING, then
/// re-check. `Ok(())` when usable afterwards, else `Err(reason)`. The engine calls this
/// on opt-in so enabling `stt_engine=system` verifies availability and never silently
/// falls back. Always `Err` off macOS.
#[cfg(target_os = "macos")]
pub fn system_authorize() -> Result<(), String> {
    sysspeech::authorize()
}
#[cfg(not(target_os = "macos"))]
pub fn system_authorize() -> Result<(), String> {
    Err("system speech recognition is macOS-only".into())
}

/// A speech-to-text backend. Object-safe so the factory hands back `Box<dyn Stt>`.
///
/// The engine calls these on the Caps-Lock edges:
///   OFF→ON  ⇒ `start()`
///   ON→OFF  ⇒ `stop()`
///   §F reset⇒ `abort()` (discard, do not inject) then the engine resets.
pub trait Stt {
    /// Begin capture on Caps-ON. ClaudeNative emits the initial Ctrl+G down (if a
    /// terminal is frontmost); local engines open the mic + start buffering.
    /// Returns whether the engine considered itself started (informational).
    fn start(&mut self) -> bool;

    /// End capture on Caps-OFF. ClaudeNative emits Ctrl+G up (Claude submits);
    /// local engines run the final transcription pass and inject the transcript.
    fn stop(&mut self);

    /// Abort the in-flight capture WITHOUT injecting (the §F long-press
    /// force-reset path). DEFAULT delegates to `stop()`; the local engine overrides it
    /// to DISCARD the in-flight capture (per §F.1 the reset must not inject).
    fn abort(&mut self) {
        self.stop();
    }

    /// Whether this engine is usable right now (model present, supported OS).
    /// The factory probes this and degrades to the Phase-1 default when false.
    fn is_available(&self) -> bool {
        true
    }

    /// Debug tag for tests / logs (which concrete engine this box is).
    fn kind(&self) -> &'static str {
        "stt"
    }

    /// Whether this engine DEFERS the paste: it deposits the final transcript into
    /// the shared dictation buffer ASYNCHRONOUSLY (so `stop()` never blocks the poll
    /// thread on the slow final pass), and the engine auto-submits once it lands. True
    /// for the local-transcript path (Parakeet helper). DEFAULT false — ClaudeNative
    /// submits inline via Ctrl+G and has no deferred final.
    fn defers_paste(&self) -> bool {
        false
    }
}
