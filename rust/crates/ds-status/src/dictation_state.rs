//! Canonical dictation confirm-panel state — single source for the `dictation.state` wire token.
//!
//! Producer (`dontspeakd::status::dictation_state`) derives one [`DictationState`] per snapshot
//! with precedence `awaiting_confirm > (recording && local_stt) > refused > hidden`, stores
//! [`DictationState::as_str`] in [`crate::Dictation::state`] (additive; legacy booleans stay).
//! Rust consumers classify via [`DictationState::parse`], not raw `&str` matching.
//!
//! Platform UIs (Swift/C#) hand-mirror token values across the C ABI; the pinning test below
//! blocks silent drift. Absent/unknown token ⇒ fall back to legacy booleans (never straight to
//! hidden), so version skew cannot kill the panel.

use std::str::FromStr;

/// Dictation confirm-panel state; 1:1 with `dictation.state`. Panel shown when not `hidden`.
/// Producer precedence and skew fallback: module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DictationState {
    /// Idle, or recording whose engine shows no panel (ClaudeNative).
    Hidden,
    /// Local-transcript engine actively capturing (live partials in panel).
    Recording,
    /// Transcript finalized; waiting for Caps confirm tap.
    AwaitingConfirm,
    /// Dictation START refused (engine can't transcribe yet); warning glow for the refusal window.
    Refused,
}

impl DictationState {
    pub const ALL: [DictationState; 4] = [
        DictationState::Hidden,
        DictationState::Recording,
        DictationState::AwaitingConfirm,
        DictationState::Refused,
    ];

    /// Wire token — engine→app contract; change only with every platform mirror + pinning test.
    pub fn as_str(self) -> &'static str {
        match self {
            DictationState::Hidden => "hidden",
            DictationState::Recording => "recording",
            DictationState::AwaitingConfirm => "awaiting_confirm",
            DictationState::Refused => "refused",
        }
    }

    /// Wire token → variant; `None` if unrecognized (consumers use legacy booleans, never hidden).
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
        // Pin wire tokens: Swift/C# UIs hand-mirror these across the C ABI.
        assert_eq!(DictationState::Hidden.as_str(), "hidden");
        assert_eq!(DictationState::Recording.as_str(), "recording");
        assert_eq!(DictationState::AwaitingConfirm.as_str(), "awaiting_confirm");
        assert_eq!(DictationState::Refused.as_str(), "refused");
        let all: Vec<&str> = DictationState::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(all, ["hidden", "recording", "awaiting_confirm", "refused"]);
    }
}
