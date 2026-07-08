//! ds-voices — voice/language enumeration for DontSpeak's TTS engines (issue #5).
//!
//! Split out of `ds-tts` so the most-spawned artifact in the product (the `dontspeak`
//! CLI — every hook + MCP call) can list voices (`list_voices` MCP tool, the settings
//! picker) without linking the full native-synth stack (`ort`, `rodio`, `voice-g2p`,
//! `grapheme_to_phoneme`, `ds-model`). Only `ds-config` (paths + config enums) is a
//! real dependency here.
//!
//! Two engines, two enumeration sources:
//!   * Kokoro — [`voices`]' npz byte-parser reads the downloaded `voices-v1.0.bin`
//!     (never downloads it); [`enumerate`] turns the ids into picker choices.
//!   * System — the crate-private `say` module parses macOS `say -v ?` output;
//!     [`system`]'s `default_voice_name` resolves the OS-default voice name for
//!     the greeting.
//!
//! `ds-tts` re-exports [`enumerate`], [`Gender`], [`Quality`], and [`SpeakerVoice`]
//! under their old paths so every existing `ds_tts::enumerate` / `ds_tts::SpeakerVoice`
//! call site keeps compiling unchanged; its own synth code reaches [`voices`] via a
//! crate-private alias.

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

/// Voice quality tier where the engine reports it (macOS/SAPI). `qualityRank`
/// for the picker sort is the discriminant order (Default < Enhanced < Premium).
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
    /// BCP-47 tag, groups voices into variations in the picker.
    pub language_tag: String,
    pub downloadable: bool,
    pub gender: Option<Gender>,
    pub quality: Option<Quality>,
}
