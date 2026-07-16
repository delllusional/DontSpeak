# Streaming (mid-turn) narration

Speaks top-level `>` digests (plus shorts fallback) while the reply streams. Core:
`ds-narrate`. Adapters:

| Client | Transport | Payload | Adapter |
|---|---|---|---|
| Claude Code | `MessageDisplay` (per-batch process) | incremental `delta` by block `index` | `dontspeak::hook_narrate` |
| Qwen Code 0.19.10+ | `MessageDisplay` | cumulative `displayed_text` + `is_final` | `dontspeak::hook_narrate` |
| OpenAI Codex | engine app-server subscriber | `item/agentMessage/delta` + `item/completed` | `dontspeakd::codex_stream` |

All build `StreamBatch` → `ds_narrate::deliver_batch`.

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

### Config (`config.toml`, live re-read)

| Key | Default | Meaning |
|-----|---------|---------|
| `codex_stream` | `true` | Subscriber on; set `false` to opt out |
| `codex_stream_daemon_start` | `false` | Opt-in managed daemon start |
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

- **Unix** — control socket (`cfg(unix)`)
- **Windows** — loopback WebSocket; `dontspeak codex` owns Job-object listener
- Stop fallback on all OSes without a witness

## Deploy

- `dontspeakd::codex_stream` → engine/host rebuild
- `ds-narrate` + hooks → CLI; Codex launch handshake also IPC → lockstep CLI+host
- Hook-set change → `dontspeak wire codex`

See [BUILD-DEPLOY.md](BUILD-DEPLOY.md).
