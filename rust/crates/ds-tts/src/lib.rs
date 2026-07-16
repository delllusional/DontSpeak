//! ds-tts — pluggable text-to-speech engines for dontspeak (ARCHITECTURE §A.1).
//!
//! One trait [`Tts`] behind dynamic dispatch (`VoiceConfig::resolved_tts`). Implementors:
//!   * [`KokoroTts`] — DEFAULT. Native Kokoro (ort + voice-g2p + rodio) via the
//!     `ds-helper` bin in its own process group + single-speaker pidfile. No Python.
//!   * [`SystemTts`] — macOS `say`; Windows System.Speech + Linux spd-say behind cfg.
//!
//! Helper pipeline: markdown → prose ([`spoken`]) → numbers → G2P ([`g2p`]) → vocab
//! tokens → clause batches ([`batch`]) → ort synth ([`synth`] / Core ML) → trim →
//! play. Pure stages unit-tested without audio/model/network.
//!
//! Single-speaker pidfile is sacred: every engine spawns in its OWN process group and
//! returns pgid for barge-in (`killpg`). Live `Child` via `kokoro::spawn` / `system::spawn`.

use std::io;

/// Materialize ANE Kokoro voice packs from local ONNX `voices-v1.0.bin` (ANE ships only
/// `af_heart`). Only the apple-native backend calls this.
pub mod ane_voices;
/// Model-bounded phoneme batching (helper bin).
pub mod batch;
pub mod g2p;
pub(crate) mod kokoro;
pub(crate) mod numbers;
pub mod play;
/// Incremental rodio sink (warm serve + one-shot player).
pub mod sink;
pub mod spoken;
pub mod synth;
/// FluidAudio Core ML / ANE Kokoro. macOS only.
#[cfg(target_os = "macos")]
pub mod synth_coreml;
pub mod system;
pub(crate) mod trim;
pub(crate) mod vocab;
#[doc(hidden)]
pub mod wav;

pub use kokoro::KokoroTts;
pub use system::SystemTts;
pub use vocab::SAMPLE_RATE;

/// Re-export from `ds-voices` (issue #5) so CLI can list voices without this heavy crate,
/// and existing `ds_tts::enumerate` / `SpeakerVoice` call sites keep compiling.
pub use ds_voices::enumerate;
pub use ds_voices::{Gender, Quality, SpeakerVoice};
// Private: synth/ane_voices reach the npz parser as `crate::voices` (never public API).
pub(crate) use ds_voices::voices;

/// Normalize rendered English before Kokoro G2P. Shared helper front-end; idempotent
/// so backends may call it defensively outside the helper.
pub fn normalize_kokoro_text(text: &str) -> String {
    numbers::expand_numbers(&spoken::SpokenText::from_markdown(text).into_string())
}

/// Process-GROUP id of a spawned speaker — pidfile records it for caps-ON barge-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpeakHandle {
    pub pgid: i32,
}

/// TTS backend. Object-safe for `Box<dyn Tts>`.
pub trait Tts: Send {
    /// Speak at `rate` (1.0 = normal). Spawns in own process group; caller records
    /// pgid and owns wait + pidfile-clear.
    fn speak(&self, text: &str, voice_id: Option<&str>, rate: f32) -> io::Result<SpeakHandle>;

    /// Default no-op: pidfile `killpg` owns preemption.
    fn stop(&self) {}

    /// Settings-picker voices. Empty where enumeration unsupported.
    fn voices(&self) -> Vec<SpeakerVoice> {
        Vec::new()
    }

    /// Can open OS voice installer (§B.3).
    fn can_manage_voices(&self) -> bool {
        false
    }

    /// Open OS voice installer / settings (§B.3).
    fn manage_voices(&self) {}

    /// Short picker hint ("Spoken Content > System Voice > …").
    fn manage_voices_hint(&self) -> Option<&str> {
        None
    }

    /// Debug tag for tests / logs.
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
    fn kokoro_text_normalization_is_shared_and_idempotent() {
        let normalized = normalize_kokoro_text("Build 57 in room 1,000.");
        assert_eq!(normalized, "Build fifty-seven in room one thousand.");
        assert_eq!(normalize_kokoro_text(&normalized), normalized);
    }

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
