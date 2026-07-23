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

Each client has exactly one canonical name; there are no launcher aliases. `WiredAgent` owns
that name for launch, hooks, IPC, logs, usage, and MCP attribution. Launchers preserve cwd, streams, args, exit
code; start the DontSpeak host first (except `--help` / `--version`), and inject a fresh
`DONTSPEAK_SESSION_ID` inherited by that client's hooks and local MCP server. Windows:
new terminal after first install for PATH.

### MCP queue identity

Speech queue identity is not inferred from a variable merely because an agent exposes it
to shell tools. The variable must be available when the local stdio MCP child starts and
must also agree with hook events. This was audited per supported client in July 2026:

| Client | Upstream session-ID behavior | Queue-ID decision |
|---|---|---|
| Claude Code | `CLAUDE_CODE_SESSION_ID`; hooks update on `/clear`, but an existing MCP child retains its spawn ID | Excluded to prevent drift |
| Qwen Code | `QWEN_CODE_SESSION_ID` is process-global and changes for a new session | Excluded because an existing MCP child's environment is a snapshot |
| OpenAI Codex | `CODEX_THREAD_ID` is tool/shell-only, not local MCP startup | Unavailable |
| Grok | Dynamic `{{session_id}}` is documented for HTTP headers, not local stdio | Unavailable |
| Kimi Code | Stdio `env` is static configuration | Unavailable |
| Hermes Agent | `HERMES_SESSION_ID` is documented for tool subprocesses, not MCP startup | No supported MCP-startup contract |

Every client therefore uses launcher/terminal identity, then an isolated MCP-process
fallback. Native conversation IDs are never treated as queue IDs.

References: [Claude environment contract](https://code.claude.com/docs/en/env-vars),
[Codex local stdio gap](https://github.com/openai/codex/issues/19937),
[Qwen session context source](https://github.com/QwenLM/qwen-code/blob/v0.19.11/packages/core/src/utils/sessionIdContext.ts),
[Grok MCP configuration](https://docs.x.ai/build/features/mcp-servers),
[Kimi MCP configuration](https://www.kimi.com/code/docs/en/kimi-code-cli/customization/mcp.html),
[Hermes environment variables](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/reference/environment-variables.md),
and [Hermes MCP configuration](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp).

MCP `speak` and `stop` require a non-empty queue identity on the internal wire. The
generated last-resort identity therefore preserves isolation instead of silently turning
`stop` into a global operation. Logical conversation IDs remain separate for Codex/Grok
stream discovery and per-agent narration state.

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

Hooks for stream + lifecycle. `--bare` skips hooks/MCP — forwarded as-is.

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
`LaunchMode::Direct`; mid-turn is engine file-tail (`hook_streaming` false).

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
`hookSpecificOutput`). `HERMES_HOME` overrides the client dir.

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

## Usage statistics (Agents tab)

Desktop **Agents** tab (nav order: Agents → Status → Tools → Log → Credits;
cold-start default while enabled). Opt-in via config `agents` (off by default —
usage probing may touch the macOS keychain): the tab appears/disappears live via
the `model_status` push (root `agents` field; initial / engine-down probe is
`ds_agents_ui_enabled`, fail-closed 0). While off, the default nav page is
Status. Usage data itself is independent of speech state and does not extend
`model_status`. MCP prefers an `AgentUsage` request to the app-hosted engine, then
falls back to the same local collector when the engine is unreachable. This keeps
engine-down usage working while making macOS keychain reads use the host app's code
identity. Domain crate `ds-agent-usage`; hosts call `ds-core` FFI only.

### Model

`UsageDeck` → `cards[]` of `UsageCard` (`agent` wire token + optional `account` +
`needs_auth` only when true + `rows[]`); each `UsageRow` has `period`
(`session` | `week` | `month`), `used_percent` (0…100), `resets_at_unix`.

Card order is `WiredAgent::ALL`. Probe only agents with
`ClientSpec::present`. At most one row per period; missing periods omitted, never
shown as `0%`. Require percent + reset timestamp to emit a row. Never serialize
credentials, provider bodies, or raw error strings.

Optional **account** (email) is display-only from local credentials:

| Agent | Account source |
| --- | --- |
| Claude Code | `~/.claude.json` → `oauthAccount.emailAddress` |
| Codex | `~/.codex/auth.json` JWT `id_token` → `email` |
| Grok | `~/.grok/auth.json` session → `email` |
| Qwen / Kimi / Hermes | none |

Transparent by default; click toggles opacity for this UI session only (not
persisted).

### ABI (hosts call off the UI thread)

| Symbol | Behavior |
| --- | --- |
| `ds_agent_usage_skeleton_json()` | Installed agents + last-good cache; **no network** |
| `ds_agent_usage_card_json(agent, force)` | Blocking single-card load; never prompts |
| `ds_agent_usage_card_authorize_json(agent)` | User-click authorize + force load; may ACL-prompt on macOS |
| `ds_usage_resets_in(unix)` | Remaining duration (`2d 05h`, no seconds, no “Resets in” prefix) |
| `ds_random_pastel_wash_json()` | One pastel wash `{"r","g","b","a"}` (HSV: random H, S=0.42, V=0.92, α=0.30) |

Mirrors: `ds-core` FFI, `dontspeak.h`, `Native.cs`, GTK `ffi.rs`. Hosts decode ABI
JSON in the data-source adapter; views receive typed cards only. Generation
counters drop stale completions after leave-tab.

### Tab select flow

1. Skeleton paints only cards that already have rows (cache hits). Empty shells
   are not shown; first open with no cache stays blank (not “loading…”).
2. Force-load each installed agent async; transition a card only when its typed
   value changes. Identical / failed / empty probes do not remount or wipe a
   prior good value.
3. After all loads finish with no data, show `usage.unavailable`.

No Refresh button, no loading spinner. Re-select repeats the flow. Soft TTL for
non-force tooling refresh is **60s**; the tab always force-loads after skeleton.

### Cache

One typed in-memory cache keyed by `WiredAgent`, atomically mirrored to
`agent-usage-cache.json` under the OS cache directory (normalized rows, optional
account labels, fetch timestamps — never secrets or provider bodies). Empty
probes never overwrite a good entry. Concurrent refreshes for one agent share a
probe. The file **never** stores `needs_auth: true`, so skeleton never paints
authorize.

### Layout (all hosts)

Agent title (left) + optional account (top-right) → for each row: period label +
remaining duration (top-right), progress bar (percent as bar only). Strings from
`ds-i18n` (`usage.*` / `usage.provider.<token>`). No plan names, costs, balances,
charts, or raw provider errors.

### Speaking highlight

While TTS plays, `model_status.activity.speaker` is the wired agent of the
in-flight utterance (`claude` / `codex` / `qwen` / `grok` / `kimi` /
`hermes`; `null` when idle or unattributed). Hosts wash that agent’s card with a
random pastel from `ds_random_pastel_wash_json` (new color when speaker becomes
non-null or changes; frozen until idle). Top-bar speaking stripe and progress
bars stay brand purple.

Pipe: enqueue keeps `source: Option<WiredAgent>` on each TTS item → worker claim
publishes `PlayingClaim { source, session }` → `tts_running()` exposes
`(tts_active, Option<source>)` → host matches `speaker == card.agent`. Not
inferred from session id. Earcons under focus hold do not claim playback.

### Provider matrix

| Agent | Source | Session | Week | Month |
| --- | --- | --- | --- | --- |
| Claude Code | OAuth `GET …/api/oauth/usage` + macOS Keychain or `~/.claude/.credentials.json` | API `five_hour` → `session` | API `seven_day` → `week` | — |
| Codex | short-lived `codex app-server` → `account/rateLimits/read` | 300 min or session label | 10080 min or weekly label | explicit monthly label only |
| Qwen Code | Alibaba Coding Plan HTTP + env/settings API keys | `per5Hour*` | `perWeek*` | `perBillMonth*` / `perMonth*` |
| Grok | try `x.ai/billing` via `grok agent stdio`; else gRPC-web `GetGrokCreditsConfig` + Bearer from `~/.grok/auth.json` | — | web: full cycle ~4–12 days | CLI monthly-named; web else → month |
| Kimi Code | `GET https://api.kimi.com/coding/v1/usages` + Bearer from `~/.kimi-code/credentials/kimi-code.json` | `limits[]` 5h window | top-level `usage` weekly | — |
| Hermes Agent | stub (no public quota API yet) | — | — | — |

Claude: accept fractional `resets_at`; `utilization` is percent 0…100 (same scale
as `limits[].percent` — do not treat `1.0` as a full fraction); on HTTP
failure/empty keep last good card. Windows resolves CLI binaries via `.exe`/`.cmd`
(never extensionless npm shebangs). On macOS/Linux, Codex and Grok CLI probes
search `CODEX_CLI_PATH` / `GROK_CLI_PATH`, login-shell PATH, process PATH, then
known install roots. Qwen Coding Plan keys: process env, `$QWEN_HOME/.env`,
`~/.env`, then the `env` object in Qwen settings.

### Security

- Credential reads are read-only, size-bounded, documented paths only.
- Implicit reads (tab paint, MCP `usage`, skeleton/card FFI) never prompt. Sole
  prompter: `ds_agent_usage_card_authorize_json` (click-only). While config
  `agents` is off, `ds-core` additionally guards every usage export per call:
  skeleton returns an empty deck and card/authorize return empty cards before
  any provider (keychain) work. On macOS, clients
  that keep credentials in the keychain (Claude Code is the only one so far)
  probe silently with keychain UI disallowed
  (`SecKeychainSetUserInteractionAllowed`); a guarded item fails with
  `errSecInteractionNotAllowed` instead of prompting, and only that guarded
  state produces `needs_auth: true` (stale rows kept).
- “Always Allow” persists via OS keychain ACL (no config flag — Claude may
  recreate the item). MCP routes usage through the running app so that ACL grant
  covers both the Agents tab and MCP; its engine-down fallback remains silent and
  may report `needs_auth` until the app runs. Production HTTPS only for live probes;
  tests use fixtures / loopback. Secrets and raw payloads never appear in FFI JSON
  or logs.
