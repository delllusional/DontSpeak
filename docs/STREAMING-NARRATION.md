# Streaming (mid-turn) narration — one core, three adapters

DontSpeak speaks an assistant reply's *digest* (every top-level `>` blockquote, verbatim,
each once, in document order — plus the "shorts" fallback for a short blockquote-less
final reply) **while the reply streams**, not just at end of turn. The logic lives in ONE
crate — `ds-narrate` — and every client feeds it through a thin adapter:

| Client       | Transport                                     | Payload shape                                        | Adapter |
|--------------|-----------------------------------------------|------------------------------------------------------|---------|
| Claude Code  | `MessageDisplay` hook, one process per batch  | incremental `delta` keyed by content-block `index`   | `dontspeak::hook_narrate` |
| Qwen Code    | `MessageDisplay` hook (0.19.10+)              | cumulative `displayed_text` + `is_final` (snake_case)| `dontspeak::hook_narrate` |
| OpenAI Codex | the engine's **app-server subscriber**        | `item/agentMessage/delta` + `item/completed`         | `dontspeakd::codex_stream` |

All three build a client-neutral `StreamBatch` and call the same file-backed delivery step,
`ds_narrate::deliver_batch`.

## The witness contract

Every adapter persists per-session progress through the SAME on-disk file,
`narrate-display-<session>.json` (in the engine's state dir). That file is
simultaneously:

* the **cross-process accumulator** (Claude Code spawns racing per-batch hook
  processes; a lock file beside it serializes the read-modify-write);
* the **dedup high-water mark** (`offset` = blockquote runs accepted by the queue) — a
  reconnect, an engine restart, or a replayed final batch can never double-speak;
* the **streaming witness**: the `Stop` hook's `speak_reply` checks
  `witness_exists(session)` and stays silent when a streaming pass already narrated the
  session. Clients/sessions that never stream never create the file, so their `Stop`
  voices the whole reply exactly as before.

Who seeds the witness:

* Claude Code — `SessionStart` (plain `notify`).
* Qwen Code — `SessionStart`, like Claude Code.
* Plain-TUI Codex — never (`notify --greet-only`); `Stop` is its path.
* Codex on the shared app-server — **the engine**, immediately on a successful
  `thread/resume` (closing the race where a short turn's `Stop` could beat the first
  coalesced flush).

## The Codex adapter (`dontspeakd::codex_stream`)

Codex's hook system has **no `MessageDisplay` stream, no `SessionEnd`, no
`Notification`** — so mid-turn narration can't ride hooks. Instead the engine (the one
long-lived resident process) runs a supervisor thread that:

1. learns session ids from the hooks over IPC (`GreetSession` at SessionStart,
   `MarkActive` at every prompt submit — the latter is also re-discovery after an engine
   restart, and re-arms a session whose thread wasn't loaded yet);
2. attaches to the user's **shared codex app-server** — by default the unix control
   socket `$CODEX_HOME/app-server-control/app-server-control.sock` (WebSocket frames
   over UDS), or a `ws://` TCP endpoint via config — with `initialize` /`initialized`
   and an `optOutNotificationMethods` list that silences the delta streams we never
   consume;
3. lists loaded threads (`thread/loaded/list`) and `thread/resume`s **only** threads
   whose id maps to a registered session (`session_for_thread`, expected uuid
   passthrough) — a Codex Desktop or third-party thread on the same daemon is never
   narrated, and CC/Qwen session ids simply never match;
4. coalesces `item/agentMessage/delta` per item (flush on newline / ~150 ms /
   `item/completed`), feeds `deliver_batch`, and enqueues each utterance on the engine's
   TTS queue tagged with the session id — so per-terminal hold/active routing, pool
   voices, and scoped barge all work unchanged;
5. **owns cleanup** (no SessionEnd hook exists): evicts a session — deleting its
   state/lock/tmp trio — when its thread disappears from the daemon's loaded list or
   after a long idle TTL (~12 h), and sweeps crash-orphaned `narrate-display-*` files
   older than ~7 days at startup.

### Observable Codex launches

Use `dontspeak codex` for the base TUI, `resume`, or `fork`. The launcher asks the engine
to attach first, starts the local app-server path on demand, and passes the initialized
endpoint to `codex --remote`. A plain `codex` TUI is not on that shared server and keeps
the end-of-turn `Stop` fallback. User-facing launch rules and noninteractive-command
behavior are centralized in [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md).

### Configuration (`config.toml`; all re-read live, no restart)

| Key | Default | Meaning |
|-----|---------|---------|
| `codex_stream` | `true` | Master switch for the subscriber (attach + narrate). Observation-only against the user's own socket; inert without `~/.codex` + a running app-server. Note: after an upgrade the engine will start attaching to your daemon socket unprompted — set `false` to opt out. |
| `codex_stream_daemon_start` | `false` | Opt-in app-server lifecycle, started proactively so a remote TUI can connect before its first hook: run the idempotent managed daemon on Unix; on Windows own `codex app-server --listen <codex_app_server_url>` for the engine's lifetime. |
| `codex_app_server_url` | `""` | Empty = the default Unix control socket or Windows loopback listener; a loopback `ws://IP:PORT` overrides it. |
| `codex_bin` | `"codex"` | Binary for the lazy daemon start; bare names resolve via PATH + common install dirs. |

### Outage / cleanup tradeoffs (deliberate)

* On transient disconnects the per-session state files are **kept** — bias: never
  double-speak. Narration for turns that happen entirely during an app-server outage is
  lost (the reply was already superseded by the time we reconnect) rather than replayed.
* Because the witness is seeded at resume, a streamed session's `Stop` stays silent even
  if the connection drops mid-turn; the next `item/completed` after reconnect covers the
  missed deltas (cumulative text wins in the accumulator).

### Correlation verification

`session_for_thread` uses `hook session_id == app-server thread id` (a root thread's id
is its rollout session id; the resume response's `thread.sessionId` is cross-checked and
divergence logged). This was verified live on 2026-07-12 with Codex 0.144.1 on Windows,
using `codex app-server --listen ws://127.0.0.1:4500` and two `codex --remote` clients:

* remote sessions fired `SessionStart`, `UserPromptSubmit`, and `Stop` hooks;
* the hook session id exactly matched the id returned by `thread/loaded/list`;
* `thread/resume` succeeded for that same id, with no authentication token on the
  loopback WebSocket endpoint;
* a live ephemeral `thread/fork` returned a fresh `thread.id` equal to its fresh
  `thread.sessionId` (the root appears separately as `forkedFromId`), preserving the
  subscriber's id-passthrough correlation for forks;
* the streaming witness was created and the Stop hook remained the deduplicated
  end-of-turn path.

The Unix-domain-socket authentication expectation remains platform-derived rather than
live-verified on this Windows capture.

The scripted app-server integration suite now runs over loopback TCP on every platform,
including Windows. It pins streaming order, exactly-once Stop suppression, reconnect
deduplication, foreign-thread isolation, cleanup, and recovery when either
`thread/loaded/list` or `thread/resume` drops a response without closing the socket.

## Cross-platform status

* **macOS / Linux** — full support over the unix control socket (`cfg(unix)`).
* **Windows** — verified live with Codex 0.144.1 over loopback WebSocket. An explicit
  `dontspeak codex` launch starts and owns the direct listener without changing the
  persistent auto-start preference. The child runs in a kill-on-close Job Object, so
  normal exit, host crash, and force-termination all tear down the listener.
* The Stop fallback remains available on all three OSes for any session without a streaming witness.

## Qwen Code

Qwen 0.19.10 sends debounced cumulative `MessageDisplay` snapshots:
`{hook_event_name, message_id, displayed_text, is_final}`. The registry enables streaming
with Qwen's inline-shell command dialect and millisecond timeouts; the shared adapter handles
the cumulative payload.

## Deploy routes (all three apply — see docs/BUILD-DEPLOY.md)

* `dontspeakd::codex_stream` is an **engine** change → full app rebuild per OS.
* `ds-narrate` + hook/CLI adapters → the CLI route; the Codex launch handshake also changes
  the engine IPC protocol, so ship the CLI and host app in lockstep.
* The Codex hook-set change (UserPromptSubmit `notify` + `provide`) → re-run
  `dontspeak wire codex`.
