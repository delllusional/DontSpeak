# Client integrations and launchers

Supported: Claude Code, OpenAI Codex, Qwen Code, Grok, Kimi Code, Hermes Agent.
Install reconciles hooks + MCP; `dontspeak <client>` starts the installed client without
replacing its config/args.

Hook internals: [HOOKS.md](HOOKS.md). Streaming state machine:
[STREAMING-NARRATION.md](STREAMING-NARRATION.md). Adding a new client:
[ADDING-A-CLIENT.md](ADDING-A-CLIENT.md).

## Launch

```sh
dontspeak claude [args...]
dontspeak codex [args...]
dontspeak qwen [args...]
dontspeak grok [args...]
dontspeak kimi [args...]
dontspeak hermes [args...]
```

Aliases: `claude_code`, `qwen_code`, `kimi-code`. Registry owns names/executables/modes; adding a
client without a launcher fails tests. Launchers preserve cwd, streams, args, exit
code; start the DontSpeak host first (except `--help` / `--version`). Windows: new
terminal after first install for PATH.

## Capability matrix

| Client | Mid-turn digest | End-of-turn fallback | MCP | Launcher |
|---|---|---|---|---|
| Claude Code | Yes (`MessageDisplay`) | N/A (`Stop` = earcon) | Yes | Direct `claude` |
| OpenAI Codex | Yes for an app-server remote TUI | Yes for plain local TUI | Yes | Engine app-server + `codex --remote` |
| Qwen Code 0.19.10 | Yes (`MessageDisplay`) | Witness suppresses duplicate `Stop` speech | Yes | Direct `qwen` |
| Grok 0.2.101 | Yes (engine tails `updates.jsonl`) | Yes from `Stop.transcriptPath` when no witness | Yes | Direct `grok` |
| Kimi Code 0.27.0 | No | Yes from `Stop` wire.jsonl fallback | Yes | Direct `kimi` |
| Hermes Agent 0.18.2 | No | Yes from `post_llm_call` → `extra.assistant_response` | Yes | Direct `hermes` |

## Client notes

### Claude Code

Normal process; hooks for stream + lifecycle. `--bare` skips hooks/MCP — forwarded as-is.

### OpenAI Codex

No `MessageDisplay`. Interactive TUI / resume / fork handshake:

1. Start host if needed  
2. Ready Codex subscriber  
3. Attach or start local app-server  
4. `codex --remote <endpoint>`

Windows: engine owns a kill-on-close Job listener. Unix: the default control socket
uses Codex's managed daemon for standalone installs and an engine-owned ordinary
app-server for Homebrew/npm installs. An already-running external app-server is reused
and never adopted or stopped. Non-TUI commands (`exec`, `review`, `mcp`, …) pass
through. Caller `--remote` is rejected — use bare `codex` or set
`codex_app_server_url` to a loopback `ws://` endpoint.

`dontspeak codex` prepares the shared app-server, waits until the narration subscriber
has attached, then adds `--remote`. If preparation fails, hooks still provide
end-of-turn narration but cannot expose mid-turn deltas. See
[Streaming narration — Launches](STREAMING-NARRATION.md#launches) for lifecycle details.

### Qwen Code

Binary `qwen`. 0.19.10: Claude-compatible hooks but one inline shell command + ms
timeouts (registry emits Qwen shape). Cumulative `MessageDisplay`
(`displayed_text`, `is_final`). `--safe-mode` / `--bare` disable integration.

### Grok

Native hook file + Claude-compatible import; Grok dedupes matching bare commands.
`LaunchMode::Direct`; `hook_streaming` stays false (mid-turn is engine file-tail, not
`MessageDisplay`).

**Mid-turn:** engine `dontspeakd::grok_stream` tails
`~/.grok/sessions/<encoded-cwd>/<sessionId>/updates.jsonl` for ACP
`agent_message_chunk` text (config `grok_stream`, default on). Session discovery via
Grok `GreetSession` / `MarkActive` IPC. Witness on attach; `Stop` finalizes trailing
digests and does not re-voice `chat_history` when streamed.

**End-of-turn fallback:** `Stop` has `transcriptPath` (no assistant field) — when no
witness, read last non-empty assistant JSONL (`chat_history`, remapping bare
`updates.jsonl`). Silent on bad transcript; earcon still allowed.

**Digest instruction (issue #95):** Grok ignores `UserPromptSubmit` stdout, so
`additionalContext` never reaches the model. DontSpeak still emits it, plus:

1. Marker-bounded narrate section in `~/.grok/AGENTS.md` (wire/unwire/hooks)
2. Same text as MCP `initialize.instructions` when digests on

New Grok session required after first wire or digests toggle.

### Hermes Agent

Binary `hermes`. Non-streaming shell hooks in `~/.hermes/config.yaml` (`hooks:` block)
plus `mcp_servers.DontSpeak` in the same file. First-use consent pairs are pre-approved
in `~/.hermes/shell-hooks-allowlist.json`. Event remap before TitleCase: `on_session_start`
→ SessionStart, `pre_llm_call` → UserPromptSubmit, `post_llm_call` → Stop,
`on_session_finalize` → SessionEnd. Provide shape is flat `{"context":…}` (not Claude
`hookSpecificOutput`). `HERMES_HOME` overrides the client dir. No Hermes STT/TTS
providers — engine audio only.

## Wiring

`dontspeak wire --reconcile` at install; engine re-reconciles at boot via
`exclude_clients`. Additive, idempotent, backup-before-write, DontSpeak entries only.
Client-specific homes are honored through `CLAUDE_CONFIG_DIR`, `CODEX_HOME`,
`QWEN_HOME`, `GROK_HOME`, `KIMI_CODE_HOME`, and `HERMES_HOME`; relative values resolve
from the launch directory and `~/...` values resolve from the user home.

| Client | Hooks | MCP |
|---|---|---|
| Claude Code | `~/.claude/settings.json` | `~/.claude.json` |
| OpenAI Codex | `~/.codex/config.toml` | same |
| Qwen Code | `~/.qwen/settings.json` | same |
| Grok | `~/.grok/hooks/dontspeak.json` (+ `~/.grok/AGENTS.md` narrate) | `~/.grok/config.toml` |
| Kimi Code | `~/.kimi-code/config.toml` (flat `[[hooks]]`) | `~/.kimi-code/mcp.json` |
| Hermes Agent | `~/.hermes/config.yaml` (`hooks:`) + allowlist JSON | same config.yaml (`mcp_servers`) |

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
| Kimi Code | 0.27.0 | [hooks](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/hooks.html), [MCP](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/mcp.html) |
| Hermes Agent | 0.18.2 | [shell hooks](https://hermes-agent.nousresearch.com/docs/user-guide/features/hooks#shell-hooks), [MCP](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp) |

Pins record last check, not minimum version.

## Usage statistics

Full spec: [AGENT-USAGE-PLAN.md](AGENT-USAGE-PLAN.md). Summary:

**Model:** `UsageDeck` → `cards[]` of `UsageCard` (`agent` + `rows[]` +
`needs_auth`, present only when true: macOS keychain ACL); each `UsageRow` has
`period` (`session` | `week` | `month`),
`used_percent`, `resets_at_unix`.

**ABI (off UI thread):**

1. `ds_agent_usage_skeleton_json()` — installed agents + last-good memory/disk cache; no network
2. `ds_agent_usage_card_json(agent, force)` — blocking load for one agent; never prompts
3. `ds_agent_usage_card_authorize_json(agent)` — user-click authorize + force load; may ACL-prompt on macOS
4. `ds_usage_resets_in(unix)` — remaining duration string (`2d 05h`, no seconds, no prefix)

**Tab select:** paint only cards that already have rows (cache); force-load each
installed agent async; transition a card only when its typed value changes. First
visit with no cache shows no shells until a load succeeds. No Refresh button /
loading spinner. Empty probes never wipe the atomically persisted last-good cache,
and overlapping refreshes for one agent share a probe. Hosts decode ABI JSON in
their lowest-level data-source adapter; views receive typed cards only. Install gate
= wire `ClientSpec::present`.

On macOS/Linux, Codex and Grok CLI probes search an explicit `CODEX_CLI_PATH` /
`GROK_CLI_PATH`, the login-shell PATH, the process PATH, then known install roots.
Qwen Coding Plan keys are read in client order from the process environment,
`$QWEN_HOME/.env`, `~/.env`, then the `env` object in Qwen settings.

**Layout (all hosts):** agent title → for each row: period + remaining (top-right),
progress bar (percent as bar only). Strings from `ds-i18n` (`usage.*`).

**Speaking highlight:** while TTS plays, `model_status.activity.speaker` is the
wireable client token of the in-flight utterance (`claude_code` / `codex` /
`qwen_code` / `grok` / `kimi_code` / `hermes`; `null` when idle or non-client). Hosts wash
that agent’s Usage card with a random pastel from `ds_random_pastel_wash_json` (top-bar
speaking stripe stays brand purple). Source is retained on each TTS queue item at enqueue
(hooks `source`, stream adapters, `GreetSession`) — not inferred from session id.
