# Streaming (mid-turn) narration — one core, three adapters

DontSpeak speaks an assistant reply's *digest* (every top-level `>` blockquote, verbatim,
each once, in document order — plus the "shorts" fallback for a short blockquote-less
final reply) **while the reply streams**, not just at end of turn. The logic lives in ONE
crate — `ds-narrate` — and every client feeds it through a thin adapter:

| Client       | Transport                                     | Payload shape                                        | Adapter |
|--------------|-----------------------------------------------|------------------------------------------------------|---------|
| Claude Code  | `MessageDisplay` hook, one process per batch  | incremental `delta` keyed by content-block `index`   | `dontspeak::hook_narrate` |
| Qwen Code    | same hook route (upstream hook still pending) | cumulative `displayed_text` + `is_final` (snake_case)| `dontspeak::hook_narrate` (serde aliases; registry-gated) |
| OpenAI Codex | the engine's **app-server subscriber**        | `item/agentMessage/delta` + `item/completed`         | `dontspeakd::codex_stream` |

All three build a client-neutral `StreamBatch` and call the same file-backed step,
`ds_narrate::narrate_batch`.

## The witness contract

Every adapter persists per-session progress through the SAME on-disk file,
`narrate-display-<session>.json` (in the engine's state dir). That file is
simultaneously:

* the **cross-process accumulator** (Claude Code spawns racing per-batch hook
  processes; a lock file beside it serializes the read-modify-write);
* the **dedup high-water mark** (`offset` = blockquote runs already voiced) — a
  reconnect, an engine restart, or a replayed final batch can never double-speak;
* the **streaming witness**: the `Stop` hook's `speak_reply` checks
  `witness_exists(session)` and stays silent when a streaming pass already narrated the
  session. Clients/sessions that never stream never create the file, so their `Stop`
  voices the whole reply exactly as before.

Who seeds the witness:

* Claude Code — `SessionStart` (plain `notify`).
* Qwen Code / plain-TUI Codex — never (`notify --greet-only`); `Stop` is their path.
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
   `item/completed`), feeds `narrate_batch`, and enqueues each utterance on the engine's
   TTS queue tagged with the session id — so per-terminal hold/active routing, pool
   voices, and scoped barge all work unchanged;
5. **owns cleanup** (no SessionEnd hook exists): evicts a session — deleting its
   state/lock/tmp trio — when its thread disappears from the daemon's loaded list or
   after a long idle TTL (~12 h), and sweeps crash-orphaned `narrate-display-*` files
   older than ~7 days at startup.

### User setup (what makes a Codex session observable)

```sh
codex app-server daemon start   # once (idempotent)
codex --remote unix://          # run the TUI as a client of the shared server
```

A plain `codex` TUI session is NOT on the shared server: it keeps today's behavior —
the whole reply is voiced at `Stop`. `dontspeak wire codex` prints this hint.

### Configuration (`config.toml`; all re-read live, no restart)

| Key | Default | Meaning |
|-----|---------|---------|
| `codex_stream` | `true` | Master switch for the subscriber (attach + narrate). Observation-only against the user's own socket; inert without `~/.codex` + a running app-server. Note: after an upgrade the engine will start attaching to your daemon socket unprompted — set `false` to opt out. |
| `codex_stream_daemon_start` | `false` | Opt-in: run the idempotent `codex app-server daemon start` when the socket is absent. |
| `codex_app_server_url` | `""` | Empty = the default unix control socket; `ws://IP:PORT` attaches over TCP (the Windows/custom path). |
| `codex_bin` | `"codex"` | Binary for the lazy daemon start; bare names resolve via PATH + common install dirs. |

### Outage / cleanup tradeoffs (deliberate)

* On transient disconnects the per-session state files are **kept** — bias: never
  double-speak. Narration for turns that happen entirely during an app-server outage is
  lost (the reply was already superseded by the time we reconnect) rather than replayed.
* Because the witness is seeded at resume, a streamed session's `Stop` stays silent even
  if the connection drops mid-turn; the next `item/completed` after reconnect covers the
  missed deltas (cumulative text wins in the accumulator).

### Correlation note (§9 live capture — REQUIRED before shipping)

`session_for_thread` assumes `hook session_id == app-server thread id` (a root thread's
id is its rollout session id; the resume response's `thread.sessionId` is cross-checked
and divergence logged). This is *expected* but *not yet pinned against a live capture* —
codex was not installed on the implementation machine. Before release:

1. `codex app-server daemon start`; `codex --remote unix://`;
2. temporarily log raw SessionStart/Stop hook payloads at the `notify` front door (the
   BUILD-DEPLOY.md debugging pattern) and the `thread/loaded/list` reply;
3. confirm the ids match, confirm hooks still fire for `--remote`-hosted sessions
   (undocumented — if they do NOT: streaming narration still works and `Stop` simply
   never fires, but greet/`provide` would be lost; record either way in the registry
   comment), and confirm the UDS handshake needs no `--ws-auth` token (expected:
   filesystem perms are the auth on the unix socket).

## Cross-platform status

* **macOS / Linux** — full support over the unix control socket (`cfg(unix)`).
* **Windows** — the upstream codex daemon is Unix-only today and `std` has no
  `UnixStream`; the `ws://` override path compiles and works for a user who runs
  `codex app-server --listen ws://…` themselves. Otherwise Codex on Windows keeps
  Stop-only narration, unchanged. **Follow-up:** re-check when upstream ships Windows
  daemon support (then evaluate `uds_windows` if it binds AF_UNIX).
* The Stop fallback is untouched on all three OSes for any unwired/unstreamed session.

## Qwen Code: the pending flip

Qwen's sketched `MessageDisplay` hook (QwenLM/qwen-code#6488) sends debounced cumulative
snapshots — `{hook_event_name, message_id, displayed_text, is_final}`. The handler
already parses that shape (serde aliases on `displayed_text` and `is_final`), and the
wiring for `hook_streaming: true` + `InlineShell` is pinned by test. When the upstream
hook lands, the whole flip is the registry gate + a version-pin bump via the
`verify-wiring` skill — zero core or handler edits.

## Deploy routes (all three apply — see docs/BUILD-DEPLOY.md)

* `dontspeakd::codex_stream` is an **engine** change → full app rebuild per OS.
* `ds-narrate` + the hook/CLI adapters → the `install-daemon.sh` (CLI) route.
* The Codex hook-set change (UserPromptSubmit `notify` + `provide`) → re-run
  `dontspeak wire codex`.
