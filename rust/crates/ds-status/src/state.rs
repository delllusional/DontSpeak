//! Canonical engine lifecycle state — single source for the `EngineStatus.state` wire token.
//!
//! Producer (`dontspeakd::status`) picks one [`EngineState`] per engine row via
//! `downloading > failed > missing > running > warming > idle` and stores it typed in
//! [`crate::EngineStatus::state`]. Serde emits the wire token; consumers match the enum.
//!
//! Platform UIs (Swift/C#) hand-mirror token values; the pinning test blocks silent drift.
//!
//! `Blocked` is reserved: consumers treat it as a warning trouble state, but the producer does
//! not emit it yet — kept so a future producer can light it without consumer changes.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Engine lifecycle state; 1:1 with `EngineStatus.state` and the status-dot mapping.
/// Precedence and `Blocked` reservation: module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineState {
    Missing,
    /// Present but not enabled (no warm child wanted).
    Idle,
    Downloading,
    /// Present + enabled, loading into the warm child (not yet resident).
    Warming,
    /// Reserved: handled by consumers, not emitted by the current producer.
    Blocked,
    Failed,
    /// Resident + warm, ready to serve.
    Running,
}

impl EngineState {
    pub const ALL: [EngineState; 7] = [
        EngineState::Missing,
        EngineState::Idle,
        EngineState::Downloading,
        EngineState::Warming,
        EngineState::Blocked,
        EngineState::Failed,
        EngineState::Running,
    ];

    /// Wire token — engine→app contract; change only with every platform mirror + pinning test.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineState::Missing => "missing",
            EngineState::Idle => "idle",
            EngineState::Downloading => "downloading",
            EngineState::Warming => "warming",
            EngineState::Blocked => "blocked",
            EngineState::Failed => "failed",
            EngineState::Running => "running",
        }
    }

    /// Wire token → variant; `None` if unrecognized (consumers treat as neutral/ready, like old `_ =>`).
    pub fn parse(s: &str) -> Option<EngineState> {
        Some(match s {
            "missing" => EngineState::Missing,
            "idle" => EngineState::Idle,
            "downloading" => EngineState::Downloading,
            "warming" => EngineState::Warming,
            "blocked" => EngineState::Blocked,
            "failed" => EngineState::Failed,
            "running" => EngineState::Running,
            _ => return None,
        })
    }
}

impl FromStr for EngineState {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EngineState::parse(s).ok_or(())
    }
}

/// Wire token string (not externally-tagged JSON).
impl Serialize for EngineState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Unknown token → error (producer only emits [`EngineState::ALL`]; contract tests pin).
impl<'de> Deserialize<'de> for EngineState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        EngineState::parse(&s).ok_or_else(|| {
            serde::de::Error::unknown_variant(
                &s,
                &[
                    "missing",
                    "idle",
                    "downloading",
                    "warming",
                    "blocked",
                    "failed",
                    "running",
                ],
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_parse_round_trips_every_variant() {
        for v in EngineState::ALL {
            assert_eq!(EngineState::parse(v.as_str()), Some(v), "round-trip {v:?}");
            assert_eq!(v.as_str().parse::<EngineState>(), Ok(v), "FromStr {v:?}");
        }
    }

    #[test]
    fn parse_rejects_unknown_tokens() {
        assert_eq!(EngineState::parse(""), None);
        assert_eq!(EngineState::parse("Running"), None);
        assert_eq!(EngineState::parse("ready"), None);
    }

    #[test]
    fn tokens_are_the_exact_wire_strings() {
        // Pin wire tokens: Swift/C# UIs hand-mirror these across the C ABI.
        assert_eq!(EngineState::Missing.as_str(), "missing");
        assert_eq!(EngineState::Idle.as_str(), "idle");
        assert_eq!(EngineState::Downloading.as_str(), "downloading");
        assert_eq!(EngineState::Warming.as_str(), "warming");
        assert_eq!(EngineState::Blocked.as_str(), "blocked");
        assert_eq!(EngineState::Failed.as_str(), "failed");
        assert_eq!(EngineState::Running.as_str(), "running");
        let all: Vec<&str> = EngineState::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(
            all,
            [
                "missing",
                "idle",
                "downloading",
                "warming",
                "blocked",
                "failed",
                "running",
            ]
        );
    }
}
