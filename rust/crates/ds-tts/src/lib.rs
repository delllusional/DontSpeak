//! ds-tts — text-to-speech synthesis stages for dontspeak.
//!
//! The warm `ds-helper` process owns speaking and playback; this crate supplies the
//! pure stages it runs plus the one System-TTS command seam,
//! [`system::speech_command`] (empty prose is a successful no-op).
//!
//! Helper pipeline: markdown → prose ([`spoken`]) → numbers → G2P ([`g2p`]) → vocab
//! tokens → clause batches ([`batch`]) → synth ([`synth`] / MLX) → trim →
//! play. Pure stages unit-tested without audio/model/network.

/// FluidAudio ANE Kokoro voice-pack materialization. macOS only.
#[cfg(target_os = "macos")]
pub mod ane_voices;
/// Model-bounded phoneme batching (helper bin).
pub mod batch;
/// Chatterbox Multilingual AR backend.
pub mod chatterbox;
pub mod g2p;
mod language;
pub mod mlx_params;
pub(crate) mod numbers;
pub mod omnivoice;
pub(crate) mod ort_session;
pub mod play;
pub mod qwen;
/// Incremental rodio sink (warm serve + one-shot player).
pub mod sink;
pub mod spoken;
pub mod synth;
/// FluidAudio Core ML / ANE Kokoro. macOS Apple Silicon only.
#[cfg(target_os = "macos")]
pub mod synth_fluid;
/// MLX Audio Kokoro. macOS Apple Silicon only.
#[cfg(target_os = "macos")]
pub mod synth_mlx;
pub mod system;
pub(crate) mod trim;
pub(crate) mod vocab;
#[doc(hidden)]
pub mod wav;

pub use language::{
    DEFAULT_LANGUAGE, chunk_language, chunk_language_any, detect_language, supported_language,
};
pub use vocab::SAMPLE_RATE;

/// Re-export from `ds-voices` (issue #5) — CLI lists voices without this heavy crate.
pub use ds_voices::enumerate;
pub use ds_voices::{Gender, Quality, SpeakerVoice};
// Private: ONNX synthesis uses `crate::voices` for the npz parser.
pub(crate) use ds_voices::voices;

/// Canonical agent-text cleanup used by every speech backend.
pub fn normalize_spoken_text(text: &str) -> String {
    spoken::SpokenText::from_markdown(text).into_string()
}

/// Normalize rendered English before Kokoro G2P. Idempotent so backends may call it
/// defensively outside the helper.
pub fn normalize_kokoro_text(text: &str) -> String {
    numbers::expand_numbers(&normalize_spoken_text(text))
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
    fn spoken_text_renders_markdown_without_number_expansion() {
        assert_eq!(
            normalize_spoken_text(
                "Use **shared** `phonemes`; see [the audit](https://example.com/a)."
            ),
            "Use shared phonemes; see the audit."
        );
        assert_eq!(
            normalize_spoken_text("Build 57 next."),
            "Build 57 next.",
            "OS voices expand digits themselves"
        );
        assert_eq!(
            normalize_spoken_text("Read https://example.com now."),
            "Read link now."
        );
        assert_eq!(normalize_spoken_text("***"), "");
    }

    #[test]
    fn every_backend_uses_the_same_text_cleanup() {
        let source = "## Result\nUse **foo_bar** at eedfc57; see https://example.com.";
        let prose = "Result Use foo_bar at see link.";
        assert_eq!(normalize_spoken_text(source), prose);
        assert_eq!(normalize_kokoro_text(source), prose);
    }

    #[test]
    fn rate_to_wpm_baseline_and_clamp() {
        assert_eq!(rate_to_wpm(1.0), 175);
        assert_eq!(rate_to_wpm(0.5), 88);
        assert_eq!(rate_to_wpm(2.0), 350);
        assert_eq!(rate_to_wpm(0.0), 88);
        assert_eq!(rate_to_wpm(10.0), 350);
        assert_eq!(rate_to_wpm(-3.0), 88);
        assert_eq!(rate_to_wpm(1.25), 219);
    }

    #[test]
    fn quality_rank_orders_for_sort() {
        assert!(Quality::Default < Quality::Enhanced);
        assert!(Quality::Enhanced < Quality::Premium);
    }
}
