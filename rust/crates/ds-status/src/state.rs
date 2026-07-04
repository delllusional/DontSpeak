//! The canonical engine lifecycle state — THE single source of truth for the
//! `EngineObj.state` wire token.
//!
//! The engine ([`dontspeakd::status`]) computes one [`EngineState`] per engine row from a
//! precedence ladder (`downloading > failed > missing > running > warming > idle`) and stores
//! its [`EngineState::as_str`] into [`crate::EngineObj::state`] (a `String`) — so the wire
//! format is unchanged. Every Rust consumer that classifies a `state` token (the shared
//! status-panel formatters, the Linux GTK host's dot color + trouble check) routes through
//! [`EngineState::parse`] instead of re-matching raw `&str` literals.
//!
//! The per-platform UIs (macOS Swift, Windows C#) mirror these token *values* by hand across
//! the C ABI; the [`tests::tokens_are_the_exact_wire_strings`] test pins each `as_str` value so
//! those hand mirrors can never silently drift.
//!
//! `Blocked` is a RESERVED, fully-handled state: consumers color/treat it (as a "warning"
//! trouble state), but the producer's precedence ladder does NOT currently emit it. It is kept
//! deliberately so a future producer can light it without touching any consumer.

use std::str::FromStr;

/// One engine lifecycle state. Maps 1:1 to the `EngineObj.state` wire token and to the app's
/// status dot. See the module docs for the producer precedence and the `Blocked` reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EngineState {
    /// Model not on disk.
    Missing,
    /// Present but not enabled (no warm child wanted).
    Idle,
    /// Model files are being fetched.
    Downloading,
    /// Present + enabled, loading into the warm child (not yet resident).
    Warming,
    /// RESERVED: handled by consumers but NOT emitted by the current producer.
    Blocked,
    /// The engine errored.
    Failed,
    /// Resident + warm, ready to serve.
    Running,
}

impl EngineState {
    /// Every variant, in declaration order. Lets consumers/tests enumerate the vocabulary.
    pub const ALL: [EngineState; 7] = [
        EngineState::Missing,
        EngineState::Idle,
        EngineState::Downloading,
        EngineState::Warming,
        EngineState::Blocked,
        EngineState::Failed,
        EngineState::Running,
    ];

    /// The wire token. These exact strings are the engine→app contract the per-platform UIs
    /// mirror — do not change them without updating every mirror (and the pinning test).
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

    /// Parse a wire token back into a variant; `None` for anything unrecognized (consumers
    /// treat unknown tokens as the neutral/ready case, exactly as the old raw `_ =>` arms did).
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
        // Pin the wire token values: the Swift/C# UIs mirror these by hand across the C ABI,
        // so a change here that wasn't mirrored would silently break a platform's status dot.
        assert_eq!(EngineState::Missing.as_str(), "missing");
        assert_eq!(EngineState::Idle.as_str(), "idle");
        assert_eq!(EngineState::Downloading.as_str(), "downloading");
        assert_eq!(EngineState::Warming.as_str(), "warming");
        assert_eq!(EngineState::Blocked.as_str(), "blocked");
        assert_eq!(EngineState::Failed.as_str(), "failed");
        assert_eq!(EngineState::Running.as_str(), "running");
        // ...and the full set, as a defense against an added/removed variant.
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
