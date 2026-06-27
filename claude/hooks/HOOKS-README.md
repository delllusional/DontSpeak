# Claude Code voice hooks (DontSpeak)

The voice hooks are **exec-form** entries in `~/.claude/settings.json`: each one
runs the Rust binary directly (`command` = the binary, `args` = the subcommand)
and lets it read the hook JSON from stdin. There are no shell wrappers — the single
`dontspeak` binary (installed to `~/.local/bin`) is the hook executor, dispatched by
ONE of two subcommands split by CONTRACT, not by event:

- `dontspeak notify` — a COMMAND sink: the client tells us an event happened, we run the
  side effect and reply with NOTHING. Async fire-and-forget; never blocks. It routes
  internally on the payload's `hook_event_name`, so every fire-and-forget event is the
  SAME `notify` command — only the event list and per-entry flags differ.
- `dontspeak provide` — a QUERY: the client asks us for input and WAITS for our stdout
  JSON (`hookSpecificOutput`). The only hook the client blocks on.

The shipped stack (in-process native Kokoro, no Python/`uv`) is described in the
repo [README](../../README.md).

## Hook wiring (settings.json)

| Event | Verb | What it does |
|-------|------|--------------|
| `MessageDisplay` | `notify` | The **single narration pipeline**. Speaks EVERY top-level blockquote of EVERY assistant message AS it streams — the prose, the lines Claude leads each tool step with, and the final reply all flow through here. Accumulates the streamed `delta` chunks per `message_id` (or a cumulative `displayedText` if a CC version sends one) and enqueues each newly-completed blockquote on the warm engine. |
| `SessionStart` | `notify` | A new terminal opened → tells the engine to greet in this session's assigned pool voice (only if `greet_on_open` is set); claims the terminal's voice at open. |
| `SessionEnd` | `notify` | Session-scoped `StopSpeech` → barges THIS session's playback so a closing terminal silences its own queued/playing speech (without touching another window's). |
| `UserPromptSubmit` | `notify` | You just prompted HERE → marks this the active terminal so narration follows the window you're working in (the engine holds the others). |
| `UserPromptSubmit` | `provide` | The ONE synchronous hook: re-reads the `narrate` setting every turn and returns the narration spec as `hookSpecificOutput.additionalContext` when ON (so flipping narration takes effect next prompt, no reload); returns nothing when off. |
| `Stop` | `notify` | Turn finished → the reply-done **"ding"** earcon (`Earcon{reply_done}`). (For Codex, whose `Stop` carries `last_assistant_message`, this ALSO voices the reply — Claude Code's `Stop` has no message, so it only dings.) |
| `Notification` | `notify` | A `permission_prompt` / `idle_prompt` notification → the **needs-input** earcon (`Earcon{needs_input}`). Other notification types are ignored. |

Every `notify` entry is `async` fire-and-forget; the lone `provide` entry is synchronous —
Claude Code reads its stdout for the injected context, so it cannot be async.

For **Claude Code** the final reply and tool-step narration are **not** special-cased — they
are just streamed assistant messages handled by `MessageDisplay` (no per-reply mode, no
final-reply dedup). The `Stop` hook is wired only for the turn-done earcon (its payload has
no `last_assistant_message`, so it never re-speaks the streamed reply), and `Notification`
only for the needs-input earcon. Narration is gated on the `narrate` setting (a set of
`shorts` and/or `digests`); an empty set, or `tts_engine = off`, silences it. The earcons are
independent of `narrate`: each plays only when its sound (`earcon_reply_sound` /
`earcon_needs_input_sound`) is set and resolves — empty = off — and honors global mute. The
reply ding defaults to the OS chime (`ding`/`Tink`/`message` on Windows/macOS/Linux); the
needs-input cue ships off.

**OpenAI Codex** uses the very same binary and `hook_event_name` contract, wired into
`~/.codex/config.toml` (auto-detected by `wire-hooks` when `~/.codex` exists; force with
`--codex-only`, skip with `--no-codex`). Codex has no `MessageDisplay` stream, so it wires
two events instead: `UserPromptSubmit` → `provide` injects the narration spec (so Codex
*writes* the spoken-line blockquotes), and `Stop` → `notify` speaks the final reply (the
whole `last_assistant_message`, run through the same blockquote/short logic as streaming).

The narration / greet / mark-active hooks talk to the **warm engine** over the Unix
socket (`dontspeak.sock` in our data dir), so speech is synthesized by the engine's
resident Kokoro with no per-reply model reload. The engine runs in-process inside
`DontSpeak.app` (or the headless `dontspeakd` on Linux/CLI); if it is down the hooks are
best-effort no-ops (they never block Claude).

## Shell helpers

There are no hook shell helpers anymore. The old `term-focused` focus gate (it backed a
removed mid-turn message hook) has been deleted on every platform; the active terminal is
now signalled by the `UserPromptSubmit` → `notify` (mark-active routing) and resolved
engine-side. Caps-lock reading and mic-active probing also run in-process in the engine.
(`mic-active.swift` / `capslock.swift`, if present, are likewise legacy.)

## Setup on a new machine

Run `./scripts/install.sh` from the repo root. It builds the Rust workspace, installs the
binaries into `~/.local/bin` (override with `DONTSPEAK_INSTALL_DIR`), and **PRINTS** the
`settings.json` snippet (with the binary paths filled in) for you to merge yourself — it
does NOT edit your `~/.claude/settings.json`.
There is no launchd/systemd agent on macOS: the engine runs in-process inside
`DontSpeak.app` (built by `apps/macos/bundle.sh`); on Linux the headless `dontspeakd` runs
as a systemd user service (`apps/linux/enable-daemon.sh`).

The `MessageDisplay` narrate entry is only meaningful when the `narrate` setting is
non-empty (it defaults to `shorts` + `digests`); it's harmless (the bin self-gates) when
off. It needs a Claude Code version that supports the `MessageDisplay` hook event.

Logs land in `~/Library/Logs/dontspeak.log` (one line per reply) — check it first when
speech seems silent.
