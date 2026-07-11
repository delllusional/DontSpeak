//! The ONE client-identity enum.
//!
//! WHO is talking to DontSpeak — the AI client (Claude Code, OpenAI Codex, Qwen Code, Grok),
//! DontSpeak itself, or a caller we cannot name. The SAME [`ClientSource`] flows through
//! every path DontSpeak is talked to on: the wiring registry (`ds_config::CLIENT_REGISTRY`),
//! the hook command line (`--client <token>`, stamped by `ds_config::wire::cmdline`), the
//! `ds-ipc` `Request::source` field, the MCP `initialize` handshake's `clientInfo.name`, and
//! the trailing `client=<token>` key on an activity-log line.
//!
//! ## Why a leaf crate of its own
//!
//! `ds-log` must be able to take a client identity, and `ds-log` MUST NOT depend on
//! `ds-config` — `ds-config` depends on `ds-log` for `VoiceConfig::load`'s diagnostic, so the
//! reverse edge is a Cargo cycle (see `ds-log/src/log.rs`'s `default_log_file` doc). `ds-ipc`
//! needs the type too and today depends on nothing but serde. So the enum sits BELOW both,
//! in its own crate, rather than in `ds-config` where its ancestor (`WireTarget`) lived.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// WHO is talking to DontSpeak. The ONE client identity, shared by the wiring, the hook
/// command line, the `ds-ipc` protocol, the MCP tool-call path, and the activity log.
///
/// The four [`ClientSource::CLIENTS`] members are the WIRE-ABLE clients — the set the client
/// registry pins, `wire --all` iterates, and the engine's boot-time reconcile converges. The
/// other two are the non-wireable ends of the same identity axis: DontSpeak itself, and
/// "we don't know".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientSource {
    /// Claude Code — hooks in `~/.claude/settings.json` + the MCP server in `~/.claude.json`.
    ClaudeCode,
    /// OpenAI Codex — hooks + MCP in `~/.codex/config.toml`.
    Codex,
    /// Qwen Code — hooks + MCP in `~/.qwen/settings.json`.
    QwenCode,
    /// Grok (Grok Build) CLI — hooks in `~/.grok/hooks/dontspeak.json`, MCP in
    /// `~/.grok/config.toml`.
    Grok,
    /// DontSpeak itself — the host app, the engine, the CLI, the warm helper. Never wired
    /// (there is no registry entry for it, by design: `client_spec(DontSpeak)` is `None`).
    DontSpeak,
    /// A caller we cannot name: a foreign MCP client whose `clientInfo.name` is not in the
    /// registry's alias table, or a `dontspeak` binary invoked by hand with no `--client`.
    ///
    /// This is NOT a legacy/compat value and must not be deleted as one. It is the honest
    /// answer to "who is this?" when we genuinely do not know — a domain value, not a shim.
    #[default]
    Unknown,
}

impl ClientSource {
    /// Every WIRE-ABLE client, in canonical-token order — the SAME list the client registry
    /// pins (`registry_matches_the_canonical_client_list`). The single source for
    /// `wire --all`, the engine's boot-time `ds_wire::reconcile`, and the per-platform
    /// installers. `DontSpeak`/`Unknown` are deliberately NOT here: neither is a client we
    /// wire anything into.
    pub const CLIENTS: &'static [ClientSource] = &[
        ClientSource::ClaudeCode,
        ClientSource::Codex,
        ClientSource::QwenCode,
        ClientSource::Grok,
    ];

    /// Parse a canonical token (case/whitespace tolerant). `None` for anything else —
    /// callers that must not hard-fail (the `ds-ipc` decode, the CLI's `--client` scan) map
    /// that to [`ClientSource::Unknown`] themselves, so the fail-open decision stays visible
    /// at each call site instead of being baked in here.
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

    /// The canonical lowercase token (round-trips through [`ClientSource::parse`]). This is
    /// what the hook command line carries (`--client claude_code`), what the `ds-ipc` wire
    /// carries (`"source":"codex"`), and what a log line's `client=<token>` suffix shows.
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

    /// Is this one of the WIRE-ABLE [`ClientSource::CLIENTS`]? The gate every "which client do
    /// I wire / attribute this hook to" path uses — `parse()` accepts `dontspeak`/`unknown`
    /// too, and neither may enter a client set (`exclude_clients`, the CLI's `--client` scan,
    /// `dontspeak wire <token>`).
    pub fn is_client(self) -> bool {
        Self::CLIENTS.contains(&self)
    }
}

/// Serialize as the canonical `as_str()` token — the exact behaviour `ds-config`'s
/// `serialize_as_str!` macro gave `WireTarget`, hand-written here so this crate needs no
/// macro (and no dependency on `ds-config`, which is the whole point of the split).
impl Serialize for ClientSource {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// FAIL-OPEN on an unrecognised TOKEN: a string we don't know decodes to
/// [`ClientSource::Unknown`] instead of erroring. A non-string value still errors.
///
/// This is FORWARD-skew robustness, not backward compatibility: a client we have not wired
/// YET must not hard-error a whole `ds-ipc` `Request` line. It mirrors `ds_ipc::Response`'s
/// `#[serde(other)]`, which exists for exactly the same reason.
///
/// Note the ASYMMETRY, and it is deliberate: an unrecognised TOKEN fails open, but an ABSENT
/// FIELD fails closed — `ds_ipc::Request::source` is a REQUIRED field, and a line that omits
/// it is a hard decode error (pinned by `ds-ipc`'s
/// `request_without_source_is_a_hard_decode_error`). Nothing here says otherwise: this impl
/// only ever sees a value that IS present.
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
        // The two non-client members: DontSpeak can never mark ITSELF a client (that would
        // let `exclude_clients = ["dontspeak"]` through), and Unknown is not wire-able either.
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
        // FORWARD-SKEW robustness (NOT legacy support): a client we have not wired yet sends a
        // token this build doesn't know. It must decode to `Unknown`, not hard-error the line —
        // same idiom as `ds_ipc::Response::Unknown`'s `#[serde(other)]`.
        let c: ClientSource = serde_json::from_str(r#""gemini_cli""#).unwrap();
        assert_eq!(c, ClientSource::Unknown);
    }

    #[test]
    fn a_non_string_value_still_errors() {
        // Fail-open covers an unrecognised STRING only — a structurally wrong value is a real
        // decode error, and stays one.
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
