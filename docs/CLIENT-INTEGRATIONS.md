# Client integrations and launchers

Supported: Claude Code, OpenAI Codex, Qwen Code, Grok. Install reconciles hooks + MCP;
`dontspeak <client>` starts the installed client without replacing its config/args.

Hook internals: [HOOKS.md](HOOKS.md). Streaming state machine:
[STREAMING-NARRATION.md](STREAMING-NARRATION.md).

## Launch

```sh
dontspeak claude [args...]
dontspeak codex [args...]
dontspeak qwen [args...]
dontspeak grok [args...]
```

Aliases: `claude_code`, `qwen_code`. Registry owns names/executables/modes; adding a
client without a launcher fails tests. Launchers preserve cwd, streams, args, exit
code; start the DontSpeak host first (except `--help` / `--version`). Windows: new
terminal after first install for PATH.

## Capability matrix

| Client | Mid-turn digest | End-of-turn fallback | MCP | Launcher |
|---|---|---|---|---|
| Claude Code | Yes (`MessageDisplay`) | N/A (`Stop` = earcon) | Yes | Direct `claude` |
| OpenAI Codex | Yes for an app-server remote TUI | Yes for plain local TUI | Yes | Engine app-server + `codex --remote` |
| Qwen Code 0.19.10 | Yes (`MessageDisplay`) | Witness suppresses duplicate `Stop` speech | Yes | Direct `qwen` |
| Grok 0.2.101 | No stream | Yes from `Stop.transcriptPath` | Yes | Direct `grok` |

## Client notes

### Claude Code

Normal process; hooks for stream + lifecycle. `--bare` skips hooks/MCP — forwarded as-is.

### OpenAI Codex

No `MessageDisplay`. Interactive TUI / resume / fork handshake:

1. Start host if needed  
2. Ready Codex subscriber  
3. Attach or start local app-server  
4. `codex --remote <endpoint>`

Windows: engine owns kill-on-close Job listener. Unix: managed control socket. Non-TUI
commands (`exec`, `review`, `mcp`, …) pass through. Caller `--remote` rejected —
use bare `codex` or set `codex_app_server_url` loopback `ws://`.

`dontspeak codex` normally prepares the shared app-server and adds `--remote`. On
Unix it starts a missing server with `codex app-server daemon start`, which Codex
0.144.5 supports only for its standalone installation. Homebrew installations need
an externally managed `codex app-server --listen unix://` plus a direct
`codex --remote unix://` launch. See
[Streaming narration — Launches](STREAMING-NARRATION.md#launches) for the macOS
LaunchAgent setup. Without a remote app-server, hooks still provide end-of-turn
narration but cannot expose mid-turn deltas.

### Qwen Code

Binary `qwen`. 0.19.10: Claude-compatible hooks but one inline shell command + ms
timeouts (registry emits Qwen shape). Cumulative `MessageDisplay`
(`displayed_text`, `is_final`). `--safe-mode` / `--bare` disable integration.

### Grok

Native hook file + Claude-compatible import; Grok dedupes matching bare commands.
`Stop` has `transcriptPath` (no assistant field) — read last non-empty assistant JSONL
entry for final-reply narration only. Silent on bad transcript; earcon still allowed.

**Digest instruction (issue #95):** Grok ignores `UserPromptSubmit` stdout, so
`additionalContext` never reaches the model. DontSpeak still emits it, plus:

1. Marker-bounded narrate section in `~/.grok/AGENTS.md` (wire/unwire/hooks)
2. Same text as MCP `initialize.instructions` when digests on

New Grok session required after first wire or digests toggle.

## Wiring

`dontspeak wire --reconcile` at install; engine re-reconciles at boot via
`exclude_clients`. Additive, idempotent, backup-before-write, DontSpeak entries only.

| Client | Hooks | MCP |
|---|---|---|
| Claude Code | `~/.claude/settings.json` | `~/.claude.json` |
| OpenAI Codex | `~/.codex/config.toml` | same |
| Qwen Code | `~/.qwen/settings.json` | same |
| Grok | `~/.grok/hooks/dontspeak.json` (+ `~/.grok/AGENTS.md` narrate) | `~/.grok/config.toml` |

```sh
dontspeak wire --list
dontspeak wire --all --print-only
dontspeak wire <client> --print-only
```

## Verified upstream

| Client | Verified | Contracts |
|---|---:|---|
| Claude Code | 2.1.210 | [hooks](https://code.claude.com/docs/en/hooks), [MCP](https://code.claude.com/docs/en/mcp) |
| OpenAI Codex | 0.144.4 | [hooks](https://developers.openai.com/codex/hooks), [app server](https://developers.openai.com/codex/app-server), [MCP](https://developers.openai.com/codex/mcp) |
| Qwen Code | 0.19.10 | [hooks](https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/hooks.md), [MCP](https://github.com/QwenLM/qwen-code/blob/v0.19.10/docs/users/features/mcp.md) |
| Grok | 0.2.101 | [CLI](https://docs.x.ai/build/cli/reference), [hooks](https://docs.x.ai/build/features/hooks), [MCP](https://docs.x.ai/build/features/mcp-servers) |

Pins record last check, not minimum version.
