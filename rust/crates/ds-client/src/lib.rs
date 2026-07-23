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
    Hermes,
    /// Internal; `client_spec` is always `None`.
    DontSpeak,
    /// Foreign MCP / missing `--client`.
    #[default]
    Unknown,
}

struct ClientNames {
    token: &'static str,
    launch_command: Option<&'static str>,
    launch_aliases: &'static [&'static str],
    mcp_client_prefix: Option<&'static str>,
}

impl ClientSource {
    /// Wireable clients in token order (registry drift-tested).
    pub const CLIENTS: &'static [ClientSource] = &[
        ClientSource::ClaudeCode,
        ClientSource::Codex,
        ClientSource::QwenCode,
        ClientSource::Grok,
        ClientSource::KimiCode,
        ClientSource::Hermes,
    ];

    const fn names(self) -> ClientNames {
        match self {
            ClientSource::ClaudeCode => ClientNames {
                token: "claude_code",
                launch_command: Some("claude"),
                launch_aliases: &[],
                mcp_client_prefix: Some("claude-code"),
            },
            ClientSource::Codex => ClientNames {
                token: "codex",
                launch_command: Some("codex"),
                launch_aliases: &[],
                mcp_client_prefix: Some("codex"),
            },
            ClientSource::QwenCode => ClientNames {
                token: "qwen_code",
                launch_command: Some("qwen"),
                launch_aliases: &[],
                mcp_client_prefix: Some("qwen"),
            },
            ClientSource::Grok => ClientNames {
                token: "grok",
                launch_command: Some("grok"),
                launch_aliases: &[],
                mcp_client_prefix: Some("grok"),
            },
            ClientSource::KimiCode => ClientNames {
                token: "kimi_code",
                launch_command: Some("kimi"),
                launch_aliases: &["kimi-code"],
                mcp_client_prefix: Some("kimi-code"),
            },
            ClientSource::Hermes => ClientNames {
                token: "hermes",
                launch_command: Some("hermes"),
                launch_aliases: &[],
                mcp_client_prefix: Some("hermes"),
            },
            ClientSource::DontSpeak => ClientNames {
                token: "dontspeak",
                launch_command: None,
                launch_aliases: &[],
                mcp_client_prefix: None,
            },
            ClientSource::Unknown => ClientNames {
                token: "unknown",
                launch_command: None,
                launch_aliases: &[],
                mcp_client_prefix: None,
            },
        }
    }

    /// Canonical executable for wireable clients.
    pub const fn launch_command(self) -> Option<&'static str> {
        self.names().launch_command
    }

    /// Genuine compatibility spellings for the launcher surface.
    pub const fn launch_aliases(self) -> &'static [&'static str] {
        self.names().launch_aliases
    }

    /// Normalized MCP `clientInfo.name` prefix for wireable clients.
    pub const fn mcp_client_prefix(self) -> Option<&'static str> {
        self.names().mcp_client_prefix
    }

    /// Case/whitespace tolerant; `None` → call sites use Unknown.
    pub fn parse(s: &str) -> Option<Self> {
        let token = s.trim();
        Self::CLIENTS
            .iter()
            .copied()
            .chain([Self::DontSpeak, Self::Unknown])
            .find(|client| token.eq_ignore_ascii_case(client.as_str()))
    }

    /// Canonical token (hooks, IPC, log `client=`).
    pub const fn as_str(self) -> &'static str {
        self.names().token
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
        for c in ClientSource::CLIENTS
            .iter()
            .copied()
            .chain([ClientSource::DontSpeak, ClientSource::Unknown])
        {
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
        for &c in ClientSource::CLIENTS {
            assert!(c.is_client(), "{c:?} is wire-able");
            assert!(c.launch_command().is_some(), "{c:?} is launchable");
            assert!(
                c.mcp_client_prefix().is_some(),
                "{c:?} is identifiable over MCP"
            );
        }
        // DontSpeak must never count as a client (`exclude_clients = ["dontspeak"]`);
        // Unknown is not wire-able either. Default fails open to Unknown.
        assert!(!ClientSource::DontSpeak.is_client());
        assert!(!ClientSource::Unknown.is_client());
        assert_eq!(ClientSource::DontSpeak.launch_command(), None);
        assert_eq!(ClientSource::Unknown.launch_command(), None);
        assert_eq!(ClientSource::DontSpeak.mcp_client_prefix(), None);
        assert_eq!(ClientSource::Unknown.mcp_client_prefix(), None);
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
