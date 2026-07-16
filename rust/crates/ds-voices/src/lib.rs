//! ds-voices — voice/language enumeration for DontSpeak's TTS engines (issue #5).
//!
//! Split out of `ds-tts` so the most-spawned artifact (the `dontspeak` CLI — every
//! hook + MCP call) can list voices without linking the full native-synth stack
//! (`ort`, `rodio`, `voice-g2p`, `ds-model`). Only `ds-config` is a real dependency.
//!
//! Two engines, two sources:
//!   * Kokoro — [`voices`] reads downloaded `voices-v1.0.bin` (never downloads);
//!     [`enumerate`] turns ids into picker choices.
//!   * System — crate-private `say` parses macOS `say -v ?`;
//!     [`system`]`::default_voice_name` resolves the OS default for the greeting.
//!
//! `ds-tts` re-exports [`enumerate`], [`Gender`], [`Quality`], and [`SpeakerVoice`]
//! under their old paths so existing call sites keep compiling; its synth code
//! reaches [`voices`] via a crate-private alias.

pub mod enumerate;
pub(crate) mod say;
pub mod system;
pub mod voices;

/// Voice gender where the engine reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    Female,
    Male,
}

/// Voice quality tier (macOS/SAPI). `qualityRank` for picker sort is the
/// discriminant order (Default < Enhanced < Premium).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Default,
    Enhanced,
    Premium,
}

/// A voice for the settings picker. `id` is the opaque handle the engine expects back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerVoice {
    pub id: String,
    pub name: String,
    /// BCP-47 tag; groups voices into variations in the picker.
    pub language_tag: String,
    pub downloadable: bool,
    pub gender: Option<Gender>,
    pub quality: Option<Quality>,
}
