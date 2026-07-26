//! Client identity and execution-surface classification for wiring, hooks, MCP, launchers,
//! IPC, and log `client=`.
//! Leaf crate (breaks `ds-log` ↔ `ds-config` cycle).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const HERDR_PANE_ID_ENV: &str = "HERDR_PANE_ID";
const HERDR_SCOPE_PREFIX: &str = "dontspeak:herdr:HERDR_PANE_ID:";
/// Codex host-origin marker inherited by hooks and MCP children.
pub const CODEX_ORIGIN_ENV: &str = "CODEX_INTERNAL_ORIGINATOR_OVERRIDE";
/// Claude Code entrypoint marker inherited by Desktop Code children.
pub const CLAUDE_ENTRYPOINT_ENV: &str = "CLAUDE_CODE_ENTRYPOINT";
const CODEX_DESKTOP_ORIGIN: &str = "Codex Desktop";
const CLAUDE_DESKTOP_ENTRYPOINTS: &[&str] = &["claude-desktop", "claude-desktop-3p"];
const CLAUDE_DESKTOP_MCP_NAMES: &[&str] = &["claude-ai", "claude-desktop"];

/// Queue identity shared by hooks and MCP children inside one Herdr pane.
pub fn herdr_queue_scope(pane_id: &str) -> String {
    format!("{HERDR_SCOPE_PREFIX}{pane_id}")
}

/// Recover the public Herdr pane id used by `agent.list` and pane events.
pub fn herdr_pane_id(scope: &str) -> Option<&str> {
    scope
        .strip_prefix(HERDR_SCOPE_PREFIX)
        .filter(|value| !value.is_empty())
}

/// A client supported by DontSpeak's wiring registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WiredAgent {
    ClaudeCode,
    Codex,
    QwenCode,
    Grok,
    KimiCode,
    Hermes,
}

impl WiredAgent {
    /// Every wired client in token order (registry drift-tested).
    pub const ALL: &'static [WiredAgent] = &[
        WiredAgent::ClaudeCode,
        WiredAgent::Codex,
        WiredAgent::QwenCode,
        WiredAgent::Grok,
        WiredAgent::KimiCode,
        WiredAgent::Hermes,
    ];

    /// One canonical identity used for parsing, serialization, launch, IPC, logs, and MCP.
    pub const fn as_str(self) -> &'static str {
        match self {
            WiredAgent::ClaudeCode => "claude",
            WiredAgent::Codex => "codex",
            WiredAgent::QwenCode => "qwen",
            WiredAgent::Grok => "grok",
            WiredAgent::KimiCode => "kimi",
            WiredAgent::Hermes => "hermes",
        }
    }

    /// Case/whitespace tolerant; unwired tokens return `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let token = s.trim();
        Self::ALL
            .iter()
            .copied()
            .find(|client| token.eq_ignore_ascii_case(client.as_str()))
    }

    /// Exact normalized `clientInfo.name` identities accepted for MCP attribution.
    pub const fn mcp_names(self) -> &'static [&'static str] {
        match self {
            WiredAgent::ClaudeCode => &["claude", "claude-code", "claude-ai", "claude-desktop"],
            WiredAgent::Codex => &["codex", "codex-mcp-client", "codex-vscode"],
            WiredAgent::QwenCode => &["qwen", "qwen-code", "qwen-cli-mcp-client-dontspeak"],
            WiredAgent::Grok => &["grok", "grok-shell-dontspeak"],
            WiredAgent::KimiCode => &["kimi", "kimi-code"],
            WiredAgent::Hermes => &["hermes", "hermes-agent-dontspeak"],
        }
    }
}

/// Product family, independent of the executable or UI that hosts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientFamily {
    Claude,
    Codex,
    Qwen,
    Grok,
    Kimi,
    Hermes,
}

impl From<WiredAgent> for ClientFamily {
    fn from(client: WiredAgent) -> Self {
        match client {
            WiredAgent::ClaudeCode => Self::Claude,
            WiredAgent::Codex => Self::Codex,
            WiredAgent::QwenCode => Self::Qwen,
            WiredAgent::Grok => Self::Grok,
            WiredAgent::KimiCode => Self::Kimi,
            WiredAgent::Hermes => Self::Hermes,
        }
    }
}

/// Execution surface whose visible-output contract may differ within one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSurface {
    TerminalCli,
    DesktopApp,
    Unknown,
}

/// One classification consumed by hooks, MCP initialization, and client launchers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientContext {
    pub client: Option<WiredAgent>,
    pub family: Option<ClientFamily>,
    pub surface: ClientSurface,
}

impl ClientContext {
    /// Hook context from the wired identity and inherited host markers.
    pub fn for_hook(client: WiredAgent) -> Self {
        Self::for_wired_with_markers(
            client,
            std::env::var(CODEX_ORIGIN_ENV).ok().as_deref(),
            std::env::var(CLAUDE_ENTRYPOINT_ENV).ok().as_deref(),
        )
    }

    /// MCP context from raw `clientInfo.name` and inherited host markers.
    pub fn for_mcp_name(name: &str) -> Self {
        Self::for_mcp_name_with_markers(
            name,
            std::env::var(CODEX_ORIGIN_ENV).ok().as_deref(),
            std::env::var(CLAUDE_ENTRYPOINT_ENV).ok().as_deref(),
        )
    }

    /// A launcher always creates a terminal client, even when its parent is Desktop-owned.
    pub const fn for_launcher(client: WiredAgent) -> Self {
        Self {
            client: Some(client),
            family: Some(match client {
                WiredAgent::ClaudeCode => ClientFamily::Claude,
                WiredAgent::Codex => ClientFamily::Codex,
                WiredAgent::QwenCode => ClientFamily::Qwen,
                WiredAgent::Grok => ClientFamily::Grok,
                WiredAgent::KimiCode => ClientFamily::Kimi,
                WiredAgent::Hermes => ClientFamily::Hermes,
            }),
            surface: ClientSurface::TerminalCli,
        }
    }

    /// Classify a wired hook identity using explicit, injectable host markers.
    pub fn for_wired_with_markers(
        client: WiredAgent,
        codex_origin: Option<&str>,
        claude_entrypoint: Option<&str>,
    ) -> Self {
        let family = ClientFamily::from(client);
        let desktop = match family {
            ClientFamily::Codex => {
                codex_origin.is_some_and(|value| value.trim() == CODEX_DESKTOP_ORIGIN)
            }
            ClientFamily::Claude => claude_entrypoint
                .is_some_and(|value| CLAUDE_DESKTOP_ENTRYPOINTS.contains(&value.trim())),
            _ => false,
        };
        Self {
            client: Some(client),
            family: Some(family),
            surface: if desktop {
                ClientSurface::DesktopApp
            } else {
                ClientSurface::TerminalCli
            },
        }
    }

    /// Classify raw MCP identity using exact names and explicit, injectable host markers.
    pub fn for_mcp_name_with_markers(
        name: &str,
        codex_origin: Option<&str>,
        claude_entrypoint: Option<&str>,
    ) -> Self {
        let normalized = normalize_mcp_name(name);
        let client = WiredAgent::ALL
            .iter()
            .copied()
            .find(|client| client.mcp_names().contains(&normalized.as_str()));
        let Some(client) = client else {
            return Self {
                client: None,
                family: None,
                surface: ClientSurface::Unknown,
            };
        };
        let mut context = Self::for_wired_with_markers(client, codex_origin, claude_entrypoint);
        if CLAUDE_DESKTOP_MCP_NAMES.contains(&normalized.as_str()) {
            context.surface = ClientSurface::DesktopApp;
        }
        context
    }

    /// Desktop text surfaces do not accept the model-facing narration contract or hook effects.
    pub const fn allows_narration(self) -> bool {
        !matches!(self.surface, ClientSurface::DesktopApp)
    }

    /// Marker a terminal launcher must remove so its child cannot be mistaken for Desktop.
    pub const fn inherited_desktop_marker(self) -> Option<&'static str> {
        match self.family {
            Some(ClientFamily::Codex) => Some(CODEX_ORIGIN_ENV),
            Some(ClientFamily::Claude) => Some(CLAUDE_ENTRYPOINT_ENV),
            _ => None,
        }
    }
}

fn normalize_mcp_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

impl Serialize for WiredAgent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// Unknown tokens fail closed.
impl<'de> Deserialize<'de> for WiredAgent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        WiredAgent::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown wired client {s:?}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_client_round_trips_parse_serde_and_display() {
        for &c in WiredAgent::ALL {
            assert_eq!(WiredAgent::parse(c.as_str()), Some(c), "{c:?}");
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(s, format!("\"{}\"", c.as_str()), "{c:?}");
            let back: WiredAgent = serde_json::from_str(&s).unwrap();
            assert_eq!(back, c, "{c:?} round-trips through serde");
        }
    }

    #[test]
    fn parse_is_case_and_whitespace_tolerant_and_rejects_the_rest() {
        assert_eq!(
            WiredAgent::parse("  ClAuDe \n"),
            Some(WiredAgent::ClaudeCode)
        );
        assert_eq!(WiredAgent::parse("gemini_cli"), None);
        assert_eq!(WiredAgent::parse(""), None);
    }

    #[test]
    fn all_contains_the_complete_token_set() {
        assert_eq!(
            WiredAgent::ALL
                .iter()
                .copied()
                .map(WiredAgent::as_str)
                .collect::<Vec<_>>(),
            ["claude", "codex", "qwen", "grok", "kimi", "hermes"]
        );
    }

    #[test]
    fn exact_mcp_identities_do_not_guess_by_prefix() {
        for (name, client) in [
            ("claude-code", WiredAgent::ClaudeCode),
            ("claude-ai", WiredAgent::ClaudeCode),
            ("claude_desktop", WiredAgent::ClaudeCode),
            ("codex-mcp-client", WiredAgent::Codex),
            ("qwen-cli-mcp-client-DontSpeak", WiredAgent::QwenCode),
        ] {
            assert_eq!(
                ClientContext::for_mcp_name_with_markers(name, None, None).client,
                Some(client),
                "{name}"
            );
        }
        for name in [
            "claude-code-fork",
            "codex-community-fork",
            "claude-desktop-beta",
        ] {
            assert_eq!(
                ClientContext::for_mcp_name_with_markers(name, None, None),
                ClientContext {
                    client: None,
                    family: None,
                    surface: ClientSurface::Unknown,
                },
                "{name}"
            );
        }
    }

    #[test]
    fn codex_and_claude_desktop_markers_change_only_their_own_family() {
        assert_eq!(
            ClientContext::for_wired_with_markers(
                WiredAgent::Codex,
                Some(CODEX_DESKTOP_ORIGIN),
                None,
            )
            .surface,
            ClientSurface::DesktopApp
        );
        assert_eq!(
            ClientContext::for_wired_with_markers(
                WiredAgent::ClaudeCode,
                None,
                Some("claude-desktop-3p"),
            )
            .surface,
            ClientSurface::DesktopApp
        );
        assert_eq!(
            ClientContext::for_wired_with_markers(
                WiredAgent::ClaudeCode,
                Some(CODEX_DESKTOP_ORIGIN),
                None,
            )
            .surface,
            ClientSurface::TerminalCli
        );
        assert_eq!(
            ClientContext::for_wired_with_markers(WiredAgent::Codex, None, Some("claude-desktop"),)
                .surface,
            ClientSurface::TerminalCli
        );
    }

    #[test]
    fn raw_claude_chat_identity_is_desktop_without_an_env_marker() {
        for name in ["claude-ai", "CLAUDE_DESKTOP"] {
            let context = ClientContext::for_mcp_name_with_markers(name, None, None);
            assert_eq!(context.family, Some(ClientFamily::Claude), "{name}");
            assert_eq!(context.surface, ClientSurface::DesktopApp, "{name}");
            assert!(!context.allows_narration(), "{name}");
        }
    }

    #[test]
    fn non_agent_token_fails_deserialization() {
        assert!(serde_json::from_str::<WiredAgent>(r#""gemini_cli""#).is_err());
    }

    #[test]
    fn a_non_string_value_still_errors() {
        assert!(serde_json::from_str::<WiredAgent>("42").is_err());
        assert!(serde_json::from_str::<WiredAgent>("null").is_err());
    }

    #[test]
    fn herdr_scope_round_trips_public_pane_ids_with_colons() {
        let scope = herdr_queue_scope("workspace:p7");
        assert_eq!(herdr_pane_id(&scope), Some("workspace:p7"));
        assert_eq!(herdr_pane_id("dontspeak:launch:other"), None);
    }
}
