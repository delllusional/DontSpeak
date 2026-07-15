# Client integrations and launchers

DontSpeak supports Claude Code, OpenAI Codex, Qwen Code, and Grok as distinct
integrations. Installation reconciles each detected client's hooks and MCP registration;
the `dontspeak <client>` commands then start the installed client without replacing its
normal configuration or arguments.

This page is the source of truth for client capabilities and launch quirks. Hook internals
live in [the hook executor document](../claude/hooks/HOOKS-README.md), and the shared
narration state machine lives in [the streaming narration document](STREAMING-NARRATION.md).

## Launch commands

```sh
dontspeak claude [claude arguments...]
dontspeak codex [codex arguments...]
dontspeak qwen [qwen arguments...]
dontspeak grok [grok arguments...]
```

`claude_code` and `qwen_code` remain accepted aliases so launcher names and wiring tokens
compose cleanly. The client registry owns these names, executable names, and launch modes;
adding a supported client without a launcher fails its registry tests.

The direct launchers preserve the current directory, inherited terminal streams, child
arguments, and child exit code. They first make sure the resident DontSpeak host is running,
except for information-only flags such as `--help` and `--version`. On Windows, the installer
adds the install directory to the per-user `PATH`; use a new terminal after first install.

## Capability matrix

| Client | Automatic mid-turn digest | End-of-turn reply fallback | MCP tools | Launcher |
|---|---|---|---|---|
| Claude Code | Yes, through `MessageDisplay` | Not needed; `Stop` is the reply earcon | Yes | Direct `claude` |
| OpenAI Codex | Yes for interactive sessions started by `dontspeak codex` | Yes for a plain local TUI | Yes | Engine-managed app-server plus `codex --remote` |
| Qwen Code 0.19.10 | Yes, through `MessageDisplay` | No; the session witness suppresses duplicate `Stop` narration | Yes | Direct `qwen` |
| Grok 0.2.101 | No message stream | Yes, from the final assistant entry in `Stop.transcriptPath` | Yes | Direct `grok` |

The launcher surface is uniform, but only clients exposing a message stream can narrate
mid-turn. Grok uses an end-of-turn fallback instead.

## Client-specific behavior

### Claude Code

`dontspeak claude` starts the normal Claude Code process. Claude's `MessageDisplay` hook
delivers streaming assistant text, while lifecycle hooks select the active terminal, greet,
clean up the session, and play earcons.

Claude's `--bare` mode deliberately skips hooks and MCP/customization. The launcher forwards
that flag unchanged, so it also disables the DontSpeak integration for that invocation.

### OpenAI Codex

Codex has no `MessageDisplay` hook. For the base interactive TUI, `resume`, and `fork`, the
launcher performs this ordered handshake:

1. start the resident DontSpeak host if necessary;
2. ask the engine to make its Codex app-server subscriber ready;
3. let the engine attach to an existing server or start the supported local server path;
4. receive the initialized endpoint and start `codex --remote <endpoint>`.

The engine owns a direct Windows listener in a kill-on-close Job Object and reuses it for
later launches. On macOS and Linux it uses Codex's managed Unix control socket and starts the
idempotent daemon command on demand. No DontSpeak preference is silently changed, and the TUI
is not launched until the narration observer has initialized against the same server.

Codex commands that do not run an interactive TUI (`exec`, `review`, `mcp`, `app-server`,
login, diagnostics, and other management commands) pass through directly because upstream
does not support the TUI remote path for them. A caller-supplied `--remote` is rejected by
the wrapper: use `codex` directly for a custom remote endpoint, or set DontSpeak's
`codex_app_server_url` to a loopback `ws://` endpoint that its subscriber can observe.

### Qwen Code

The executable is `qwen`, not `qwen-code`. Version 0.19.10 uses Claude-compatible
hooks, but its hook runner accepts one inline shell command and millisecond timeouts rather
than Claude's command-plus-arguments shape and second timeouts. The registry therefore emits
a Qwen-specific execution shape while reusing the same hook handlers.

Qwen 0.19.10 ships a cumulative `MessageDisplay` payload (`displayed_text` and `is_final`).
DontSpeak wires it for mid-turn narration. `Stop` remains wired for completion handling, but
the session witness suppresses duplicate reply narration.

Qwen's `--safe-mode` and `--bare` modes disable hooks, MCP servers, and customization. The
launcher forwards them unchanged and therefore cannot provide automatic integration in those
modes.

### Grok

Grok reads native hooks from its own hook directory and also imports Claude-compatible hooks.
DontSpeak owns one dedicated native hook file whose bare command matches the target Grok
derives from the imported Claude entry. Grok then deduplicates the two instead of running the
same side effect twice; unrelated Claude-compatible hooks remain enabled.

Grok's hook payload is camelCase and its `Stop` event contains completion metadata plus a
`transcriptPath`, but no assistant reply field. DontSpeak reads a bounded tail of that JSONL
transcript, selects the last non-empty assistant entry, and feeds it through the same
end-of-turn narration core as Qwen Code. This provides final-reply narration, not mid-turn
streaming; an absent, unreadable, or unexpected transcript stays silent and still permits the
reply-done earcon.

## Wiring and reconciliation

The installer runs `dontspeak wire --reconcile`. The resident engine repeats reconciliation
at boot, using `config.toml`'s `exclude_clients` as the desired set. Every merge is additive,
idempotent, backed up before write, and scoped to DontSpeak's own entries.

| Client | Hook configuration | MCP configuration |
|---|---|---|
| Claude Code | `~/.claude/settings.json` | `~/.claude.json` |
| OpenAI Codex | `~/.codex/config.toml` | Same TOML file |
| Qwen Code | `~/.qwen/settings.json` | Same JSON file |
| Grok | `~/.grok/hooks/dontspeak.json` | `~/.grok/config.toml` |

Useful diagnostics:

```sh
dontspeak wire --list
dontspeak wire --all --print-only
dontspeak wire <client> --print-only
```

## Verified upstream contracts

Current registry pins and official contract sources:

| Client | Verified version | Official contracts |
|---|---:|---|
| Claude Code | 2.1.210 | [hooks](https://code.claude.com/docs/en/hooks), [MCP](https://code.claude.com/docs/en/mcp) |
| OpenAI Codex | 0.144.4 | [hooks](https://developers.openai.com/codex/hooks), [app server](https://developers.openai.com/codex/app-server), [MCP](https://developers.openai.com/codex/mcp) |
| Qwen Code | 0.19.10 | [release hooks](https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/hooks.md), [MCP](https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/mcp.md) |
| Grok | 0.2.101 | [CLI](https://docs.x.ai/build/cli/reference), [hooks](https://docs.x.ai/build/features/hooks), [MCP](https://docs.x.ai/build/features/mcp-servers) |

The version pin records when a contract was checked; it is not a minimum-version claim.
