//! The ONE client-identity enum.
//!
//! WHO is talking to DontSpeak — an AI client, DontSpeak itself, or a caller we
//! cannot name. The same [`ClientSource`] flows through the wiring registry, hook
//! cmdline (`--client`), `ds-ipc` `Request::source`, MCP `clientInfo.name`, and the
//! activity-log `client=<token>` key.
//!
//! ## Why a leaf crate
//!
//! `ds-log` needs a client identity but must not depend on `ds-config` (`ds-config`
//! depends on `ds-log` for `VoiceConfig::load` diagnostics — reverse edge = cycle;
//! see `ds-log`'s `default_log_file`). `ds-ipc` needs the type with no heavy deps.
//! So the enum sits below both, not in `ds-config` (where ancestor `WireTarget` lived).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// WHO is talking to DontSpeak — shared by wiring, hooks, `ds-ipc`, MCP, and the log.
///
/// [`ClientSource::CLIENTS`] are the wire-able clients. `DontSpeak` / `Unknown` are the
/// non-wireable ends of the same axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClientSource {
    /// Claude Code — hooks in `~/.claude/settings.json` + MCP in `~/.claude.json`.
    ClaudeCode,
    /// OpenAI Codex — hooks + MCP in `~/.codex/config.toml`.
    Codex,
    /// Qwen Code — hooks + MCP in `~/.qwen/settings.json`.
    QwenCode,
    /// Grok (Grok Build) — hooks in `~/.grok/hooks/dontspeak.json`, MCP in `~/.grok/config.toml`.
    Grok,
    /// DontSpeak itself. Never wired (`client_spec(DontSpeak)` is `None`).
    DontSpeak,
    /// Unknown caller (foreign MCP name, or no `--client`). Domain value, not a legacy shim —
    /// do not delete as "compat".
    #[default]
    Unknown,
}

impl ClientSource {
    /// Wire-able clients in canonical-token order — single source for `wire --all`, boot
    /// reconcile, and installers. Registry pins this list (`registry_matches_the_canonical_client_list`).
    /// `DontSpeak`/`Unknown` are deliberately omitted.
    pub const CLIENTS: &'static [ClientSource] = &[
        ClientSource::ClaudeCode,
        ClientSource::Codex,
        ClientSource::QwenCode,
        ClientSource::Grok,
    ];

    /// Parse a canonical token (case/whitespace tolerant). `None` for anything else —
    /// fail-open to [`ClientSource::Unknown`] stays at each call site (IPC decode, `--client`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude_code" => Some(ClientSource::ClaudeCode),
            "codex" => Some(ClientSource::Codex),
            "qwen_code" => Some(ClientSource::QwenCode),
            "grok" => Some(ClientSource::Grok),
            "dontspeak" => Some(ClientSource::DontSpeak),
            "unknown" => Some(ClientSource::Unknown),
            _ => None,
        }
    }

    /// Canonical lowercase token (round-trips through [`ClientSource::parse`]) — hook cmdline,
    /// `ds-ipc` wire, log `client=` suffix.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientSource::ClaudeCode => "claude_code",
            ClientSource::Codex => "codex",
            ClientSource::QwenCode => "qwen_code",
            ClientSource::Grok => "grok",
            ClientSource::DontSpeak => "dontspeak",
            ClientSource::Unknown => "unknown",
        }
    }

    /// Wire-able? Gate for client sets / `wire <token>`; `parse` also accepts non-clients.
    pub fn is_client(self) -> bool {
        Self::CLIENTS.contains(&self)
    }
}

/// Serialize as the `as_str()` token (hand-written so this crate needs no `ds-config` macro).
impl Serialize for ClientSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Fail-open on unrecognised TOKEN → [`ClientSource::Unknown`]; non-string still errors.
///
/// Forward-skew (not legacy): an as-yet-unwired client must not hard-error a `ds-ipc` line
/// (same idea as `ds_ipc::Response`'s `#[serde(other)]`).
///
/// Deliberate asymmetry: unrecognised TOKEN fails open; ABSENT field fails closed —
/// `Request::source` is required (pinned by `request_without_source_is_a_hard_decode_error`).
/// This impl only sees a present value.
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
    fn every_variant_round_trips_through_parse_and_as_str() {
        for c in [
            ClientSource::ClaudeCode,
            ClientSource::Codex,
            ClientSource::QwenCode,
            ClientSource::Grok,
            ClientSource::DontSpeak,
            ClientSource::Unknown,
        ] {
            assert_eq!(ClientSource::parse(c.as_str()), Some(c), "{c:?}");
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
                ClientSource::Grok
            ]
        );
        for &c in ClientSource::CLIENTS {
            assert!(c.is_client(), "{c:?} is wire-able");
        }
        // DontSpeak must never count as a client (`exclude_clients = ["dontspeak"]`);
        // Unknown is not wire-able either.
        assert!(!ClientSource::DontSpeak.is_client());
        assert!(!ClientSource::Unknown.is_client());
    }

    #[test]
    fn default_is_unknown() {
        assert_eq!(ClientSource::default(), ClientSource::Unknown);
    }

    #[test]
    fn serializes_as_the_bare_token() {
        assert_eq!(
            serde_json::to_string(&ClientSource::QwenCode).unwrap(),
            r#""qwen_code""#
        );
        assert_eq!(
            serde_json::to_string(&ClientSource::Unknown).unwrap(),
            r#""unknown""#
        );
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

    #[test]
    fn every_token_round_trips_through_serde() {
        for c in [
            ClientSource::ClaudeCode,
            ClientSource::Codex,
            ClientSource::QwenCode,
            ClientSource::Grok,
            ClientSource::DontSpeak,
            ClientSource::Unknown,
        ] {
            let s = serde_json::to_string(&c).unwrap();
            let back: ClientSource = serde_json::from_str(&s).unwrap();
            assert_eq!(back, c, "{c:?} round-trips through serde");
        }
    }
}
