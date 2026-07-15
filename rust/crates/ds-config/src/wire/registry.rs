//! The CLIENT REGISTRY — the ONE declarative catalog of every AI client DontSpeak wires
//! into. Each [`ClientSpec`] separates the three questions the wiring used to answer
//! implicitly, per client, in scattered `match` arms:
//!
//!   * WHO — the client ([`ClientSource`] token, display name, [`ClientKind`]: terminal CLI
//!     vs desktop app, plus the `clientInfo.name` aliases it announces itself with over MCP —
//!     [`ClientSpec::mcp_client_names`], the other half of the client-identity story the
//!     hooks' `--client` token covers);
//!   * WHERE — the platform surface it's installed on: the presence probe
//!     ([`ClientSpec::present`]/[`ClientSpec::detect_dir`]) and each config file the wire
//!     edits ([`Surface::config_file`]), both resolved per-OS by [`Paths`];
//!   * HOW — the [`WireMechanism`] each surface is written with (Claude-contract JSON
//!     hooks, Claude-contract TOML hooks, or an MCP entry), the [`LaunchSpec`] exposed by
//!     `dontspeak <client>`, plus the OFFICIAL documentation ([`DocRef`]) the wiring
//!     contracts were derived from.
//!
//! The `dontspeak wire` orchestrator iterates a spec's surfaces and dispatches on the
//! mechanism — so adding a client (e.g. Qwen Code, whose hooks reuse Claude Code's wire
//! protocol, or Gemini CLI) is ONE new `ClientSource::CLIENTS` member + `Paths` fields + a
//! registry entry, not a new code path. The pure merge/strip shapers stay in the sibling
//! modules; this file holds no IO.

use std::path::Path;

use crate::paths::Paths;
use ds_client::ClientSource;

/// What KIND of application a client is — i.e. where the integration runs and, by
/// convention, where its config lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// A terminal CLI agent. Config lives under a dot-dir in `$HOME` on every OS
    /// (`~/.claude`, `~/.codex`, …), so the paths are platform-uniform.
    TerminalCli,
    /// A desktop GUI app. Config lives under the per-OS application-support dir
    /// (macOS `~/Library/Application Support`, Windows `%APPDATA%`, Linux `~/.config`),
    /// so the path is platform-resolved by [`Paths`].
    DesktopApp,
}

/// How `dontspeak <client>` launches one supported client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    /// Start the client normally after making sure the resident DontSpeak host is running.
    Direct,
    /// Ask the engine to attach to an app-server, then start Codex's interactive TUI with
    /// the returned `--remote` endpoint. Noninteractive Codex commands still pass through.
    CodexRemote,
}

/// The executable and public command names for one client launcher. This lives beside the
/// wiring facts so the command surface cannot silently omit a supported integration.
pub struct LaunchSpec {
    /// Preferred `dontspeak <name>` token and executable name.
    pub command: &'static str,
    /// Accepted compatibility names (normally the canonical [`ClientSource`] token).
    pub aliases: &'static [&'static str],
    pub mode: LaunchMode,
}

/// HOW one integration surface is written into a client's config file. Every mechanism is
/// additive + idempotent + user-preserving; the writers live in the `dontspeak` crate, the
/// pure shapers in this crate's sibling `wire::*` modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireMechanism {
    /// DontSpeak's voice hooks merged into a JSON settings file using Claude Code's hook
    /// contract (`hooks.<Event>` groups; JSON on stdin; `Stop` carries
    /// `last_assistant_message`). Shaper: `merge_hooks`/`strip_hooks`.
    ClaudeJsonHooks,
    /// The SAME Claude-contract hooks, but in a TOML config edited format-preservingly
    /// (`toml_edit`) — `[[hooks.<Event>]]` tables. Shaper:
    /// `merge_codex_hooks`/`strip_codex_hooks`.
    ClaudeTomlHooks,
    /// DontSpeak's voice hooks written to a DEDICATED JSON file we own outright (Grok's
    /// `~/.grok/hooks/dontspeak.json`). The Claude hooks-contract shape, but with one BARE
    /// binary command per event and seconds timeouts. The target matches what Grok's Claude
    /// adapter produces after dropping `args`, so Grok deduplicates native and imported
    /// registrations; the runtime dispatches from the reserved `GROK_HOOK_EVENT` marker.
    /// Because the file is exclusively ours, there is nothing to merge: wire OVERWRITES it
    /// (a backup is taken first) and unwire DELETES it. Shaper: `grok_hooks_value`.
    GrokJsonHooks,
    /// The stdio `mcpServers.DontSpeak` entry merged into a JSON config. Shaper:
    /// `merge_mcp_server`/`strip_mcp_server`.
    JsonMcp,
    /// The stdio `mcp_servers.DontSpeak` entry merged into a TOML config (Grok style).
    /// Shaper: `merge_mcp_server_toml`/`strip_mcp_server_toml`.
    TomlMcp,
}

/// HOW the client's hook runner EXECUTES one wired command entry — the dialect the
/// `ClaudeJsonHooks` shaper must emit. Two clients share the JSON hook contract but run
/// the commands completely differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookCommandStyle {
    /// Claude Code: spawns `command` + the `args` array directly (no shell);
    /// `timeout` is in SECONDS.
    ArgsArray,
    /// Qwen Code: passes ONLY the `command` string to a shell (`executeCommandHook` spawns
    /// `shellConfig.executable` with `[...argsPrefix, hookConfig.command]`); its
    /// `CommandHookConfig` has NO `args` field, so the verbs must be inlined into the
    /// command string; `timeout` is in MILLISECONDS (default 60000).
    InlineShell,
}

/// ONE config file the wire edits for a client, and how.
pub struct Surface {
    pub mechanism: WireMechanism,
    /// The config file this surface edits, resolved per-OS by [`Paths`].
    pub config_file: fn(&Paths) -> &Path,
    /// For [`WireMechanism::JsonMcp`]: how the user loads the newly registered server
    /// (printed after a successful wire). Hook surfaces take effect on the next turn, so
    /// they carry no hint.
    pub load_hint: Option<&'static str>,
    /// For [`WireMechanism::ClaudeJsonHooks`]: whether the client streams assistant messages
    /// via `MessageDisplay` (Claude Code and Qwen Code → `true`). Non-streaming clients
    /// omit the hook and voice the reply whole from `Stop`. Ignored by
    /// [`WireMechanism::JsonMcp`] and [`WireMechanism::ClaudeTomlHooks`] (Codex's streaming-ness
    /// is baked into its own TOML shaper's fixed hook set).
    pub hook_streaming: bool,
    /// For [`WireMechanism::ClaudeJsonHooks`]: how the client's hook runner executes a wired
    /// command entry (see [`HookCommandStyle`]). Ignored by the other mechanisms — same
    /// convention as [`hook_streaming`](Self::hook_streaming).
    pub hook_command_style: HookCommandStyle,
}

/// A pointer to the OFFICIAL documentation a wiring is derived from — so every registry
/// entry names its sources and a contract change is checkable against the upstream doc
/// rather than against folklore.
pub struct DocRef {
    /// What the document specifies: `"hooks"` or `"mcp"`.
    pub topic: &'static str,
    pub url: &'static str,
}

/// One wireable client: WHO it is, WHERE it lives, HOW it's wired, and the docs saying so.
pub struct ClientSpec {
    /// The canonical [`ClientSource`] token (`claude_code` / `codex`). Always a
    /// [`ClientSource::CLIENTS`] member — `DontSpeak`/`Unknown` have no registry entry, by
    /// design (they are identities, not things we wire).
    pub target: ClientSource,
    /// Human-facing name for messages ("Claude Code", "OpenAI Codex", …).
    pub display_name: &'static str,
    pub kind: ClientKind,
    /// How the installed client is started through `dontspeak <client>`.
    pub launch: LaunchSpec,
    /// The `clientInfo.name` values this client announces itself with in the MCP `initialize`
    /// handshake — the MCP half of the client-identity story (the hooks' half is the
    /// `--client <token>` verb the wiring stamps). Matched EXACTLY (after normalizing case /
    /// `_`→`-`) by [`client_from_mcp_name`]; a prefix rule would mis-attribute a foreign
    /// client, so there isn't one. Anything unmatched is [`ClientSource::Unknown`] — the
    /// honest answer, not a fallback to a guess.
    ///
    /// Every `initialize` logs the RAW `clientInfo.name` it saw (see `dontspeak::mcp`), which
    /// is how an UNVERIFIED alias below gets confirmed or corrected from the field — a
    /// one-line edit here, then a `verified_on` bump via the `verify-wiring` skill.
    pub mcp_client_names: &'static [&'static str],
    /// Is the client installed? A REAL wire (not `--remove`, not `--print-only`) of a
    /// [`gate_on_presence`](Self::gate_on_presence) client is skipped when this is false,
    /// so we never scatter a stray config on a machine without the client.
    pub present: fn(&Paths) -> bool,
    /// The directory whose existence [`present`](Self::present) probes — named in the
    /// "not detected (…)" skip message.
    pub detect_dir: fn(&Paths) -> &Path,
    /// `false` only for Claude Code: the installers wire it unconditionally (our hooks
    /// write CREATES `~/.claude`, which then satisfies the MCP surface's gate) — it is
    /// DontSpeak's primary client. Everything else gates.
    pub gate_on_presence: bool,
    pub surfaces: &'static [Surface],
    /// The official docs this entry's mechanisms and paths were derived from.
    pub docs: &'static [DocRef],
    /// The VERSION PIN: the client version current when this wiring was last verified —
    /// i.e. the [`docs`](Self::docs) were (re-)read and the merge shape confirmed against
    /// them. NOT a compatibility floor (the contracts are stable across versions until
    /// proven otherwise); it says "implemented per the docs as of this client version".
    /// Update it whenever a wiring is re-checked or changed.
    pub verified_client_version: &'static str,
    /// ISO date of that verification (`YYYY-MM-DD`).
    pub verified_on: &'static str,
}

/// The registry. Order matches [`ClientSource::CLIENTS`] (pinned by test): this is the SAME
/// canonical client list, with the wiring facts attached.
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
        // VERIFIED: Claude Code announces itself as `claude-code` in `initialize`'s
        // `clientInfo.name`.
        mcp_client_names: &["claude-code"],
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
        // `codex-mcp-client` is the constant codex-rs sets in its MCP connection manager;
        // the other two cover the plain CLI and the VS Code surface.
        mcp_client_names: &["codex-mcp-client", "codex", "codex-vscode"],
        present: |p| p.codex_dir.exists(),
        detect_dir: |p| &p.codex_dir,
        gate_on_presence: true,
        // Codex adopted Claude Code's hook contract (same events, same stdin JSON,
        // `Stop.last_assistant_message`), so the SAME dontspeak binary serves it; only
        // the file format (TOML) differs. Its hook set is three events — `SessionStart`
        // (greet-only), `UserPromptSubmit` (ONE group, two inner hooks: `notify` for
        // mark-active routing + the engine's codex_stream session re-discovery, and the
        // synchronous `provide` for the narration spec), `Stop`; `SessionStart` landed in
        // Codex CLI 0.142.x, it didn't exist when Codex was first wired. Codex's hook
        // event list has NO `SessionEnd` and no `Notification` (confirmed against the
        // hooks doc, 2026-07-07) — per-session cleanup for Codex is owned by the
        // engine's codex_stream supervisor, not a hook. MID-TURN narration doesn't ride
        // hooks at all: the engine subscribes to the shared app-server (`codex
        // app-server daemon start` + `codex --remote`) — see the "app-server" DocRef and
        // docs/STREAMING-NARRATION.md. Verified live with Codex 0.144.1 on 2026-07-12:
        // `--remote` sessions still fire SessionStart, UserPromptSubmit, and Stop, and
        // the hook session id equals the app-server thread id used by the subscriber.
        //
        // Codex ALSO registers external MCP servers via `[mcp_servers.<name>]` in the
        // SAME `~/.codex/config.toml` (confirmed against the mcp doc + `codex mcp
        // list`/`add`/`remove` on the locally installed 0.142.5 binary, 2026-07-10) —
        // the identical stdio table shape (`command`/`args`/`env`/`startup_timeout_sec`/
        // `tool_timeout_sec`) Grok uses, so it reuses the SAME `TomlMcp` mechanism and
        // shaper, just pointed at Codex's own config file. Hooks and MCP share one file,
        // same pattern as Qwen Code sharing `~/.qwen/settings.json` between its
        // `ClaudeJsonHooks` and `JsonMcp` surfaces.
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
        // UNVERIFIED — Qwen Code's own `clientInfo.name` is not published (the reachable docs
        // describe MCP *server* names). It is a gemini-cli fork, so it may still send a
        // gemini-derived name. The `mcp initialize clientInfo.name=…` capture line is what
        // settles this from the field; correct this list from what it reports.
        mcp_client_names: &["qwen-code", "qwen-code-mcp-client"],
        present: |p| p.qwen_dir.exists(),
        detect_dir: |p| &p.qwen_dir,
        gate_on_presence: true,
        // Qwen Code reuses Claude Code's hook contract (same events, same stdin JSON,
        // `Stop.last_assistant_message`, `UserPromptSubmit` honors `additionalContext`), so the
        // SAME dontspeak binary serves it via the JSON writer — but its RUNNER differs: it
        // passes ONLY the `command` string to a shell (no `args` field exists in its
        // `CommandHookConfig`, `timeout` is milliseconds), so the surface is
        // `HookCommandStyle::InlineShell` — verbs inlined into the command string, timeouts
        // scaled. Version 0.19.10 ships `MessageDisplay` with a cumulative snake_case payload
        // (`displayed_text` + `is_final`), so the streaming hook is enabled. Hooks + MCP both
        // live in the ONE `~/.qwen/settings.json`, so the two surfaces share a config_file.
        // The InlineShell + streaming combination (inlined notify command, ms-scaled timeout,
        // and plain-notify SessionStart witness seed) is pinned by
        // `inline_streaming_wires_messagedisplay_with_ms_timeout_and_plain_sessionstart`
        // in wire/hooks.rs.
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
        // Live verified (2026-07-13): Grok sends clientInfo.name="grok-shell-DontSpeak",
        // normalized to "grok-shell-dontspeak". Keep the older aliases for version skew.
        mcp_client_names: &["grok", "grok-cli", "grok-shell-dontspeak"],
        present: |p| p.grok_dir.exists(),
        detect_dir: |p| &p.grok_dir,
        gate_on_presence: true,
        // Grok (Grok Build) uses TOML for MCP servers under `[mcp_servers.<name>]` in
        // `~/.grok/config.toml` (and project `.grok/config.toml`), so the MCP surface reuses
        // the same `TomlMcp` mechanism/shaper Codex uses, pointed at Grok's config file.
        //
        // Grok reads native hooks from `~/.grok/hooks/*.json`; DontSpeak owns and replaces its
        // dedicated file. The five lifecycle hooks provide greeting, session routing, and
        // earcons independently of Claude compatibility. Grok deduplicates an imported Claude
        // entry with the identical bare command, and `GROK_HOOK_EVENT` distinguishes that hook
        // launch from DontSpeak's no-argument MCP mode.
        //
        // Live 0.2.93 captures on 2026-07-13 showed camelCase data keys and lowercase-snake
        // `hookEventName` values, which the runtime normalizes mechanically. Stop is
        // metadata-only and carries no final assistant text, so it signals completion but
        // cannot drive full-reply narration. The MCP handshake identified itself as
        // `grok-shell-DontSpeak`, normalized to the alias above.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::GrokJsonHooks,
                config_file: |p| &p.grok_hooks_json, // ~/.grok/hooks/dontspeak.json
                load_hint: None,
                hook_streaming: false,
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
];

/// Look up the registry entry for a client token. Returns `Some` for every
/// [`ClientSource::CLIENTS`] member — so callers that iterate `CLIENTS` can `expect` a hit —
/// and `None` for `ClientSource::DontSpeak` / `ClientSource::Unknown`, BY DESIGN: neither is
/// a client we wire anything into, and that `None` is what makes `dontspeak wire dontspeak`
/// a clean "unknown client" error instead of a panic (see `ds_wire::run`'s parse arm).
pub fn client_spec(target: ClientSource) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY.iter().find(|s| s.target == target)
}

/// Resolve a public `dontspeak <client>` token through the registry's preferred command
/// names and aliases. Internal verbs (`notify`, `provide`, `wire`) are not registry names.
pub fn client_spec_for_launch(name: &str) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY
        .iter()
        .find(|spec| spec.launch.command == name || spec.launch.aliases.contains(&name))
}

/// Normalize an MCP `clientInfo.name` for alias matching: trim, lowercase, `_` → `-`. Both
/// sides of the comparison go through this, so the registry's aliases are written in the
/// normalized form and a client sending `Claude_Code` still matches `claude-code`.
fn normalize_mcp_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// Which client is this MCP caller? Maps the `initialize` handshake's `clientInfo.name`
/// (free-form, per the MCP lifecycle spec) onto a [`ClientSource`] via the registry's
/// [`ClientSpec::mcp_client_names`] alias lists.
///
/// EXACT match after normalization — deliberately NOT a prefix/substring rule, which would
/// happily attribute `codex-community-fork` (or any foreign client with a colliding prefix)
/// to a client we wire. An unrecognised name is [`ClientSource::Unknown`]: the honest answer,
/// and the thing the MCP server's raw-name capture line exists to turn into a new alias.
pub fn client_from_mcp_name(name: &str) -> ClientSource {
    let n = normalize_mcp_name(name);
    if n.is_empty() {
        return ClientSource::Unknown;
    }
    CLIENT_REGISTRY
        .iter()
        .find(|s| s.mcp_client_names.contains(&n.as_str()))
        .map_or(ClientSource::Unknown, |s| s.target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry IS `ClientSource::CLIENTS` with wiring facts attached — same set, same
    /// order. A client added to one place but not the other fails here, not in the field.
    #[test]
    fn registry_matches_the_canonical_client_list() {
        let registry: Vec<ClientSource> = CLIENT_REGISTRY.iter().map(|s| s.target).collect();
        assert_eq!(registry, ClientSource::CLIENTS);
    }

    /// Every entry is fully specified: at least one surface, and at least one official
    /// doc reference per DISTINCT mechanism it uses — the registry's contract is that a
    /// wiring names its sources.
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

    /// `client_spec` resolves every WIRE-ABLE client — and only those. `DontSpeak` and
    /// `Unknown` are `ClientSource` members but NOT clients, so they have no registry entry
    /// by design; that `None` is the guard `ds_wire::run` relies on to reject
    /// `dontspeak wire dontspeak` cleanly.
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

    /// The launcher is another registry consumer: every preferred name and compatibility
    /// alias must be nonempty, unique, and resolve back to the declaring client.
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

    /// The alias table's own hygiene: every entry declares at least one `clientInfo.name`,
    /// and each is already in the NORMALIZED form `client_from_mcp_name` compares against
    /// (lowercase, `-` not `_`, trimmed) — an alias written `Qwen_Code` would silently never
    /// match, since only the incoming name is normalized.
    #[test]
    fn mcp_client_names_are_present_and_already_normalized() {
        for spec in CLIENT_REGISTRY {
            assert!(
                !spec.mcp_client_names.is_empty(),
                "{}: a client with no clientInfo.name alias can never be identified over MCP",
                spec.display_name
            );
            for alias in spec.mcp_client_names {
                assert!(!alias.is_empty(), "{}: empty alias", spec.display_name);
                assert_eq!(
                    &normalize_mcp_name(alias),
                    alias,
                    "{}: alias {alias:?} must be written in normalized form",
                    spec.display_name
                );
            }
        }
    }

    /// Every declared alias maps back to its own client, case/underscore-tolerantly.
    #[test]
    fn every_declared_alias_maps_to_its_client() {
        for spec in CLIENT_REGISTRY {
            for alias in spec.mcp_client_names {
                assert_eq!(client_from_mcp_name(alias), spec.target, "{alias}");
                assert_eq!(
                    client_from_mcp_name(&alias.to_ascii_uppercase().replace('-', "_")),
                    spec.target,
                    "{alias} (case + underscore variant)"
                );
                assert_eq!(
                    client_from_mcp_name(&format!("  {alias}\n")),
                    spec.target,
                    "{alias} (padded)"
                );
            }
        }
        assert_eq!(
            client_from_mcp_name("claude-code"),
            ClientSource::ClaudeCode
        );
        assert_eq!(
            client_from_mcp_name("codex-mcp-client"),
            ClientSource::Codex
        );
        assert_eq!(
            client_from_mcp_name("grok-shell-DontSpeak"),
            ClientSource::Grok
        );
    }

    /// An unrecognised / absent / near-miss name is `Unknown` — never guessed onto a client.
    /// The near-miss cases are the reason matching is EXACT: a prefix rule would attribute
    /// `codex-community-fork` (and `gemini-cli-mcp-client`, which shares nothing but shape)
    /// to a client we wire, and every one of that client's tool calls would be mis-logged.
    #[test]
    fn unrecognised_mcp_names_are_unknown_not_guessed() {
        for name in [
            "gemini-cli-mcp-client",
            "codex-community-fork",
            "claude-code-fork",
            "",
            "   ",
            "🙂",
        ] {
            assert_eq!(
                client_from_mcp_name(name),
                ClientSource::Unknown,
                "{name:?} must not be attributed to a wired client"
            );
        }
    }
}
