//! The `ds-helper --serve` child's stdout reply-token vocabulary.
//!
//! ONE shared definition for both sides of the helper→engine wire: `ds-helper`
//! EMITS these lines (`serve.rs` / `listen.rs`) and `dontspeakd` PARSES them
//! (`tts.rs` pre-READY wait + post-READY `reader_loop`). Engine→helper stdin is
//! typed serde JSON and lives elsewhere. Constants only — no format/parse helpers —
//! so call sites keep exact byte behaviour (notably the [`ERR`] no-trailing-space
//! quirk). `#[cfg(test)]` pins make edits deliberate protocol changes;
//! `dontspeakd` reader tests keep raw-byte fixtures as the parse-side drift guard.
//!
//! Contract (every line `\n`-terminated; engine trims before matching):
//!
//! * `READY` — once: TTS warm AND audio output open.
//! * `WARMING stt` / `WARMING tts` — load+warm started ([`WARMING_PREFIX`]);
//!   pre-READY loop deliberately ignores these.
//! * `PROVIDER <ep>` — realized TTS EP, before `READY`.
//! * `ERR <msg>` — fatal pre-READY (child exits) or soft per-speak after.
//!   **Quirk:** engine strips [`ERR`] with NO trailing space, so `<msg>` keeps its
//!   leading space. Do not "normalize" this.
//! * `DONE` — speak/preview terminal (success or cancel); failures use `ERR`.
//! * `CUEDONE` — earcon terminal (played, suppressed, or cancelled).
//! * `STATS <k=v …>` — per-utterance synth timing, just before `DONE`.
//! * `PROGRESS <n>` — played-batch high-water for batch-granular resume
//!   ([`PROGRESS_PREFIX`]); intermediate, never terminal.
//! * `TTSLOADED` / `STTLOADED` — model resident + warm.
//! * `TTSLOADERR <msg>` / `STTLOADERR <msg>` — (re)load failed.
//! * `STT_PROVIDER <ep>` — realized STT EP (may land either side of `READY`).
//! * `LISTENING` — dictation opened (emit-only; engine ignores).
//! * `PARTIAL <text>` — live overlay (helper de-dupes).
//! * `FINAL <text>` — final transcript; empty emits `FINAL ` so trim yields bare
//!   `FINAL` — both [`FINAL`] and [`FINAL_PREFIX`] exist.
//! * `STTSTATS <k=v …>` — per-listen timing, just before `FINAL`.
//! * `STTERR <msg>` — listen failed (e.g. mic won't open).
//! * `LDONE` — listen terminal (never `DONE`; demuxes concurrent speak on shared stdout).
//! * `DIAR <json>` / `DIARERR <msg>` / `DDONE` — one-shot diarize.
//! * `EMB <json>` / `ENROLLERR <msg>` / `EDONE` — one-shot enroll.

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
/// Absolute played-batch high-water (`skip` included) for batch-granular resume.
/// Intermediate only — request still ends in [`DONE`]/[`ERR`]. Rodio path only
/// (full-duplex never pauses/resumes). Version skew degrades to replay-from-top
/// both ways: older engine drops the line; older helper never emits (mark stays 0).
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
/// Listen terminal — never [`DONE`], so the engine can demux concurrent speak on shared stdout.
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
