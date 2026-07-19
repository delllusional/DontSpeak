//! `ds-helper --serve` wire vocabulary.
//!
//! **Stdout** — token constants (exact bytes; keep [`ERR`] no-trailing-space quirk).
//! **Stdin** — [`HelperOp`] / [`HelperModel`] JSON (shared serde; op typos fail at compile/decode).
//!
//! Lines `\n`-terminated; engine trims before match:
//! - `READY` — TTS warm + output open (once)
//! - `WARMING stt|tts` — load started; pre-READY ignores
//! - `PROVIDER <ep>` / `STT_PROVIDER <ep>` — realized EPs
//! - `ERR <msg>` — fatal pre-READY or soft per-speak; strip `ERR` with **no** trailing space
//! - `DONE` / `CUEDONE` / `LDONE` / `DDONE` / `EDONE` — terminals (listen ≠ speak)
//! - `PROGRESS <n>` — batch high-water; intermediate only
//! - `TTSLOADED` / `STTLOADED` / `*LOADERR` — residency
//! - `LISTENING` / `PARTIAL` / `FINAL` / `STTSTATS` / `STTERR` — listen stream
//! - `DIAR*` / `EMB*` — one-shot diarize / enroll

// ── stdin ops (engine → helper JSON lines) ───────────────────────────────────

/// Stdin op (`{"op":…}`, snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperOp {
    Stop,
    Mute,
    Stopfade,
    Cue,
    Speak,
    Listen,
    Lstop,
    Diarize,
    Enroll,
    Unload,
    Load,
}

/// `load` / `unload` target model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperModel {
    Tts,
    Stt,
}

// ── startup / lifecycle ──────────────────────────────────────────────────────

/// TTS warm AND audio output open.
pub const READY: &str = "READY";
/// Load+warm started. Emit-only; pre-READY loop ignores these.
pub const WARMING_PREFIX: &str = "WARMING ";
/// Realized TTS EP, before [`READY`].
pub const PROVIDER_PREFIX: &str = "PROVIDER ";
/// Fatal pre-READY or soft per-speak error. Stripped with NO trailing space — see crate doc.
pub const ERR: &str = "ERR";

// ── speak ────────────────────────────────────────────────────────────────────

/// Speak/preview terminal; failures use [`ERR`].
pub const DONE: &str = "DONE";
/// Per-utterance synth timing, just before [`DONE`].
pub const STATS_PREFIX: &str = "STATS ";
/// Batch high-water (`skip` included); intermediate only. Rodio path; skew → replay-from-top.
pub const PROGRESS_PREFIX: &str = "PROGRESS ";

// ── earcon ───────────────────────────────────────────────────────────────────

/// Earcon terminal (played, muted-before-start, or cancelled).
pub const CUEDONE: &str = "CUEDONE";

// ── model residency ──────────────────────────────────────────────────────────

/// Kokoro (TTS) resident + warm.
pub const TTSLOADED: &str = "TTSLOADED";
/// Parakeet (STT) resident + warm.
pub const STTLOADED: &str = "STTLOADED";
/// TTS (re)load failed.
pub const TTSLOADERR_PREFIX: &str = "TTSLOADERR ";
/// STT (re)load failed.
pub const STTLOADERR_PREFIX: &str = "STTLOADERR ";
/// Realized STT EP (either side of [`READY`]).
pub const STT_PROVIDER_PREFIX: &str = "STT_PROVIDER ";

// ── listen (dictation) ───────────────────────────────────────────────────────

/// Dictation opened. Emit-only; engine ignores.
pub const LISTENING: &str = "LISTENING";
/// Live dictation-overlay update.
pub const PARTIAL_PREFIX: &str = "PARTIAL ";
/// Bare final transcript after engine trim of empty `FINAL `.
pub const FINAL: &str = "FINAL";
/// Final transcript payload form (trailing space for empty text).
pub const FINAL_PREFIX: &str = "FINAL ";
/// Per-listen timing, just before the final transcript.
pub const STTSTATS_PREFIX: &str = "STTSTATS ";
/// Listen failed (e.g. mic won't open).
pub const STTERR_PREFIX: &str = "STTERR ";
/// Listen terminal (≠ [`DONE`]; demux concurrent speak on shared stdout).
pub const LDONE: &str = "LDONE";

// ── one-shot diarize ─────────────────────────────────────────────────────────

/// Diarization result (`{segments,speakers}` JSON).
pub const DIAR_PREFIX: &str = "DIAR ";
/// Diarization failure.
pub const DIARERR_PREFIX: &str = "DIARERR ";
/// Diarize terminal.
pub const DDONE: &str = "DDONE";

// ── one-shot enroll ──────────────────────────────────────────────────────────

/// Enrollment voiceprint (JSON float array).
pub const EMB_PREFIX: &str = "EMB ";
/// Enrollment failure.
pub const ENROLLERR_PREFIX: &str = "ENROLLERR ";
/// Enroll terminal.
pub const EDONE: &str = "EDONE";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_ops_serialize_to_wire_tokens() {
        assert_eq!(serde_json::to_value(HelperOp::Stop).unwrap(), "stop");
        assert_eq!(
            serde_json::to_value(HelperOp::Stopfade).unwrap(),
            "stopfade"
        );
        assert_eq!(serde_json::to_value(HelperOp::Lstop).unwrap(), "lstop");
        assert_eq!(serde_json::to_value(HelperModel::Tts).unwrap(), "tts");
        assert_eq!(serde_json::to_value(HelperModel::Stt).unwrap(), "stt");
        assert_eq!(
            serde_json::from_value::<HelperOp>(serde_json::json!("speak")).unwrap(),
            HelperOp::Speak
        );
        assert!(serde_json::from_value::<HelperOp>(serde_json::json!("nope")).is_err());
    }

    /// Wire contract predates this crate — any value change is a helper/engine protocol break.
    #[test]
    fn bare_tokens_are_pinned() {
        assert_eq!(READY, "READY");
        assert_eq!(ERR, "ERR");
        assert_eq!(DONE, "DONE");
        assert_eq!(CUEDONE, "CUEDONE");
        assert_eq!(TTSLOADED, "TTSLOADED");
        assert_eq!(STTLOADED, "STTLOADED");
        assert_eq!(LISTENING, "LISTENING");
        assert_eq!(FINAL, "FINAL");
        assert_eq!(LDONE, "LDONE");
        assert_eq!(DDONE, "DDONE");
        assert_eq!(EDONE, "EDONE");
    }

    /// Prefixes keep the trailing space each `strip_prefix` site depends on.
    #[test]
    fn prefixes_are_pinned_with_their_trailing_space() {
        assert_eq!(WARMING_PREFIX, "WARMING ");
        assert_eq!(PROVIDER_PREFIX, "PROVIDER ");
        assert_eq!(STATS_PREFIX, "STATS ");
        assert_eq!(PROGRESS_PREFIX, "PROGRESS ");
        assert_eq!(TTSLOADERR_PREFIX, "TTSLOADERR ");
        assert_eq!(STTLOADERR_PREFIX, "STTLOADERR ");
        assert_eq!(STT_PROVIDER_PREFIX, "STT_PROVIDER ");
        assert_eq!(PARTIAL_PREFIX, "PARTIAL ");
        assert_eq!(FINAL_PREFIX, "FINAL ");
        assert_eq!(STTSTATS_PREFIX, "STTSTATS ");
        assert_eq!(STTERR_PREFIX, "STTERR ");
        assert_eq!(DIAR_PREFIX, "DIAR ");
        assert_eq!(DIARERR_PREFIX, "DIARERR ");
        assert_eq!(EMB_PREFIX, "EMB ");
        assert_eq!(ENROLLERR_PREFIX, "ENROLLERR ");
    }

    /// ERR quirk: strip with NO trailing space so the payload keeps its leading space
    /// (`"ERR bad phoneme"` → `" bad phoneme"`). `ERR_PREFIX = "ERR "` would reshape every msg.
    #[test]
    fn err_strips_without_a_trailing_space() {
        assert!(!ERR.ends_with(' '));
        assert_eq!("ERR bad phoneme".strip_prefix(ERR), Some(" bad phoneme"));
    }
}
