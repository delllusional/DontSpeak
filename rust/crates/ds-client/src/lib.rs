//! Wired-client identity for wiring, hooks, IPC, MCP, and log `client=`.
//! Leaf crate (breaks `ds-log` ↔ `ds-config` cycle).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A client supported by DontSpeak's wiring registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WiredClient {
    ClaudeCode,
    Codex,
    QwenCode,
    Grok,
    KimiCode,
    Hermes,
}

impl WiredClient {
    /// Every wired client in token order (registry drift-tested).
    pub const ALL: &'static [WiredClient] = &[
        WiredClient::ClaudeCode,
        WiredClient::Codex,
        WiredClient::QwenCode,
        WiredClient::Grok,
        WiredClient::KimiCode,
        WiredClient::Hermes,
    ];

    /// One canonical identity used for parsing, serialization, launch, IPC, logs, and MCP.
    pub const fn as_str(self) -> &'static str {
        match self {
            WiredClient::ClaudeCode => "claude",
            WiredClient::Codex => "codex",
            WiredClient::QwenCode => "qwen",
            WiredClient::Grok => "grok",
            WiredClient::KimiCode => "kimi",
            WiredClient::Hermes => "hermes",
        }
    }

    /// Case/whitespace tolerant; non-client tokens return `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let token = s.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|client| token.eq_ignore_ascii_case(client.as_str()))
    }
}

impl Serialize for WiredClient {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Unknown tokens fail closed.
impl<'de> Deserialize<'de> for WiredClient {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        WiredClient::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown wired client {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_client_round_trips_parse_serde_and_display() {
        for &c in WiredClient::ALL {
            assert_eq!(WiredClient::parse(c.as_str()), Some(c), "{c:?}");
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(s, format!("\"{}\"", c.as_str()), "{c:?}");
            let back: WiredClient = serde_json::from_str(&s).unwrap();
            assert_eq!(back, c, "{c:?} round-trips through serde");
        }
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant_and_rejects_the_rest() {
        assert_eq!(
            WiredClient::parse("  ClAuDe \n"),
            Some(WiredClient::ClaudeCode)
        );
        assert_eq!(WiredClient::parse("gemini_cli"), None);
        assert_eq!(WiredClient::parse("dontspeak"), None);
        assert_eq!(WiredClient::parse("unknown"), None);
        assert_eq!(WiredClient::parse(""), None);
    }

    #[test]
    fn all_contains_the_complete_token_set() {
        assert_eq!(
            WiredClient::ALL
                .iter()
                .copied()
                .map(WiredClient::as_str)
                .collect::<Vec<_>>(),
            ["claude", "codex", "qwen", "grok", "kimi", "hermes"]
        );
    }

    #[test]
    fn non_client_tokens_fail_deserialization() {
        for token in [r#""gemini_cli""#, r#""dontspeak""#, r#""unknown""#] {
            assert!(
                serde_json::from_str::<WiredClient>(token).is_err(),
                "{token}"
            );
        }
    }

    #[test]
    fn a_non_string_value_still_errors() {
        assert!(serde_json::from_str::<WiredClient>("42").is_err());
        assert!(serde_json::from_str::<WiredClient>("null").is_err());
    }
}
