# Native frontend protocol (Zed) — the subscriber contract

A **native frontend** is a client app (Zed is the first) that renders DontSpeak's
dictation state itself — IME-style marked text in its focused input plus its own
status indicator — instead of DontSpeak's floating overlay, and receives final
transcripts over the daemon socket (acknowledged) instead of clipboard-paste. The
daemon keeps sole ownership of the CapsLock key, the gesture state machine, mic
capture, STT, the TTS queue, and narration logic; the frontend only *renders* and
*inserts*.

This file is the cross-repo contract: the Zed side (`crates/dontspeak` in the Zed
fork) mirrors the JSON below as byte-exact fixture tests, and
`ds-ipc/src/protocol.rs` pins the same bytes in
`frontend_wire_shapes_match_the_documented_contract`. A serialization change that
renames or reorders anything must update **both** repos and this file in the same
change.

## Transport

The daemon's existing NDJSON RPC socket — one JSON object per `\n`-terminated
line, an AF_UNIX filesystem socket on all three OSes (`ds-ipc/src/transport.rs`;
0600/owner-only permissions), at `state_dir/dontspeak.sock`:

| OS      | socket path |
|---------|-------------|
| Windows | `%LOCALAPPDATA%\DontSpeak\dontspeak.sock` |
| macOS   | `~/Library/Application Support/DontSpeak/dontspeak.sock` |
| Linux   | `$XDG_STATE_HOME`/`~/.local/state/dontspeak/dontspeak.sock` |

A frontend degrades silently when the socket is absent (daemon not installed /
not running) and reconnects with backoff on its own.

## Subscription (persistent connection)

Client → daemon, first line on a fresh connection:

```json
{"cmd":"subscribe_frontend","app":"zed"}
```

- `app` is the frontend's tag in the daemon's `FRONTEND_APPS` identity table
  (`ds-platform/src/lib.rs`) — it maps the tag to per-OS process identities
  (Windows exe basename `zed.exe`; macOS bundles `dev.zed.Zed` /
  `dev.zed.Zed-Preview`; Linux wm_class `dev.zed.Zed`) for frontmost matching.
  A tag outside that table is refused outright (below) rather than left to
  subscribe uselessly — `FrontendRegistry::subscribe` only evicts a SAME-tag
  entry, so an unchecked tag would let a caller that varies it on every retry
  grow the subscriber list without bound.
- The connection is **taken over**: the server stops running its request loop on
  it and streams `frontend_event` lines for the life of the connection. The only
  thing the client ever writes back on it is `ack_deliver` (below).
- **One live subscription per app tag.** A resubscribe (e.g. Zed restarted while
  the old TCP-level connection lingers) evicts and *closes* the previous
  subscriber — a stale instance sees EOF and knows it lost the subscription.
- **Kill-switch:** when `frontend_enabled = false` in `config.toml`
  (`ds-config/src/voice.rs`, default `true`), the daemon refuses with a terminal
  error line instead of taking the connection over:

```json
{"ok":"error","message":"frontend subscriptions are disabled (frontend_enabled = false in config.toml)"}
```

- **Unknown tag:** a subscribe whose `app` isn't in `FRONTEND_APPS` is refused
  the same way:

```json
{"ok":"error","message":"unknown frontend app tag 'vscode'"}
```

## Dictation events (daemon → client, streamed)

Every event line carries a monotonically increasing `seq` (correlation id +
ordering witness). Exact byte shapes:

A subscription starts with the next event; the daemon does not replay a current-state
snapshot. A frontend that reconnects during an owned dictation may therefore first see
`partial`, `awaiting_confirm`, `deliver`, or a terminal reset. Treat `partial` and
`awaiting_confirm` as implicit recording starts (initialize the surface before applying
their text), and make terminal resets idempotent.

```json
{"ok":"frontend_event","event":"recording_started","seq":1}
{"ok":"frontend_event","event":"partial","text":"hello wor","seq":2}
{"ok":"frontend_event","event":"awaiting_confirm","text":"hello world","seq":3}
{"ok":"frontend_event","event":"deliver","text":"hello world","submit":true,"seq":4}
{"ok":"frontend_event","event":"cancelled","seq":5}
{"ok":"frontend_event","event":"refused","seq":6}
```

Semantics (derived in the engine from the same status-digest transition that
drives the overlay, so every dictation-ending path emits a terminal event):

| event               | meaning | expected frontend rendering |
|---------------------|---------|------------------------------|
| `recording_started` | a frontend-owned PTT dictation opened the mic | show recording state; mark empty text in the focused input |
| `partial`           | live partial transcript (cumulative — replaces the previous partial) | set IME-style marked text |
| `awaiting_confirm`  | recording ended; transcript awaits the confirm gesture | keep marked text |
| `deliver`           | final transcript — **must be acked** (below) | clear mark; insert `text` into the focused input; when `submit` is true, dispatch the input's submit (Enter) |
| `cancelled`         | dictation ended with nothing to deliver (long-press cancel, empty final, teardown) | clear mark, reset indicator |
| `refused`           | the dictation attempt was refused (e.g. models not ready) | reset indicator |

## Acknowledged delivery (`deliver` → `ack_deliver`)

Client → daemon, on the **same** subscribed connection, echoing the `deliver`
event's `seq`:

```json
{"cmd":"ack_deliver","seq":4,"ok":true}
```

- `ok:true` = the transcript was inserted into a focused input. Only then does
  the daemon count the delivery as successful and skip the clipboard-paste path.
  When `submit` was true, the daemon also performs the same voice-submit
  bookkeeping as the classic auto-Enter path, so the subsequent
  `UserPromptSubmit → mark_active` from a hook-wired agent is deduplicated as a
  voice submit.
- `ok:false` (frontend could not insert: no active window, no input handler,
  input rejected the text) → the daemon falls back to the classic
  clipboard-paste delivery. **An utterance is never lost.**
- The daemon spends at most **~300 ms** (`ACK_DELIVER_TIMEOUT`,
  `dontspeakd/src/ipc.rs`) **end-to-end** on the `deliver` write **plus** the
  ack wait — one shared deadline, not two independent 300 ms phases. Time spent
  on the write shrinks the remaining ack budget. This runs on the engine's
  single tick thread, so Caps poll / gesture classification / LED sync stall for
  that window. Subscription write timeout is also set to that budget at
  takeover (not the constructor's generous 5s one-shot-RPC default) so a
  frontend that stops reading entirely can't block for seconds on the write
  alone. On nack, timeout, write error, or EOF it falls back to paste **and
  drops the subscriber** (its connection state is no longer trustworthy; a late
  duplicate insert of the same text must not race the paste). The frontend just
  reconnects and resubscribes.
- "No live subscriber whose app is frontmost" is also a failed delivery (classic
  path), but keeps everyone subscribed.

## Ownership & fallback semantics

- **Ownership is decided per dictation, at `start_recording`, PTT (CapsLock)
  path only:** the dictation is frontend-owned iff a live subscriber's app is
  frontmost right then (`FrontmostWindow::is_app_frontmost`, fail-closed —
  including on Wayland, which has no portable active-window query and so
  always reads `false`: a dictation there is never frontend-owned, exactly as
  if Zed weren't subscribed). Always-listening submissions keep the classic
  overlay + paste path entirely.
- While a dictation is frontend-owned the engine reports the overlay state token
  as `hidden`, so none of the tray hosts draw the floating overlay; the frontend
  is the only dictation UI. If delivery later fails, the tag is cleared and
  classic behavior (overlay token, paste) resumes.
- When Zed is not frontmost, not running, or not subscribed: today's behavior,
  byte-for-byte (floating overlay + clipboard paste — Zed stays whitelisted as a
  paste target via `CUSTOM_TEXT_BUNDLES`/`CUSTOM_TEXT_EXES`).

## Panel-agent narration (`narrate_batch`, one-shot connections)

For agents running in the frontend's own UI (Zed's agent panel / ACP threads)
the frontend feeds cumulative assistant-message text into the same
blockquote-digest pipeline the CLI hooks use (`ds_narrate::narrate_batch`):

```json
{"cmd":"narrate_batch","session":"<id>","key":"<session>#<generation>#<entry-ix>","text":"<cumulative>","is_final":false}
```

→ `{"ok":"done"}`. Sent as ordinary one-shot requests on fresh connections (not
on the subscribed one). `key` is the frontend's stable per-message key — a new
key starts a new accumulation; `is_final:true` marks the turn's last batch
(completes the final blockquote run, allows the "shorts" fallback). Dedup is the
daemon's on-disk per-session `DisplayState` (file-locked), so replays and
concurrent hook/daemon callers are safe. Mic gating is daemon-side (system-wide
mic probe). CLI agents in the frontend's *terminals* need none of this — the
existing hooks narrate them regardless of the hosting app.

Session lifecycle mirroring uses the existing verbs, same one-shot style:
`{"cmd":"mark_active","session":"<id>","source":"unknown"}` on prompt send,
`{"cmd":"session_end","session":"<id>","source":"unknown"}` on thread close,
`{"cmd":"stop_speech","session":"<id>","source":"unknown"}` for barge-in. `source`
is a REQUIRED `ClientSource` token on every client-originated request
(`ds-ipc/src/protocol.rs`; absent ⇒ hard decode error, see
`request_without_source_is_a_hard_decode_error`) — `unknown` is the placeholder
here since none of the four wireable clients (`ClientSource::CLIENTS`) name a
frontend like Zed; whether Zed instead warrants its own dedicated
`ClientSource` variant for log attribution is an open follow-up, not decided
by this contract.

## Focus gate: Zed counts as a terminal, but never as a key target

DontSpeak's TTS focus gate (`pause_in_background`) holds narration while no
terminal is frontmost. Zed hosts CLI agents in embedded terminals, so the shared
terminal table (`ds_platform::KNOWN_TERMINALS`) carries Zed rows — narration
keeps speaking while Zed is frontmost — but with **`inject_keys: false`**: the
`claude_code` STT engine's push-to-talk chord tap is gated on the
inject-eligible subset (`is_inject_terminal_frontmost()`), so a Caps tap with
Zed frontmost and `stt_engine = claude_code` never types the chord into a Zed
buffer.

Latch implication (documented on `pause_in_background` in
`ds-config/src/voice.rs`): the focus gate only arms once a known terminal has
been seen frontmost (`terminal_seen`). Because Zed is in the table, focusing Zed
once arms the gate — from then on, with `pause_in_background = true`, speech
holds whenever a non-terminal app is frontmost, even if the user never focuses a
classic terminal emulator.

## Version skew

Both repos pin the counterpart commit next to their fixtures. The daemon's
`Response` decoder treats unknown `ok` tags as terminal `unknown` (forward
compat for its own CLI clients); a frontend should likewise ignore
`frontend_event` lines whose `event` tag it doesn't know, and treat unknown
top-level lines on the subscribed connection as no-ops rather than errors.
