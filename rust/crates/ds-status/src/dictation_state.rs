//! The canonical dictation confirm-panel state — THE single source of truth for the
//! `dictation.state` wire token.
//!
//! The engine derives one [`DictationState`] per status snapshot (the producer derivation
//! lives in `dontspeakd::status::dictation_state`, precedence
//! `awaiting_confirm > (recording && local_stt) > refused > hidden`) and stores its
//! [`DictationState::as_str`] into [`crate::Dictation::state`] (a `String`) — an ADDITIVE
//! wire change: the legacy boolean fields stay. Every Rust consumer that classifies the
//! token (the Linux GTK host's panel show gate) routes through [`DictationState::parse`]
//! instead of re-matching raw `&str` literals.
//!
//! The per-platform UIs (macOS Swift, Windows C#) mirror these token *values* by hand
//! across the C ABI; the [`tests::tokens_are_the_exact_wire_strings`] test pins each
//! `as_str` value so those hand mirrors can never silently drift. An absent/unknown token
//! (older engine payload) makes each host fall back to the legacy boolean derivation —
//! never to hidden — so version skew can't silently kill the dictation panel.

use std::str::FromStr;

/// One dictation confirm-panel state. Maps 1:1 to the `dictation.state` wire token; every
/// host shows the panel exactly when the state is not [`DictationState::Hidden`]. See the
/// module docs for the producer precedence and the version-skew fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DictationState {
    /// No panel: idle, or a recording whose engine shows no panel (ClaudeNative).
    Hidden,
    /// Actively capturing on a local-transcript engine (live partials in the panel).
    Recording,
    /// Transcript finalized, waiting for the Caps confirm tap.
    AwaitingConfirm,
    /// A dictation START was just refused (engine can't transcribe yet) — the panel
    /// shows the warning glow for the refusal window.
    Refused,
}

impl DictationState {
    /// Every variant, in declaration order. Lets consumers/tests enumerate the vocabulary.
    pub const ALL: [DictationState; 4] = [
        DictationState::Hidden,
        DictationState::Recording,
        DictationState::AwaitingConfirm,
        DictationState::Refused,
    ];

    /// The wire token. These exact strings are the engine→app contract the per-platform UIs
    /// mirror — do not change them without updating every mirror (and the pinning test).
    pub fn as_str(self) -> &'static str {
        match self {
            DictationState::Hidden => "hidden",
            DictationState::Recording => "recording",
            DictationState::AwaitingConfirm => "awaiting_confirm",
            DictationState::Refused => "refused",
        }
    }

    /// Parse a wire token back into a variant; `None` for anything unrecognized (consumers
    /// fall back to the legacy boolean derivation on `None` — never straight to hidden).
    pub fn parse(s: &str) -> Option<DictationState> {
        Some(match s {
            "hidden" => DictationState::Hidden,
            "recording" => DictationState::Recording,
            "awaiting_confirm" => DictationState::AwaitingConfirm,
            "refused" => DictationState::Refused,
            _ => return None,
        })
    }
}

impl FromStr for DictationState {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DictationState::parse(s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_parse_round_trips_every_variant() {
        for v in DictationState::ALL {
            assert_eq!(
                DictationState::parse(v.as_str()),
                Some(v),
                "round-trip {v:?}"
            );
            assert_eq!(v.as_str().parse::<DictationState>(), Ok(v), "FromStr {v:?}");
        }
    }

    #[test]
    fn parse_rejects_unknown_tokens() {
        assert_eq!(DictationState::parse(""), None);
        assert_eq!(DictationState::parse("Recording"), None);
        assert_eq!(DictationState::parse("awaiting"), None);
    }

    #[test]
    fn tokens_are_the_exact_wire_strings() {
        // Pin the wire token values: the Swift/C# UIs mirror these by hand across the C ABI,
        // so a change here that wasn't mirrored would silently break a platform's panel.
        assert_eq!(DictationState::Hidden.as_str(), "hidden");
        assert_eq!(DictationState::Recording.as_str(), "recording");
        assert_eq!(DictationState::AwaitingConfirm.as_str(), "awaiting_confirm");
        assert_eq!(DictationState::Refused.as_str(), "refused");
        // ...and the full set, as a defense against an added/removed variant.
        let all: Vec<&str> = DictationState::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(all, ["hidden", "recording", "awaiting_confirm", "refused"]);
    }
}
