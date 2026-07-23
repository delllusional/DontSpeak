//! Client registry — declarative catalog of wireable AI clients.
//! Each [`ClientSpec`]: identity, paths ([`Paths`]), mechanisms, launch, docs.
//! `wire` iterates surfaces; add a client = CLIENTS + Paths + entry (no IO here).

use std::path::Path;

use crate::client_binary::resolve_client_binary;
use crate::paths::Paths;
use ds_client::WiredAgent;

/// Where config lives by convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Terminal CLI — `$HOME` dot-dir (`~/.claude`, …). Every registered client is one;
    /// add a variant when a desktop-GUI client with app-support config appears.
    TerminalCli,
}

/// How `dontspeak <client>` launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Start after ensuring the resident host is running.
    Direct,
    /// Engine on app-server, then Codex TUI `--remote`. Noninteractive passes through.
    CodexRemote,
}

/// Launch behavior; the command is always the target's canonical identity.
pub struct LaunchSpec {
    pub mode: LaunchMode,
}

/// How one surface is written. Additive + idempotent + user-preserving;
/// writers in `dontspeak`, pure shapers in sibling `wire::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireMechanism {
    /// JSON Claude-contract hooks (`hooks.<Event>`). `merge_hooks`/`strip_hooks`.
    ClaudeJsonHooks,
    /// TOML same contract (`[[hooks.<Event>]]`). `merge_codex_hooks`/`strip_codex_hooks`.
    ClaudeTomlHooks,
    /// Owned file `~/.grok/hooks/dontspeak.json`: bare bin + seconds; Grok dedupes after
    /// dropping `args`; `GROK_HOOK_EVENT`. Wire overwrites (backup), unwire deletes.
    /// `grok_hooks_value`.
    GrokJsonHooks,
    /// Flat `[[hooks]]` — only `event`/`matcher`/`command`/`timeout` (extra keys break Kimi).
    /// `merge_kimi_hooks`/`strip_kimi_hooks` (vs grouped ClaudeTomlHooks).
    KimiTomlHooks,
    /// Hermes nested `hooks.<event>: [{command, timeout}]` in config.yaml.
    /// `merge_hermes_hooks`/`strip_hermes_hooks` (YAML; comment loss on re-emit).
    HermesYamlHooks,
    /// Hermes `mcp_servers.DontSpeak` in the same config.yaml.
    /// `merge_hermes_mcp`/`strip_hermes_mcp`.
    HermesYamlMcp,
    /// Hermes `shell-hooks-allowlist.json` approvals for `(event, command)` consent.
    /// `merge_hermes_allowlist`/`strip_hermes_allowlist`.
    HermesShellAllowlist,
    /// JSON `mcpServers.DontSpeak`. `merge_mcp_server`/`strip_mcp_server`.
    JsonMcp,
    /// TOML `mcp_servers.DontSpeak`. `merge_mcp_server_toml`/`strip_mcp_server_toml`.
    TomlMcp,
}

/// Hook runner dialect for `ClaudeJsonHooks`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookCommandStyle {
    /// Spawn `command`+`args`; timeout SECONDS.
    ArgsArray,
    /// Shell `command` only (no `args`); timeout MILLISECONDS (default 60000).
    InlineShell,
}

/// One config file the wire edits.
pub struct Surface {
    pub mechanism: WireMechanism,
    /// Per-OS path via [`Paths`].
    pub config_file: fn(&Paths) -> &Path,
    /// Post-wire load hint for MCP surfaces; hooks take effect next turn.
    pub load_hint: Option<&'static str>,
    /// `ClaudeJsonHooks`: wire `MessageDisplay` when true; else voice from `Stop`.
    /// MCP / ClaudeTomlHooks ignore (Codex streaming is in its shaper).
    pub hook_streaming: bool,
    /// `ClaudeJsonHooks` only.
    pub hook_command_style: HookCommandStyle,
}

/// Upstream doc the wiring was derived from.
pub struct DocRef {
    /// `"hooks"` or `"mcp"`.
    pub topic: &'static str,
    pub url: &'static str,
}

/// One wireable client.
pub struct ClientSpec {
    /// Canonical token; always in [`WiredAgent::ALL`].
    pub target: WiredAgent,
    pub display_name: &'static str,
    pub kind: ClientKind,
    pub launch: LaunchSpec,
    /// Client-owned configuration root, reported separately from executable presence.
    pub client_config_dir: fn(&Paths) -> &Path,
    pub surfaces: &'static [Surface],
    pub docs: &'static [DocRef],
    /// Client version when wiring last verified against [`docs`](Self::docs) (not a floor).
    pub verified_client_version: &'static str,
    /// ISO date of that verification (`YYYY-MM-DD`).
    pub verified_on: &'static str,
}

impl ClientSpec {
    /// Presence uses the same configured-binary resolver as launch and usage. A dot-dir can
    /// outlive uninstall, while a resolvable executable reflects the current installation.
    pub fn present(&self, paths: &Paths) -> bool {
        resolve_client_binary(self.target, paths).is_some()
    }
}

/// Order matches [`WiredAgent::ALL`] (pinned by test).
pub const CLIENT_REGISTRY: &[ClientSpec] = &[
    ClientSpec {
        target: WiredAgent::ClaudeCode,
        display_name: "Claude Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::Direct,
        },
        // Verified: announces as `claude-code` in `clientInfo.name`.
        client_config_dir: |p| &p.claude_dir,
        surfaces: &[
            Surface {
                mechanism: WireMechanism::ClaudeJsonHooks,
                config_file: |p| &p.settings_json, // ~/.claude/settings.json
                load_hint: None,
                hook_streaming: true,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
            Surface {
                mechanism: WireMechanism::JsonMcp,
                config_file: |p| &p.claude_code_config, // ~/.claude.json (user scope)
                load_hint: Some("start a new Claude Code session to load the server"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "hooks",
                url: "https://code.claude.com/docs/en/hooks",
            },
            DocRef {
                topic: "mcp",
                url: "https://code.claude.com/docs/en/mcp",
            },
        ],
        verified_client_version: "2.1.210",
        verified_on: "2026-07-15",
    },
    ClientSpec {
        target: WiredAgent::Codex,
        display_name: "OpenAI Codex",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::CodexRemote,
        },
        // `codex-mcp-client`; prefix covers CLI + VS Code.
        client_config_dir: |p| &p.codex_dir,
        // TOML hooks: SessionStart greet-only, UserPromptSubmit notify+provide, Stop.
        // No SessionEnd/Notification (engine codex_stream). Mid-turn = app-server subscriber.
        // MCP same config.toml. session id = thread id.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::ClaudeTomlHooks,
                config_file: |p| &p.codex_config, // ~/.codex/config.toml
                load_hint: None,
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
            Surface {
                mechanism: WireMechanism::TomlMcp,
                config_file: |p| &p.codex_config, // same file
                load_hint: Some("start a new Codex session or run `codex mcp list` to verify"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "hooks",
                url: "https://developers.openai.com/codex/hooks",
            },
            DocRef {
                topic: "config",
                url: "https://github.com/openai/codex/blob/main/docs/config.md",
            },
            DocRef {
                topic: "app-server",
                url: "https://developers.openai.com/codex/app-server",
            },
            DocRef {
                topic: "mcp",
                url: "https://developers.openai.com/codex/mcp",
            },
        ],
        verified_client_version: "0.144.4",
        verified_on: "2026-07-15",
    },
    ClientSpec {
        target: WiredAgent::QwenCode,
        display_name: "Qwen Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::Direct,
        },
        // Live: `qwen-cli-mcp-client-DontSpeak` and older `qwen-code*` use this prefix.
        client_config_dir: |p| &p.qwen_dir,
        // JSON hooks, InlineShell (ms). Hooks+MCP share settings.json. Streaming pinned by
        // `inline_streaming_wires_messagedisplay_with_ms_timeout_and_plain_sessionstart`.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::ClaudeJsonHooks,
                config_file: |p| &p.qwen_settings, // ~/.qwen/settings.json
                load_hint: None,
                hook_streaming: true,
                hook_command_style: HookCommandStyle::InlineShell,
            },
            Surface {
                mechanism: WireMechanism::JsonMcp,
                config_file: |p| &p.qwen_settings, // same file
                load_hint: Some("start a new Qwen Code session to load the server"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "hooks",
                url: "https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/hooks.md",
            },
            DocRef {
                topic: "mcp",
                url: "https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/mcp.md",
            },
        ],
        verified_client_version: "0.19.10",
        verified_on: "2026-07-15",
    },
    ClientSpec {
        target: WiredAgent::Grok,
        display_name: "Grok",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::Direct,
        },
        // Live: `grok-shell-DontSpeak` uses this prefix.
        client_config_dir: |p| &p.grok_dir,
        // MCP TomlMcp; hooks own file. Bare command dedupes with imported Claude;
        // GROK_HOOK_EVENT vs no-arg MCP. Stop → chat_history; mid-turn = updates.jsonl tail;
        // digests → AGENTS.md (#95).
        surfaces: &[
            Surface {
                mechanism: WireMechanism::GrokJsonHooks,
                config_file: |p| &p.grok_hooks_json, // ~/.grok/hooks/dontspeak.json
                load_hint: None,
                hook_streaming: false, // mid-turn = updates.jsonl tail
                hook_command_style: HookCommandStyle::ArgsArray,
            },
            Surface {
                mechanism: WireMechanism::TomlMcp,
                config_file: |p| &p.grok_config,
                load_hint: Some("start a new Grok session or run `grok mcp list` / `grok inspect`"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "mcp",
                url: "https://docs.x.ai/build/features/mcp-servers",
            },
            DocRef {
                topic: "hooks",
                url: "https://docs.x.ai/build/features/hooks",
            },
        ],
        verified_client_version: "0.2.101",
        verified_on: "2026-07-15",
    },
    ClientSpec {
        target: WiredAgent::KimiCode,
        display_name: "Kimi Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::Direct,
        },
        client_config_dir: |p| &p.kimi_dir,
        // Flat [[hooks]] only event/matcher/command/timeout; greet-only SessionStart;
        // has SessionEnd+Notification. MCP separate mcp.json. KIMI_CODE_HOME → Paths::resolve.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::KimiTomlHooks,
                config_file: |p| &p.kimi_config_toml, // ~/.kimi-code/config.toml
                load_hint: None,
                hook_streaming: false,
                hook_command_style: HookCommandStyle::InlineShell,
            },
            Surface {
                mechanism: WireMechanism::JsonMcp,
                config_file: |p| &p.kimi_mcp_json, // ~/.kimi-code/mcp.json
                load_hint: Some("start a new Kimi Code session to load the server"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "hooks",
                url: "https://www.kimi.com/code/docs/en/kimi-code-cli/customization/hooks.html",
            },
            DocRef {
                topic: "mcp",
                url: "https://www.kimi.com/code/docs/en/kimi-code-cli/customization/mcp.html",
            },
        ],
        verified_client_version: "0.27.0",
        verified_on: "2026-07-18",
    },
    ClientSpec {
        target: WiredAgent::Hermes,
        display_name: "Hermes Agent",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            mode: LaunchMode::Direct,
        },
        // A generic MCP prefix would match every MCP client; use the launch-command prefix.
        client_config_dir: |p| &p.hermes_dir,
        // Shell hooks + MCP share config.yaml; allowlist is the consent sidecar.
        // Non-streaming: on_session_start greet-only; pre_llm_call notify+provide;
        // post_llm_call Stop; on_session_finalize SessionEnd. HERMES_HOME → Paths::resolve.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::HermesYamlHooks,
                config_file: |p| &p.hermes_config_yaml, // ~/.hermes/config.yaml
                load_hint: None,
                hook_streaming: false,
                hook_command_style: HookCommandStyle::InlineShell,
            },
            Surface {
                mechanism: WireMechanism::HermesYamlMcp,
                config_file: |p| &p.hermes_config_yaml, // same file
                load_hint: Some("start a new Hermes session to load the server"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
            Surface {
                mechanism: WireMechanism::HermesShellAllowlist,
                config_file: |p| &p.hermes_shell_hooks_allowlist,
                load_hint: None,
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray,
            },
        ],
        docs: &[
            DocRef {
                topic: "hooks",
                url: "https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks#shell-hooks",
            },
            DocRef {
                topic: "mcp",
                url: "https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp",
            },
        ],
        // Docs-derived pin (shell hooks + mcp contracts); live session pin pending.
        verified_client_version: "0.18.2",
        verified_on: "2026-07-19",
    },
];

/// Look up a wired client's configuration.
pub fn client_spec(target: WiredAgent) -> &'static ClientSpec {
    CLIENT_REGISTRY
        .iter()
        .find(|spec| spec.target == target)
        .expect("every WiredAgent must have a registry entry")
}

/// `dontspeak <client>` via its single canonical command (internal verbs excluded).
pub fn client_spec_for_launch(name: &str) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY
        .iter()
        .find(|spec| spec.target.as_str() == name)
}

/// MCP `clientInfo.name` normalize: trim, lowercase, `_` → `-`.
fn normalize_mcp_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// MCP name → [`WiredAgent`] via the same canonical client identity.
/// Matching is prefix-based because upstream names add suffixes such as `-code` or `-vscode`.
pub fn client_from_mcp_name(name: &str) -> Option<WiredAgent> {
    let n = normalize_mcp_name(name);
    if n.is_empty() {
        return None;
    }
    CLIENT_REGISTRY
        .iter()
        .find(|spec| n.starts_with(spec.target.as_str()))
        .map(|spec| spec.target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_client_stub(dir: &Path, client: WiredAgent) {
        let command = client.as_str();
        let filename = if cfg!(windows) {
            format!("{command}.exe")
        } else {
            command.to_string()
        };
        std::fs::write(dir.join(filename), b"fixture").unwrap();
    }

    /// A leftover dot-dir with no matching binary must not read as installed — the
    /// exact real-world case this presence check exists to reject.
    #[test]
    fn presence_is_the_binary_not_a_leftover_dot_dir() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(root.path());
        std::fs::create_dir_all(&paths.grok_dir).unwrap();

        assert!(!client_spec(WiredAgent::Grok).present(&paths));
    }

    #[test]
    fn presence_finds_the_binary_on_a_synthetic_path() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(root.path());
        let bin_dir = root.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        write_client_stub(&bin_dir, WiredAgent::Grok);
        paths.path_env = Some(std::env::join_paths([&bin_dir]).unwrap());

        assert!(client_spec(WiredAgent::Grok).present(&paths));
    }

    /// Fallback dirs are checked even with no `$PATH` at all (GUI-launched hosts).
    #[test]
    fn presence_falls_back_to_the_home_local_bin_dir() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(root.path());
        let bin_dir = root.path().join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        write_client_stub(&bin_dir, WiredAgent::Codex);

        assert!(client_spec(WiredAgent::Codex).present(&paths));
    }

    /// Same set/order as [`WiredAgent::ALL`].
    #[test]
    fn registry_matches_the_canonical_client_list() {
        let registry: Vec<WiredAgent> = CLIENT_REGISTRY.iter().map(|s| s.target).collect();
        assert_eq!(registry, WiredAgent::ALL);
    }

    /// Surfaces + docs + version pin + ISO date.
    #[test]
    fn every_client_has_surfaces_and_documentation() {
        for spec in CLIENT_REGISTRY {
            assert!(
                !spec.surfaces.is_empty(),
                "{}: a client with no surfaces wires nothing",
                spec.display_name
            );
            assert!(
                !spec.docs.is_empty(),
                "{}: every wiring must reference the official doc it was derived from",
                spec.display_name
            );
            for d in spec.docs {
                assert!(
                    d.url.starts_with("https://"),
                    "{}: doc ref {:?} is not a URL",
                    spec.display_name,
                    d.url
                );
            }
            assert!(
                !spec.verified_client_version.is_empty()
                    && spec
                        .verified_client_version
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit()),
                "{}: the version pin must name the client version the wiring was verified against",
                spec.display_name
            );
            assert!(
                spec.verified_on.len() == 10 && spec.verified_on.chars().nth(4) == Some('-'),
                "{}: verified_on must be an ISO date (YYYY-MM-DD), got {:?}",
                spec.display_name,
                spec.verified_on
            );
        }
    }

    #[test]
    fn lookup_covers_every_client() {
        for &t in WiredAgent::ALL {
            assert_eq!(client_spec(t).target, t);
        }
    }

    /// Every client has one unique canonical launcher command.
    #[test]
    fn launcher_commands_are_complete_unique_and_resolvable() {
        let mut names = std::collections::HashSet::new();
        for spec in CLIENT_REGISTRY {
            let name = spec.target.as_str();
            assert!(
                !name.is_empty(),
                "{}: empty launcher command",
                spec.display_name
            );
            assert!(
                names.insert(name),
                "launcher command {name:?} is declared for multiple clients"
            );
            assert_eq!(
                client_spec_for_launch(name).map(|found| found.target),
                Some(spec.target),
                "{name}"
            );
        }
        for internal in ["notify", "provide", "wire"] {
            assert!(client_spec_for_launch(internal).is_none(), "{internal}");
        }
    }

    /// Canonical identities are nonempty and already normalized for MCP prefix matching.
    #[test]
    fn canonical_client_identity_is_valid_for_mcp_prefix_matching() {
        for spec in CLIENT_REGISTRY {
            let identity = spec.target.as_str();
            assert!(
                !identity.is_empty(),
                "{}: a client with no clientInfo.name prefix can never be identified over MCP",
                spec.display_name
            );
            assert_eq!(
                normalize_mcp_name(identity),
                identity,
                "{}: prefix {:?} must be written in normalized form",
                spec.display_name,
                identity
            );
        }
    }

    #[test]
    fn known_mcp_names_map_to_their_client() {
        for (name, want) in [
            ("claude-code", WiredAgent::ClaudeCode),
            ("codex-mcp-client", WiredAgent::Codex),
            (WiredAgent::Codex.as_str(), WiredAgent::Codex),
            ("codex-vscode", WiredAgent::Codex),
            ("qwen-code", WiredAgent::QwenCode),
            ("qwen-cli-mcp-client-DontSpeak", WiredAgent::QwenCode),
            ("grok-shell-DontSpeak", WiredAgent::Grok),
            ("kimi-code", WiredAgent::KimiCode),
            (WiredAgent::Hermes.as_str(), WiredAgent::Hermes),
            ("hermes-agent-DontSpeak", WiredAgent::Hermes),
        ] {
            assert_eq!(client_from_mcp_name(name), Some(want), "{name}");
            assert_eq!(
                client_from_mcp_name(&name.to_ascii_uppercase().replace('-', "_")),
                Some(want),
                "{name} (case + underscore variant)"
            );
            assert_eq!(
                client_from_mcp_name(&format!("  {name}\n")),
                Some(want),
                "{name} (padded)"
            );
        }
    }

    /// Intentional prefix collision from canonical prefix matching.
    #[test]
    fn prefix_match_accepts_the_foreign_client_collision_tradeoff() {
        assert_eq!(
            client_from_mcp_name("codex-community-fork"),
            Some(WiredAgent::Codex)
        );
        assert_eq!(
            client_from_mcp_name("claude-code-fork"),
            Some(WiredAgent::ClaudeCode)
        );
    }

    #[test]
    fn unrecognised_mcp_names_are_not_guessed() {
        for name in ["gemini-cli-mcp-client", "", "   ", "🙂"] {
            assert_eq!(
                client_from_mcp_name(name),
                None,
                "{name:?} must not be attributed to a wired client"
            );
        }
    }
}
