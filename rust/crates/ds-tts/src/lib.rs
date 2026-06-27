//! ds-tts — pluggable text-to-speech engines for dontspeak (ARCHITECTURE §A.1).
//!
//! One trait [`Tts`] behind dynamic dispatch, selected by config enum via the
//! `ds-engines` factory. Two implementors:
//!   * [`KokoroTts`] — the DEFAULT. NATIVE in-process Kokoro synthesis (ort +
//!     voice-g2p + rodio), spawned via the thin `ds-helper` helper bin in
//!     its own process group, recorded in the single-speaker pidfile. NO Python,
//!     NO uv, NO speak.py.
//!   * [`SystemTts`] — macOS `say -v NAME -r WPM` (compiled here); Windows
//!     PowerShell System.Speech + Linux spd-say/espeak behind cfg (NOT built on
//!     the macOS host).
//!
//! The native Kokoro pipeline (in the helper bin): voice-g2p phonemize the
//! (already-normalized) text ([`g2p`]) → map to Kokoro vocab token ids (`vocab`)
//! → batch the phonemes at clause marks into a ramped sequence for gapless
//! streaming ([`batch`]'s `stream_batches`) → ort session inference over
//! kokoro-v1.0.onnx ([`synth`], style rows from the voices npz parsed by
//! `voices`) → trim silence (`trim`) → stream 24 kHz mono PCM ([`play`]),
//! synthesizing batch N+1 while batch N plays. The pure stages (vocab/voices/
//! trim/g2p) are unit-tested with no audio, no model, no network; synth/play are
//! gated behind the helper bin.
//!
//! The single-speaker pidfile contract is sacred: every engine spawns playback
//! in its OWN process group and returns its pgid so the caller records it in
//! `~/.claude/speak-hook.pid` for barge-in (killpg). `speak` therefore returns a
//! [`SpeakHandle`]; the live `Child` (needed by narrate's pidfile-takeover watch
//! loop) is obtained via the per-engine `kokoro::spawn` / `system::spawn` helpers,
//! which `speak` wraps.

use std::io;

/// Materialize FluidAudio Core ML / ANE Kokoro voice packs from the local ONNX
/// `voices-v1.0.bin` (the ANE repo ships only `af_heart`). Path logic is harmless
/// off-macOS; only the apple-native backend ever calls it.
pub mod ane_voices;
/// Phoneme batching / streaming for gapless synth (used by the helper bin).
pub mod batch;
pub mod enumerate;
pub mod g2p;
pub(crate) mod kokoro;
pub(crate) mod numbers;
pub mod play;
pub(crate) mod say;
pub mod synth;
/// Apple-native (FluidAudio Core ML / ANE) Kokoro backend. macOS only.
#[cfg(target_os = "macos")]
pub mod synth_coreml;
pub mod system;
pub(crate) mod trim;
pub(crate) mod vocab;
pub(crate) mod voices;
#[doc(hidden)]
pub mod wav;

pub use kokoro::KokoroTts;
pub use system::SystemTts;

/// The process-GROUP id of a spawned speaker — recorded in the pidfile so the
/// engine's caps-ON barge-in (`killpg(-pgid, SIGTERM)`) can preempt it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakHandle {
    pub pgid: i32,
}

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

/// A text-to-speech backend. Object-safe so the factory hands back `Box<dyn Tts>`.
pub trait Tts: Send {
    /// Speak `text` with `voice_id` (engine-opaque) at `rate` (1.0 = normal).
    /// Spawns playback in its OWN process group and returns its pgid; the caller
    /// records it in the pidfile and owns the wait + pidfile-clear lifecycle.
    fn speak(&self, text: &str, voice_id: Option<&str>, rate: f32) -> io::Result<SpeakHandle>;

    /// Stop current playback. Default no-op: the pidfile `killpg` barge-in owns
    /// preemption, so engines that record their pgid need do nothing here.
    fn stop(&self) {}

    /// Voices for the settings picker. Empty where enumeration isn't supported.
    fn voices(&self) -> Vec<SpeakerVoice> {
        Vec::new()
    }

    /// Whether this engine can open the OS voice installer (§B.3).
    fn can_manage_voices(&self) -> bool {
        false
    }

    /// Open the OS voice installer / settings pane (§B.3).
    fn manage_voices(&self) {}

    /// A short human hint for the picker ("Spoken Content > System Voice > …").
    fn manage_voices_hint(&self) -> Option<&str> {
        None
    }

    /// Debug tag for tests / logs (which concrete engine this box is).
    fn kind(&self) -> &'static str {
        "tts"
    }
}

/// Map a normalized `rate` (1.0 = normal) to a system-TTS words-per-minute
/// value. Clamped to the 0.5..=2.0 range; 1.0 maps to a 175 wpm baseline
/// (macOS `say`'s default-ish speaking rate), scaling linearly. PURE + tested.
pub fn rate_to_wpm(rate: f32) -> u16 {
    const BASELINE_WPM: f32 = 175.0;
    let clamped = rate.clamp(0.5, 2.0);
    (BASELINE_WPM * clamped).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_to_wpm_baseline_and_clamp() {
        assert_eq!(rate_to_wpm(1.0), 175);
        // 0.5 -> ~88, 2.0 -> 350.
        assert_eq!(rate_to_wpm(0.5), 88);
        assert_eq!(rate_to_wpm(2.0), 350);
        // Out-of-range clamps, never panics.
        assert_eq!(rate_to_wpm(0.0), 88); // clamps up to 0.5
        assert_eq!(rate_to_wpm(10.0), 350); // clamps down to 2.0
        assert_eq!(rate_to_wpm(-3.0), 88);
        // A mid step (1.25) is a sane interpolation.
        assert_eq!(rate_to_wpm(1.25), 219);
    }

    #[test]
    fn quality_rank_orders_for_sort() {
        assert!(Quality::Default < Quality::Enhanced);
        assert!(Quality::Enhanced < Quality::Premium);
    }
}
