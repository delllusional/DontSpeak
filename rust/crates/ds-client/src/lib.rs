//! Client identity for wiring, hooks, IPC, MCP, log `client=`.
//! Leaf crate (breaks `ds-log` ↔ `ds-config` cycle).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Wireable clients in [`ClientSource::CLIENTS`]; plus `DontSpeak` / `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClientSource {
    ClaudeCode,
    Codex,
    QwenCode,
    Grok,
    KimiCode,
    /// Internal; `client_spec` is always `None`.
    DontSpeak,
    /// Foreign MCP / missing `--client`.
    #[default]
    Unknown,
}

impl ClientSource {
    /// Wireable clients in token order (registry drift-tested).
    pub const CLIENTS: &'static [ClientSource] = &[
        ClientSource::ClaudeCode,
        ClientSource::Codex,
        ClientSource::QwenCode,
        ClientSource::Grok,
        ClientSource::KimiCode,
    ];

    /// Case/whitespace tolerant; `None` → call sites use Unknown.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude_code" => Some(ClientSource::ClaudeCode),
            "codex" => Some(ClientSource::Codex),
            "qwen_code" => Some(ClientSource::QwenCode),
            "grok" => Some(ClientSource::Grok),
            "kimi_code" => Some(ClientSource::KimiCode),
            "dontspeak" => Some(ClientSource::DontSpeak),
            "unknown" => Some(ClientSource::Unknown),
            _ => None,
        }
    }

    /// Canonical token (hooks, IPC, log `client=`).
    pub fn as_str(self) -> &'static str {
        match self {
            ClientSource::ClaudeCode => "claude_code",
            ClientSource::Codex => "codex",
            ClientSource::QwenCode => "qwen_code",
            ClientSource::Grok => "grok",
            ClientSource::KimiCode => "kimi_code",
            ClientSource::DontSpeak => "dontspeak",
            ClientSource::Unknown => "unknown",
        }
    }

    pub fn is_client(self) -> bool {
        Self::CLIENTS.contains(&self)
    }
}

impl Serialize for ClientSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Unknown token → [`ClientSource::Unknown`]. Absent field fails closed at `Request::source`.
impl<'de> Deserialize<'de> for ClientSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(ClientSource::parse(&s).unwrap_or(ClientSource::Unknown))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_parse_serde_and_display() {
        for c in [
            ClientSource::ClaudeCode,
            ClientSource::Codex,
            ClientSource::QwenCode,
            ClientSource::Grok,
            ClientSource::KimiCode,
            ClientSource::DontSpeak,
            ClientSource::Unknown,
        ] {
            assert_eq!(ClientSource::parse(c.as_str()), Some(c), "{c:?}");
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(s, format!("\"{}\"", c.as_str()), "{c:?}");
            let back: ClientSource = serde_json::from_str(&s).unwrap();
            assert_eq!(back, c, "{c:?} round-trips through serde");
        }
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant_and_rejects_the_rest() {
        assert_eq!(
            ClientSource::parse("  Claude_Code \n"),
            Some(ClientSource::ClaudeCode)
        );
        assert_eq!(ClientSource::parse("gemini_cli"), None);
        assert_eq!(ClientSource::parse(""), None);
    }

    #[test]
    fn clients_is_the_wireable_subset_and_is_client_gates_on_it() {
        assert_eq!(
            ClientSource::CLIENTS,
            &[
                ClientSource::ClaudeCode,
                ClientSource::Codex,
                ClientSource::QwenCode,
                ClientSource::Grok,
                ClientSource::KimiCode
            ]
        );
        for &c in ClientSource::CLIENTS {
            assert!(c.is_client(), "{c:?} is wire-able");
        }
        // DontSpeak must never count as a client (`exclude_clients = ["dontspeak"]`);
        // Unknown is not wire-able either. Default fails open to Unknown.
        assert!(!ClientSource::DontSpeak.is_client());
        assert!(!ClientSource::Unknown.is_client());
        assert_eq!(ClientSource::default(), ClientSource::Unknown);
    }

    #[test]
    fn unknown_token_decodes_to_unknown_instead_of_erroring() {
        // Forward-skew: unknown token → Unknown, not hard-error (not legacy support).
        let c: ClientSource = serde_json::from_str(r#""gemini_cli""#).unwrap();
        assert_eq!(c, ClientSource::Unknown);
    }

    #[test]
    fn a_non_string_value_still_errors() {
        // Fail-open is for unrecognised strings only; wrong type still errors.
        assert!(serde_json::from_str::<ClientSource>("42").is_err());
        assert!(serde_json::from_str::<ClientSource>("null").is_err());
    }
}
