//! `dictation.state` wire token — single source.
//!
//! Producer precedence: `awaiting_confirm > (recording && local_stt) > refused > hidden`.
//! Swift/C# hand-mirror tokens; pinning test blocks drift.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Confirm-panel mode; 1:1 with `dictation.state`. Panel when not `hidden`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DictationState {
    /// Idle, or ClaudeNative recording (no panel).
    Hidden,
    /// Local STT capturing (live partials).
    Recording,
    /// Finalized; wait Caps confirm.
    AwaitingConfirm,
    /// Start refused (not ready); warning glow for the window.
    Refused,
}

impl DictationState {
    pub const ALL: [DictationState; 4] = [
        DictationState::Hidden,
        DictationState::Recording,
        DictationState::AwaitingConfirm,
        DictationState::Refused,
    ];

    /// Wire token; change only with every platform mirror + pinning test.
    pub fn as_str(self) -> &'static str {
        match self {
            DictationState::Hidden => "hidden",
            DictationState::Recording => "recording",
            DictationState::AwaitingConfirm => "awaiting_confirm",
            DictationState::Refused => "refused",
        }
    }

    /// Wire token → variant; `None` if unrecognized.
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

impl Serialize for DictationState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Unknown token → error.
impl<'de> Deserialize<'de> for DictationState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        DictationState::parse(&s).ok_or_else(|| {
            serde::de::Error::unknown_variant(
                &s,
                &["hidden", "recording", "awaiting_confirm", "refused"],
            )
        })
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
