//! The `ds-helper --serve` child's stdout reply-token vocabulary.
//!
//! ONE shared definition for both sides of the helper→engine wire: `ds-helper`
//! EMITS these lines (`serve.rs` for the speak/load/diarize/enroll lifecycle,
//! `listen.rs` for dictation) and `dontspeakd` PARSES them (`tts.rs` — the
//! pre-READY wait loop in `start()` and the persistent post-READY `reader_loop`).
//! Requests in the other direction (engine → helper stdin) are typed serde JSON
//! and don't live here; this crate covers only the bare stdout reply tokens,
//! which used to be independent string literals on each side — the one wire
//! contract in the workspace with no shared definition.
//!
//! Constants only — no format/parse helpers — so every call site keeps its exact
//! byte behaviour (notably the [`ERR`] no-trailing-space quirk). The `#[cfg(test)]`
//! pins on every value make any future edit a visible, deliberate protocol change;
//! `dontspeakd`'s reader tests keep their raw-byte fixtures (`b"ERR bad phoneme\n"`,
//! …) as the drift guard on the parsing side.
//!
//! The contract, line by line (every line is `\n`-terminated; the engine trims
//! whitespace before matching):
//!
//! * `READY` — serve loop, once: the TTS model is warm AND the audio output is open.
//! * `WARMING stt` / `WARMING tts` — a model load+warm just started
//!   ([`WARMING_PREFIX`]); the engine's pre-READY loop deliberately ignores these.
//! * `PROVIDER <ep>` — the realized TTS execution provider, before `READY`.
//! * `ERR <msg>` — a fatal load/audio failure before `READY` (the child exits), or
//!   a soft per-speak error after it (the child stays alive). THE QUIRK: the engine
//!   strips [`ERR`] with NO trailing space, so `<msg>` keeps its leading space
//!   (e.g. `"TTS child error: bad phoneme"`). Do not "normalize" this.
//! * `DONE` — the terminal for a successful or cancelled speak/preview request.
//!   A failed request terminates with `ERR <msg>` instead.
//! * `CUEDONE` — the terminal for a played, suppressed, or cancelled earcon.
//! * `STATS <k=v …>` — per-utterance synth timing, just before `DONE`.
//! * `TTSLOADED` / `STTLOADED` — model-residency confirmations (load/preload/lazy
//!   reload succeeded; the model is genuinely resident + warm).
//! * `TTSLOADERR <msg>` / `STTLOADERR <msg>` — a model (re)load failed.
//! * `STT_PROVIDER <ep>` — the realized STT execution provider (STT preloads on a
//!   parallel thread, so this lands on either side of `READY`).
//! * `LISTENING` — a dictation session opened (emit-only; the engine ignores it).
//! * `PARTIAL <text>` — a live dictation-overlay update (de-duped by the helper).
//! * `FINAL <text>` — the final transcript. Emitted with the trailing space even
//!   when `<text>` is empty; the engine's trim then sees bare `FINAL`, so BOTH
//!   [`FINAL`] (equality) and [`FINAL_PREFIX`] (strip) exist.
//! * `STTSTATS <k=v …>` — per-listen timing, just before `FINAL`.
//! * `STTERR <msg>` — a listen failed (e.g. the mic wouldn't open).
//! * `LDONE` — the listen terminal (demuxes a listen from concurrent speak output
//!   on the shared stdout — never `DONE`).
//! * `DIAR <json>` / `DIARERR <msg>` / `DDONE` — one-shot diarize result /
//!   failure / terminal.
//! * `EMB <json>` / `ENROLLERR <msg>` / `EDONE` — one-shot enroll voiceprint /
//!   failure / terminal.

// ── startup / lifecycle ──────────────────────────────────────────────────────

/// Serve loop, once: the TTS model is warm AND the audio output is open.
pub const READY: &str = "READY";
/// A model load+warm started (`WARMING stt` / `WARMING tts`). Emit-only — the
/// engine's pre-READY loop deliberately ignores these lines.
pub const WARMING_PREFIX: &str = "WARMING ";
/// The realized TTS execution provider, emitted before [`READY`].
pub const PROVIDER_PREFIX: &str = "PROVIDER ";
/// Fatal pre-READY failure or soft per-speak error. Stripped by the engine with
/// NO trailing space, so the payload keeps its leading space — see the crate doc.
pub const ERR: &str = "ERR";

// ── speak ────────────────────────────────────────────────────────────────────

/// Terminal for a successful or cancelled speak/preview request; failures use [`ERR`].
pub const DONE: &str = "DONE";
/// Per-utterance synth timing (`k=v` pairs), just before [`DONE`].
pub const STATS_PREFIX: &str = "STATS ";

// ── earcon ────────────────────────────────────────────────────────────────────────────

/// Terminal for a played, muted-before-start, or explicitly cancelled earcon.
pub const CUEDONE: &str = "CUEDONE";

// ── model residency ──────────────────────────────────────────────────────────

/// The Kokoro (TTS) model is resident + warm.
pub const TTSLOADED: &str = "TTSLOADED";
/// The Parakeet (STT) model is resident + warm.
pub const STTLOADED: &str = "STTLOADED";
/// A TTS model (re)load failed.
pub const TTSLOADERR_PREFIX: &str = "TTSLOADERR ";
/// An STT model (re)load failed.
pub const STTLOADERR_PREFIX: &str = "STTLOADERR ";
/// The realized STT execution provider (lands on either side of [`READY`]).
pub const STT_PROVIDER_PREFIX: &str = "STT_PROVIDER ";

// ── listen (dictation) ───────────────────────────────────────────────────────

/// A dictation session opened. Emit-only; the engine ignores it.
pub const LISTENING: &str = "LISTENING";
/// A live dictation-overlay update.
pub const PARTIAL_PREFIX: &str = "PARTIAL ";
/// The final transcript, bare form: an empty transcript is emitted as `FINAL `
/// (via [`FINAL_PREFIX`]) and the engine's trim reduces it to this.
pub const FINAL: &str = "FINAL";
/// The final transcript, payload form.
pub const FINAL_PREFIX: &str = "FINAL ";
/// Per-listen timing (`k=v` pairs), just before the final transcript.
pub const STTSTATS_PREFIX: &str = "STTSTATS ";
/// A listen failed (e.g. the mic wouldn't open).
pub const STTERR_PREFIX: &str = "STTERR ";
/// The listen terminal — never [`DONE`], so the engine can demux a listen from
/// concurrent speak output on the shared stdout.
pub const LDONE: &str = "LDONE";

// ── one-shot diarize ─────────────────────────────────────────────────────────

/// Diarization result (`{segments,speakers}` JSON).
pub const DIAR_PREFIX: &str = "DIAR ";
/// Diarization failure.
pub const DIARERR_PREFIX: &str = "DIARERR ";
/// The diarize terminal.
pub const DDONE: &str = "DDONE";

// ── one-shot enroll ──────────────────────────────────────────────────────────

/// Enrollment voiceprint (JSON float array).
pub const EMB_PREFIX: &str = "EMB ";
/// Enrollment failure.
pub const ENROLLERR_PREFIX: &str = "ENROLLERR ";
/// The enroll terminal.
pub const EDONE: &str = "EDONE";

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin every bare token's EXACT bytes. This wire contract predates the crate,
    /// so any value change here is a protocol break between a helper and an engine
    /// built from different revisions — make it a deliberate, visible edit, never
    /// an incidental rename.
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

    /// Pin every prefix's EXACT bytes, INCLUDING the single trailing space each
    /// `strip_prefix` site depends on to hand back a space-free payload.
    #[test]
    fn prefixes_are_pinned_with_their_trailing_space() {
        assert_eq!(WARMING_PREFIX, "WARMING ");
        assert_eq!(PROVIDER_PREFIX, "PROVIDER ");
        assert_eq!(STATS_PREFIX, "STATS ");
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

    /// THE ERR QUIRK: the engine strips [`ERR`] with NO trailing space, so the
    /// payload keeps its leading space (`"ERR bad phoneme"` → `" bad phoneme"`,
    /// giving `"TTS child error: bad phoneme"` downstream). A well-meaning
    /// `ERR_PREFIX = "ERR "` would silently reshape every error message.
    #[test]
    fn err_strips_without_a_trailing_space() {
        assert!(!ERR.ends_with(' '));
        assert_eq!("ERR bad phoneme".strip_prefix(ERR), Some(" bad phoneme"));
    }
}
