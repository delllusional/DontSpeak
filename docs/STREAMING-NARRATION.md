# Streaming (mid-turn) narration

Speaks top-level `>` digests (plus shorts fallback) while the reply streams. Core:
`ds-narrate`. Adapters:

| Client | Transport | Payload | Adapter |
|---|---|---|---|
| Claude Code | `MessageDisplay` (per-batch process) | incremental `delta` by block `index` | `dontspeak::hook_narrate` |
| Qwen Code 0.19.10+ | `MessageDisplay` | cumulative `displayed_text` + `is_final` | `dontspeak::hook_narrate` |
| OpenAI Codex | engine app-server subscriber | `item/agentMessage/delta` + `item/completed` | `dontspeakd::codex_stream` |
| Grok | engine file-tail of `updates.jsonl` | ACP `session/update` → `agent_message_chunk` | `dontspeakd::grok_stream` |

All build `StreamBatch` → `ds_narrate::deliver_batch`.

Each admitted utterance carries spoken `text` plus `detection_text` (cumulative
message-so-far at selection) and `message_key` (`batch.key` / pending key). Adapters
forward both on `SpeakNarration` / `enqueue_narration` so the engine can pin one ISO
language per turn while still speaking mid-turn digests as they complete. Stop paths
pass the full assistant body as `detection_text` and a stable per-reply
`stop:{sha256_prefix}` key shared by every line of that reply. See
[TTS-PIPELINE.md](TTS-PIPELINE.md) for the pin policy.

## Witness file

Per-session `narrate-display-<session>.json` (engine state dir):

- Cross-process accumulator (Claude races per-batch hooks; sibling lock serializes RMW)
- Dedup high-water (`offset` = blockquotes accepted by queue)
- Streaming witness: `Stop`/`speak_reply` silent if witness exists; non-streaming
  sessions never create it → end-of-turn voice unchanged

Seeding:

| Client | Seeds witness |
|---|---|
| Claude / Qwen | `SessionStart` |
| Plain-TUI Codex | never (`--greet-only`); `Stop` path |
| Codex app-server | engine on successful `thread/resume` |
| Grok | engine on first `updates.jsonl` attach (SessionStart is greet-only) |

## Codex adapter

No `MessageDisplay` / `SessionEnd` / `Notification`. Supervisor thread:

1. Learn sessions from IPC (`GreetSession`, `MarkActive`)
2. Attach to shared app-server (default UDS control socket or `ws://` via config);
   opt out of unused notification methods
3. `thread/resume` only threads mapped to registered sessions (Desktop/foreign threads skip)
4. Coalesce deltas (newline / ~150 ms / completed) → `deliver_batch` → TTS queue with session tag
5. Cleanup: evict when thread leaves loaded list or ~12 h idle; sweep orphan state > ~7 d at start

### Launches

`dontspeak codex` for TUI / resume / fork — attach first, then `--remote`. Plain
`codex` TUI stays Stop-fallback. User rules: [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md).

On Unix, DontSpeak first tries the default control socket. If it is unavailable, it
resolves `codex_bin` and chooses the lifecycle Codex supports:

- Standalone binary under `$CODEX_HOME/packages/standalone/current`: run the managed
  `codex app-server daemon start` command.
- Homebrew/npm/other binary: own an ordinary
  `codex app-server --listen unix://<control-socket>` child in a separate process group.

The observer still initializes before `dontspeak codex` returns the endpoint and starts
the TUI, preserving attach-before-TUI ordering. An engine-owned child stays warm for
later launches and is stopped with its process group when the engine shuts down,
streaming is disabled, or the endpoint changes. If an external app-server already owns
the socket, DontSpeak only attaches to it and never stops it.

An external user service remains supported when a server should outlive DontSpeak:

```sh
codex app-server --listen unix://
dontspeak codex
```

The first command must stay running (for example under launchd/systemd). The app-server
control socket is user-only; do not expose an unauthenticated listener beyond the local
machine.

### Config (`config.toml`, live re-read)

| Key | Default | Meaning |
|-----|---------|---------|
| `codex_stream` | `true` | Subscriber on; set `false` to opt out |
| `codex_daemon` | `false` | Keep the subscriber active and opt into app-server auto-start when the endpoint is unavailable |
| `codex_app_server_url` | `""` | Empty = default UDS/loopback; else loopback `ws://` |
| `codex_bin` | `"codex"` | Binary for lazy start |

### Outage policy

Keep state on disconnect (never double-speak). Turns wholly during outage are lost.
Witness at resume keeps `Stop` silent; cumulative text after reconnect covers gaps.

### Correlation

`session_for_thread`: hook `session_id` == app-server thread id (verified live Codex
0.144.1 Windows + loopback WS). Scripted suite pins order, Stop suppression, reconnect
dedup, foreign-thread isolation, cleanup, dropped-response recovery — loopback TCP on
all platforms.

## Cross-platform

- **Unix** — control socket; standalone managed daemon or engine-owned ordinary server
- **Windows** — loopback WebSocket; `dontspeak codex` owns Job-object listener
- Stop fallback on all OSes without a witness

## Grok adapter

No `MessageDisplay`. Engine tails `~/.grok/sessions/<encoded-cwd>/<sessionId>/updates.jsonl`
(ACP NDJSON). Supervisor:

1. Learn sessions from IPC when `source=Grok` (`GreetSession`, `MarkActive`)
2. Resolve `updates.jsonl` via path helpers (cwd scan / newest mtime)
3. Attach at EOF, seed witness once
4. Parse `method=session/update` + `sessionUpdate=agent_message_chunk` + `content.text` only
   (ignore thought/tool/user); batch key = `_meta.promptId` or session id
5. Coalesce deltas (newline / ~150 ms) → `deliver_batch` → TTS queue with **real** session id
6. Stop: `retry_pending` + empty `is_final` flush; witness suppresses chat_history re-voice
7. Cleanup: `SessionEnd` forget; ~12 h idle eviction

### Launches

`dontspeak grok` is **Direct** (starts host if needed, then `grok` with your args) — no
app-server / `--remote` handshake. Mid-turn needs the **host engine** running with
`grok_stream` on (default). Bare `grok` also works once hooks are wired and the host is
up (SessionStart / UserPromptSubmit nudge the registry). Without the host or with
`grok_stream = false`, `Stop` still does end-of-turn narration from `chat_history`.
User rules: [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md).

### Config (`config.toml`, live re-read)

| Key | Default | Meaning |
|-----|---------|---------|
| `grok_stream` | `true` | File-tail on; set `false` to opt out |

No daemon-start or URL keys (Grok writes the session files itself).

## Deploy

- `dontspeakd::codex_stream` / `dontspeakd::grok_stream` → engine/host rebuild
- `ds-narrate` + hooks (Grok Stop finalize) → CLI; lockstep CLI+host for mid-turn
- Hook-set change → `dontspeak wire codex` / `dontspeak wire grok` as needed

See [BUILD-DEPLOY.md](BUILD-DEPLOY.md).
