# Claude Code voice hooks

Inline command entries in `~/.claude/settings.json` run the `dontspeak` binary with
stdin hook JSON. No wrapper scripts. Two subcommands by **contract**, not by event:

- `dontspeak notify` — fire-and-forget command sink; no stdout reply; routes on
  `hook_event_name`.
- `dontspeak provide` — query; client waits for `hookSpecificOutput` on stdout.

Every verb includes `--client <token>` (`claude_code` | `codex` | `qwen_code` |
`grok`) so IPC/logs know the source (`client=<token>`). Missing/unknown → `unknown`,
never fails the hook. MCP identity comes from `initialize` `clientInfo.name`.

## Wiring

| Event | Verb | Role |
|-------|------|------|
| `MessageDisplay` | `notify` | Streaming narration: top-level blockquotes as they complete |
| `SessionStart` | `notify` | Greet + claim agent voice (`greet_on_open`) |
| `SessionEnd` | `notify` | Session-scoped `StopSpeech` |
| `UserPromptSubmit` | `notify` | `MarkActive` — this terminal is foreground for narration |
| `UserPromptSubmit` | `provide` | Sync: inject narrate spec as `additionalContext` when on |
| `Stop` | `notify` | Reply-done earcon; non-streaming clients also voice final reply |
| `Notification` | `notify` | Needs-input earcon (`permission_prompt` / `idle_prompt` only) |

`notify` is async (except Codex: no `async` flag — Codex skips `async: true` hooks,
so Codex runs sync with tight timeouts). `provide` is always sync.

Claude final reply is not special-cased: streamed via `MessageDisplay`; `Stop`
witness suppresses re-speech, still queues the earcon. Narration gated by `narrate`
(`shorts`/`digests`) and `tts_engine != off`. Earcons independent of `narrate`
(empty sound = off; honor mute). Reply ding defaults to OS chime; needs-input off.

Other clients reuse this executor with different events/formats — see
[CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md).

Hooks talk to the warm engine over `dontspeak.sock`. Engine down → best-effort no-op
(never block the client).

## Setup

`./scripts/install/local/install.sh` builds, installs to `~/.local/bin` (or
`DONTSPEAK_INSTALL_DIR`), reconciles clients. No launchd/systemd — engine is in-process
in the host app. Logs: macOS `~/Library/Logs/DontSpeak/dontspeak.log`; other OSes
state-dir `logs/dontspeak.log`.
