//! The CLIENT REGISTRY — the ONE declarative catalog of every AI client DontSpeak wires
//! into. Each [`ClientSpec`] separates the three questions the wiring used to answer
//! implicitly, per client, in scattered `match` arms:
//!
//!   * WHO — the client ([`WireTarget`] token, display name, [`ClientKind`]: terminal CLI
//!     vs desktop app);
//!   * WHERE — the platform surface it's installed on: the presence probe
//!     ([`ClientSpec::present`]/[`ClientSpec::detect_dir`]) and each config file the wire
//!     edits ([`Surface::config_file`]), both resolved per-OS by [`Paths`];
//!   * HOW — the [`WireMechanism`] each surface is written with (Claude-contract JSON
//!     hooks, Claude-contract TOML hooks, or a JSON `mcpServers` entry), plus the
//!     OFFICIAL documentation ([`DocRef`]) the contract was derived from.
//!
//! The `dontspeak wire` orchestrator iterates a spec's surfaces and dispatches on the
//! mechanism — so adding a client (e.g. Qwen Code, whose hooks reuse Claude Code's wire
//! protocol, or Gemini CLI) is ONE new `WireTarget` variant + `Paths` fields + a registry
//! entry, not a new code path. The pure merge/strip shapers stay in the sibling modules;
//! this file holds no IO.

use std::path::Path;

use crate::enums::WireTarget;
use crate::paths::Paths;

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
    /// `~/.grok/hooks/dontspeak.json`). The Claude hooks-contract shape, but with the verb
    /// INLINED into a single `command` string (no `args` array) — rendered by the shared
    /// `wire::cmdline`, so it is quoted on POSIX and QUOTE-FREE on Windows (an embedded `"`
    /// cannot survive cmd.exe; see that module) — plus seconds timeouts, no `async` key, and
    /// camelCase (`hookEventName`) payloads handled by the runtime serde aliases.
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
    /// via `MessageDisplay` (Claude Code → `true`). Non-streaming clients (Qwen Code → `false`)
    /// omit the `MessageDisplay` hook — the reply is voiced whole from `Stop`. Ignored by
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
    /// The canonical [`WireTarget`] token (`claude_code` / `codex`).
    pub target: WireTarget,
    /// Human-facing name for messages ("Claude Code", "OpenAI Codex", …).
    pub display_name: &'static str,
    pub kind: ClientKind,
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

/// The registry. Order matches [`WireTarget::CLIENTS`] (pinned by test): this is the SAME
/// canonical client list, with the wiring facts attached.
pub const CLIENT_REGISTRY: &[ClientSpec] = &[
    ClientSpec {
        target: WireTarget::ClaudeCode,
        display_name: "Claude Code",
        kind: ClientKind::TerminalCli,
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
        verified_client_version: "2.1.198",
        verified_on: "2026-07-02",
    },
    ClientSpec {
        target: WireTarget::Codex,
        display_name: "OpenAI Codex",
        kind: ClientKind::TerminalCli,
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
        // docs/STREAMING-NARRATION.md. OPEN FINDING for the live-capture pass (§9
        // there): whether hooks still fire for `--remote`-hosted sessions is
        // undocumented — if they do NOT, streaming narration still works and Stop
        // simply never fires, but greet/`provide` would be lost for those sessions.
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
        verified_client_version: "0.142.5",
        verified_on: "2026-07-10",
    },
    ClientSpec {
        target: WireTarget::QwenCode,
        display_name: "Qwen Code",
        kind: ClientKind::TerminalCli,
        present: |p| p.qwen_dir.exists(),
        detect_dir: |p| &p.qwen_dir,
        gate_on_presence: true,
        // Qwen Code reuses Claude Code's hook contract (same events, same stdin JSON,
        // `Stop.last_assistant_message`, `UserPromptSubmit` honors `additionalContext`), so the
        // SAME dontspeak binary serves it via the JSON writer — but its RUNNER differs: it
        // passes ONLY the `command` string to a shell (no `args` field exists in its
        // `CommandHookConfig`, `timeout` is milliseconds), so the surface is
        // `HookCommandStyle::InlineShell` — verbs inlined into the command string, timeouts
        // scaled. It has NO `MessageDisplay` stream TODAY, so `hook_streaming: false` and the
        // reply is voiced whole from `Stop` (the non-streaming path the binary already serves
        // plain-TUI Codex through). Hooks + MCP both live in the ONE `~/.qwen/settings.json`,
        // so the two surfaces share a config_file.
        //
        // FUTURE FLIP (QwenLM/qwen-code#6488): when Qwen ships its MessageDisplay hook
        // (snake_case cumulative payload — `displayed_text` + `is_final`, already accepted
        // by the handler's serde aliases), the WHOLE change is `hook_streaming: true` here
        // + a version-pin bump via the `verify-wiring` skill — no core or handler edits.
        // The wiring side of that combination (InlineShell + streaming: MessageDisplay
        // group with the inlined notify command, ms-scaled timeout, plain-notify
        // SessionStart witness seed) is pinned NOW by
        // `inline_streaming_wires_messagedisplay_with_ms_timeout_and_plain_sessionstart`
        // in wire/hooks.rs.
        surfaces: &[
            Surface {
                mechanism: WireMechanism::ClaudeJsonHooks,
                config_file: |p| &p.qwen_settings, // ~/.qwen/settings.json
                load_hint: None,
                hook_streaming: false,
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
                url: "https://github.com/QwenLM/qwen-code/blob/main/docs/users/features/hooks.md",
            },
            DocRef {
                topic: "mcp",
                url: "https://github.com/QwenLM/qwen-code/blob/main/docs/users/features/mcp.md",
            },
        ],
        verified_client_version: "0.19.7",
        verified_on: "2026-07-07",
    },
    ClientSpec {
        target: WireTarget::Grok,
        display_name: "Grok",
        kind: ClientKind::TerminalCli,
        present: |p| p.grok_dir.exists(),
        detect_dir: |p| &p.grok_dir,
        gate_on_presence: true,
        // Grok (Grok Build) uses TOML for MCP servers under `[mcp_servers.<name>]` in
        // `~/.grok/config.toml` (and project `.grok/config.toml`), so the MCP surface reuses
        // the same `TomlMcp` mechanism/shaper Codex uses, pointed at Grok's config file.
        //
        // Grok ALSO reads native hook definitions from `~/.grok/hooks/*.json` (or project
        // `.grok/hooks/*.json`) using a Claude-COMPATIBLE event contract. So DontSpeak wires
        // its OWN dedicated `~/.grok/hooks/dontspeak.json` — a file it owns outright (wire
        // overwrites, backing up first; unwire deletes it) — via the `GrokJsonHooks`
        // mechanism. That makes Grok narration FIRST-CLASS and native: it no longer depends on
        // `wire claude_code` compat being present. The hook set is the full non-streaming
        // shape (five events): `SessionStart` (greet-only), `SessionEnd`, `UserPromptSubmit`
        // (notify + provide), `Stop`, `Notification`. Grok has NO `MessageDisplay` streaming
        // hook, so end-of-turn narration rides `Stop` (like Codex, voicing the whole final
        // message). Grok's hook payloads are camelCase (`hookEventName`, `sessionId`,
        // `lastAssistantMessage`, `notificationType`) — handled by the runtime serde aliases
        // on the hook payload structs, so the same `dontspeak notify`/`provide` binary serves
        // them. Hooks live in a SEPARATE file from MCP, so the two surfaces edit different
        // files (unlike Codex/Qwen, which share one).
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
        verified_client_version: "0.2.93",
        verified_on: "2026-07-10",
    },
];

/// Look up the registry entry for a client token. Returns `Some` for every
/// [`WireTarget`] variant (they're all clients), so callers that iterate
/// [`WireTarget::CLIENTS`] can `expect` a hit.
pub fn client_spec(target: WireTarget) -> Option<&'static ClientSpec> {
    CLIENT_REGISTRY.iter().find(|s| s.target == target)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry IS `WireTarget::CLIENTS` with wiring facts attached — same set, same
    /// order. A client added to one place but not the other fails here, not in the field.
    #[test]
    fn registry_matches_the_canonical_client_list() {
        let registry: Vec<WireTarget> = CLIENT_REGISTRY.iter().map(|s| s.target).collect();
        assert_eq!(registry, WireTarget::CLIENTS);
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

    /// `client_spec` resolves every client (`WireTarget` is client-only now).
    #[test]
    fn lookup_covers_every_client() {
        for &t in WireTarget::CLIENTS {
            assert!(
                client_spec(t).is_some(),
                "{} missing from registry",
                t.as_str()
            );
        }
    }
}
