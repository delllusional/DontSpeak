//! Client registry — declarative catalog of wireable AI clients.
//! Each [`ClientSpec`]: WHO ([`ClientSource`], kind, MCP name), WHERE (presence + config
//! files via [`Paths`]), HOW ([`WireMechanism`], [`LaunchSpec`], [`DocRef`]).
//! `wire` iterates surfaces and dispatches; add a client = CLIENTS + Paths + entry (no IO here).

use std::path::Path;

use crate::paths::Paths;
use ds_client::ClientSource;

/// Where the integration runs / config lives by convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Terminal CLI. Config under a `$HOME` dot-dir on every OS (`~/.claude`, …).
    TerminalCli,
    /// Desktop GUI. Config under per-OS app-support (resolved by [`Paths`]).
    DesktopApp,
}

/// How `dontspeak <client>` launches one supported client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Start normally after ensuring the resident host is running.
    Direct,
    /// Attach engine to app-server, then Codex TUI with `--remote`. Noninteractive passes through.
    CodexRemote,
}

/// Executable + public command names — lives beside wiring so launch surface cannot omit a client.
pub struct LaunchSpec {
    /// Preferred `dontspeak <name>` token and executable name.
    pub command: &'static str,
    /// Compatibility names (usually the canonical [`ClientSource`] token).
    pub aliases: &'static [&'static str],
    pub mode: LaunchMode,
}

/// HOW one integration surface is written. Every mechanism is additive + idempotent +
/// user-preserving; writers in `dontspeak`, pure shapers in sibling `wire::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireMechanism {
    /// Claude-contract voice hooks in JSON (`hooks.<Event>`; stdin JSON; `Stop` has
    /// `last_assistant_message`). Shaper: `merge_hooks`/`strip_hooks`.
    ClaudeJsonHooks,
    /// Same contract in format-preserving TOML (`[[hooks.<Event>]]`).
    /// Shaper: `merge_codex_hooks`/`strip_codex_hooks`.
    ClaudeTomlHooks,
    /// Dedicated JSON file we own outright (`~/.grok/hooks/dontspeak.json`). Claude shape with
    /// bare binary + seconds timeouts; matches Grok adapter after dropping `args` so native and
    /// imported registrations dedupe; runtime dispatches on `GROK_HOOK_EVENT`. Wire OVERWRITES
    /// (backup first), unwire DELETES. Shaper: `grok_hooks_value`.
    GrokJsonHooks,
    /// Flat `[[hooks]]` in `~/.kimi-code/config.toml`. Entry may carry ONLY
    /// `event`/`matcher`/`command`/`timeout` (extra keys break Kimi load) — needs its own shaper
    /// vs grouped [`WireMechanism::ClaudeTomlHooks`]: `merge_kimi_hooks`/`strip_kimi_hooks`.
    KimiTomlHooks,
    /// Stdio `mcpServers.DontSpeak` in JSON. Shaper: `merge_mcp_server`/`strip_mcp_server`.
    JsonMcp,
    /// Stdio `mcp_servers.DontSpeak` in TOML. Shaper: `merge_mcp_server_toml`/`strip_mcp_server_toml`.
    TomlMcp,
}

/// How the client's hook runner executes one wired command entry (`ClaudeJsonHooks` dialect).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookCommandStyle {
    /// Claude Code: spawn `command` + `args` array (no shell); `timeout` in SECONDS.
    ArgsArray,
    /// Qwen Code: only `command` string to a shell (no `args` field); verbs inlined;
    /// `timeout` in MILLISECONDS (default 60000).
    InlineShell,
}

/// One config file the wire edits, and how.
pub struct Surface {
    pub mechanism: WireMechanism,
    /// Resolved per-OS by [`Paths`].
    pub config_file: fn(&Paths) -> &Path,
    /// For [`WireMechanism::JsonMcp`]: how the user loads the new server (printed after wire).
    /// Hook surfaces take effect next turn — no hint.
    pub load_hint: Option<&'static str>,
    /// For [`WireMechanism::ClaudeJsonHooks`]: `true` ⇒ install `MessageDisplay`. Non-streaming
    /// clients omit it and voice the reply whole from `Stop`. Ignored by MCP and
    /// [`WireMechanism::ClaudeTomlHooks`] (Codex streaming-ness is baked into its shaper).
    pub hook_streaming: bool,
    /// For [`WireMechanism::ClaudeJsonHooks`] only; ignored by other mechanisms.
    pub hook_command_style: HookCommandStyle,
}

/// Official doc a wiring is derived from — contract changes checkable against upstream.
pub struct DocRef {
    /// `"hooks"` or `"mcp"`.
    pub topic: &'static str,
    pub url: &'static str,
}

/// One wireable client: WHO, WHERE, HOW, and the docs saying so.
pub struct ClientSpec {
    /// Canonical [`ClientSource`] token. Always a [`ClientSource::CLIENTS`] member —
    /// `DontSpeak`/`Unknown` have no entry (identities, not wire targets).
    pub target: ClientSource,
    pub display_name: &'static str,
    pub kind: ClientKind,
    pub launch: LaunchSpec,
    /// Prefix of MCP `initialize` `clientInfo.name` (after normalize: lowercase, `_`→`-`).
    /// Matched by [`client_from_mcp_name`] as `starts_with`. Covers variants without a
    /// hand-maintained alias list. TRADE-OFF: a foreign client sharing the prefix
    /// (e.g. `codex-community-fork`) is misattributed rather than `Unknown` — accepted to
    /// avoid per-version alias upkeep. No match → [`ClientSource::Unknown`].
    /// Every `initialize` logs the RAW name (`dontspeak::mcp`) for verify-wiring.
    pub mcp_client_prefix: &'static str,
    /// Real wire of a [`gate_on_presence`](Self::gate_on_presence) client skips when false.
    pub present: fn(&Paths) -> bool,
    /// Directory named in the "not detected" skip message.
    pub detect_dir: fn(&Paths) -> &Path,
    /// `false` only for Claude Code: installers wire unconditionally (hooks create `~/.claude`
    /// that then satisfies the MCP gate). Everything else gates.
    pub gate_on_presence: bool,
    pub surfaces: &'static [Surface],
    pub docs: &'static [DocRef],
    /// Version pin: client version when wiring was last verified against [`docs`](Self::docs).
    /// Not a compatibility floor — "implemented per docs as of this version".
    pub verified_client_version: &'static str,
    /// ISO date of that verification (`YYYY-MM-DD`).
    pub verified_on: &'static str,
}

/// Order matches [`ClientSource::CLIENTS`] (pinned by test).
pub const CLIENT_REGISTRY: &[ClientSpec] = &[
    ClientSpec {
        target: ClientSource::ClaudeCode,
        display_name: "Claude Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            command: "claude",
            aliases: &["claude_code"],
            mode: LaunchMode::Direct,
        },
        // Verified: announces as `claude-code` in `clientInfo.name`.
        mcp_client_prefix: "claude",
        present: |p| p.claude_dir.exists(),
        detect_dir: |p| &p.claude_dir,
        gate_on_presence: false,
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
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by JsonMcp
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
        target: ClientSource::Codex,
        display_name: "OpenAI Codex",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            command: "codex",
            aliases: &[],
            mode: LaunchMode::CodexRemote,
        },
        // `codex-mcp-client` is codex-rs's constant; prefix also covers CLI + VS Code.
        mcp_client_prefix: "codex",
        present: |p| p.codex_dir.exists(),
        detect_dir: |p| &p.codex_dir,
        gate_on_presence: true,
        // Claude hook contract in TOML. Events: SessionStart (greet-only), UserPromptSubmit
        // (notify + provide), Stop. No SessionEnd/Notification — cleanup via engine codex_stream.
        // Mid-turn narration: engine app-server subscriber (`codex --remote`), not hooks
        // (docs/STREAMING-NARRATION.md). MCP: `[mcp_servers.<name>]` in the SAME config.toml
        // (TomlMcp). Verified live 0.144.1: --remote still fires hooks; session id = thread id.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::ClaudeTomlHooks,
                config_file: |p| &p.codex_config, // ~/.codex/config.toml
                load_hint: None,
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by ClaudeTomlHooks
            },
            Surface {
                mechanism: WireMechanism::TomlMcp,
                config_file: |p| &p.codex_config, // same file
                load_hint: Some("start a new Codex session or run `codex mcp list` to verify"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by TomlMcp
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
        target: ClientSource::QwenCode,
        display_name: "Qwen Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            command: "qwen",
            aliases: &["qwen_code"],
            mode: LaunchMode::Direct,
        },
        // Live: `qwen-cli-mcp-client-DontSpeak` (and older `qwen-code*`) → prefix "qwen".
        mcp_client_prefix: "qwen",
        present: |p| p.qwen_dir.exists(),
        detect_dir: |p| &p.qwen_dir,
        gate_on_presence: true,
        // Claude hook contract via JSON writer, but runner is InlineShell (no `args`;
        // timeout ms). MessageDisplay with cumulative snake_case. Hooks + MCP share one
        // settings.json. Inline+streaming pinned by
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
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by JsonMcp
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
        target: ClientSource::Grok,
        display_name: "Grok",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            command: "grok",
            aliases: &[],
            mode: LaunchMode::Direct,
        },
        // Live: `grok-shell-DontSpeak` → prefix "grok".
        mcp_client_prefix: "grok",
        present: |p| p.grok_dir.exists(),
        detect_dir: |p| &p.grok_dir,
        gate_on_presence: true,
        // MCP: TomlMcp in ~/.grok/config.toml. Hooks: own file under ~/.grok/hooks/*.json.
        // Five lifecycle hooks; Grok dedupes imported Claude entry with identical bare command;
        // GROK_HOOK_EVENT distinguishes hook launch from no-arg MCP. Stop is metadata-only →
        // chat_history fallback. Mid-turn = engine updates.jsonl tail (not MessageDisplay) →
        // hook_streaming false, SessionStart greet-only. Digests also → AGENTS.md (issue #95).
        surfaces: &[
            Surface {
                mechanism: WireMechanism::GrokJsonHooks,
                config_file: |p| &p.grok_hooks_json, // ~/.grok/hooks/dontspeak.json
                load_hint: None,
                hook_streaming: false, // mid-turn = engine updates.jsonl tail
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by GrokJsonHooks
            },
            Surface {
                mechanism: WireMechanism::TomlMcp,
                config_file: |p| &p.grok_config,
                load_hint: Some("start a new Grok session or run `grok mcp list` / `grok inspect`"),
                hook_streaming: false,
                hook_command_style: HookCommandStyle::ArgsArray, // ignored for MCP
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
        target: ClientSource::KimiCode,
        display_name: "Kimi Code",
        kind: ClientKind::TerminalCli,
        launch: LaunchSpec {
            command: "kimi",
            aliases: &["kimi-code"],
            mode: LaunchMode::Direct,
        },
        mcp_client_prefix: "kimi",
        present: |p| p.kimi_dir.exists(),
        detect_dir: |p| &p.kimi_dir,
        gate_on_presence: true,
        // Flat [[hooks]] — only event/matcher/command/timeout; own KimiTomlHooks shaper.
        // Inline shell, seconds timeouts, no matcher. Non-streaming: greet-only SessionStart;
        // has SessionEnd + Notification (unlike Codex). MCP: separate mcp.json (JsonMcp).
        // KIMI_CODE_HOME overrides ~/.kimi-code (see Paths::resolve).
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
                hook_command_style: HookCommandStyle::ArgsArray, // ignored by JsonMcp
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
];

/// `None` for DontSpeak/Unknown (by design — not wireable).
pub fn client_spec(target: ClientSource) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY.iter().find(|s| s.target == target)
}

/// Resolve `dontspeak <client>` via preferred command + aliases. Internal verbs not registry names.
pub fn client_spec_for_launch(name: &str) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY
        .iter()
        .find(|spec| spec.launch.command == name || spec.launch.aliases.contains(&name))
}

/// Normalize MCP `clientInfo.name` for matching: trim, lowercase, `_` → `-`.
fn normalize_mcp_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// Map MCP `clientInfo.name` → [`ClientSource`] via registry prefixes (see
/// [`ClientSpec::mcp_client_prefix`]). No match → [`ClientSource::Unknown`].
pub fn client_from_mcp_name(name: &str) -> ClientSource {
    let n = normalize_mcp_name(name);
    if n.is_empty() {
        return ClientSource::Unknown;
    }
    CLIENT_REGISTRY
        .iter()
        .find(|s| n.starts_with(s.mcp_client_prefix))
        .map_or(ClientSource::Unknown, |s| s.target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Registry IS CLIENTS with wiring facts — same set, same order.
    #[test]
    fn registry_matches_the_canonical_client_list() {
        let registry: Vec<ClientSource> = CLIENT_REGISTRY.iter().map(|s| s.target).collect();
        assert_eq!(registry, ClientSource::CLIENTS);
    }

    /// Surfaces nonempty; ≥1 doc ref; version pin + ISO date present.
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

    /// DontSpeak/Unknown have no entry — guard `ds_wire::run` relies on for `wire dontspeak`.
    #[test]
    fn lookup_covers_every_client() {
        for &t in ClientSource::CLIENTS {
            assert!(
                client_spec(t).is_some(),
                "{} missing from registry",
                t.as_str()
            );
        }
        assert!(client_spec(ClientSource::DontSpeak).is_none());
        assert!(client_spec(ClientSource::Unknown).is_none());
    }

    /// Launcher names nonempty, unique, resolve back to declaring client.
    #[test]
    fn launcher_names_are_complete_unique_and_resolvable() {
        let mut names = std::collections::HashSet::new();
        for spec in CLIENT_REGISTRY {
            assert!(!spec.launch.command.is_empty(), "{}", spec.display_name);
            for name in
                std::iter::once(spec.launch.command).chain(spec.launch.aliases.iter().copied())
            {
                assert!(
                    !name.is_empty(),
                    "{}: empty launcher name",
                    spec.display_name
                );
                assert!(
                    names.insert(name),
                    "launcher name {name:?} is declared more than once"
                );
                assert_eq!(
                    client_spec_for_launch(name).map(|found| found.target),
                    Some(spec.target),
                    "{name}"
                );
            }
        }
        for internal in ["notify", "provide", "wire"] {
            assert!(client_spec_for_launch(internal).is_none(), "{internal}");
        }
    }

    /// Prefixes nonempty and already normalized (else match silently fails).
    #[test]
    fn mcp_client_prefix_is_present_and_already_normalized() {
        for spec in CLIENT_REGISTRY {
            assert!(
                !spec.mcp_client_prefix.is_empty(),
                "{}: a client with no clientInfo.name prefix can never be identified over MCP",
                spec.display_name
            );
            assert_eq!(
                normalize_mcp_name(spec.mcp_client_prefix),
                spec.mcp_client_prefix,
                "{}: prefix {:?} must be written in normalized form",
                spec.display_name,
                spec.mcp_client_prefix
            );
        }
    }

    #[test]
    fn known_mcp_names_map_to_their_client() {
        for (name, want) in [
            ("claude-code", ClientSource::ClaudeCode),
            ("codex-mcp-client", ClientSource::Codex),
            ("codex", ClientSource::Codex),
            ("codex-vscode", ClientSource::Codex),
            ("qwen-code", ClientSource::QwenCode),
            ("qwen-cli-mcp-client-DontSpeak", ClientSource::QwenCode),
            ("grok-shell-DontSpeak", ClientSource::Grok),
            ("kimi-code", ClientSource::KimiCode),
        ] {
            assert_eq!(client_from_mcp_name(name), want, "{name}");
            assert_eq!(
                client_from_mcp_name(&name.to_ascii_uppercase().replace('-', "_")),
                want,
                "{name} (case + underscore variant)"
            );
            assert_eq!(
                client_from_mcp_name(&format!("  {name}\n")),
                want,
                "{name} (padded)"
            );
        }
    }

    /// Prefix collision is intentional (see [`ClientSpec::mcp_client_prefix`]).
    #[test]
    fn prefix_match_accepts_the_foreign_client_collision_tradeoff() {
        assert_eq!(
            client_from_mcp_name("codex-community-fork"),
            ClientSource::Codex
        );
        assert_eq!(
            client_from_mcp_name("claude-code-fork"),
            ClientSource::ClaudeCode
        );
    }

    #[test]
    fn unrecognised_mcp_names_are_unknown_not_guessed() {
        for name in ["gemini-cli-mcp-client", "", "   ", "🙂"] {
            assert_eq!(
                client_from_mcp_name(name),
                ClientSource::Unknown,
                "{name:?} must not be attributed to a wired client"
            );
        }
    }
}
