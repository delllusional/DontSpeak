# Architecture

DontSpeak gives Claude Code a hands-free voice loop: tap Caps Lock to dictate, hear
replies spoken back. One native app per OS — macOS (SwiftUI), Windows (WinUI), and
Linux (GTK4) — hosts the same Rust engine **in-process** over the `ds-core` C ABI. The
Claude Code hooks and an MCP server are thin clients that talk to that engine over a
Unix-domain socket (`ds-ipc`, NDJSON). macOS is the most polished host; all three ship
complete apps, built and tested in CI.

## One app hosts the engine

The engine — Caps-Lock dictation, the TTS queue, local STT, model management, and an
RPC server — is a Rust core exposed over a small C ABI (`ds_engine_start/stop/reload`).
Each platform's app links it and runs it on a background thread, so the OS-permission
work (Accessibility, Microphone) happens inside that one signed app bundle: every TCC
grant lands there, granted once. The app itself is the login item — it starts the
engine on launch and stops it on quit.

## Configuration

Settings live in `config.toml` under the OS data dir (e.g. macOS:
`~/Library/Application Support/DontSpeak/`), a home of its own, separate from any
client's config. `~/.claude/settings.json` stays purely Claude Code's own — its hooks
and its `voice` block. The engine hot-reloads `config.toml` via an mtime-watch, and
applies changes surgically: toggling `stt_engine` or `tts_engine` rebuilds just that
subsystem; per-call params (voice, rate, narrate) take effect on the next call with no
rebuild at all.

## Pluggable STT/TTS engines

Engine selection is a two-field model, each resolved by `resolved_stt` / `resolved_tts`:

* An AUTOMATIC LADDER (`stt_engine_ladder` / `tts_engine_ladder`, config-file only): an
  ordered preference list, walked first-usable-rung. An empty ladder is off.
* A USER PREFERENCE (`stt_engine` / `tts_engine`, settable live via the MCP `set_config`
  tool): a plain scalar string — unset defers to the ladder; `"off"` forces the role off;
  naming exactly one engine forces that engine — with NO automatic substitution. If the
  named engine
  isn't usable on this platform/build, `set_config` REJECTS the change outright (so a
  bad choice is never silently persisted); if a resolved choice can't actually be
  constructed at runtime (e.g. `ds-engines`'s helper-less factory can't host live
  Parakeet), the engine degrades to the SAME inert off-placeholder rather than to a
  different, working engine the user didn't choose.

**STT** — `built_in` (DontSpeak's bundled Parakeet model, the default), `system`
(the OS's on-device recognizer), `claude_code` (delegates to Claude Code's own voice
dictation by tapping its bound `voice:pushToTalk` key).

**TTS** — `Kokoro` (native in-process synth via `ort` + `voice-g2p` + `rodio`, the
default; Apple Silicon also gets a Core ML / ANE path), `System` (the OS voice, e.g.
macOS `say`).

## Caps-Lock dictation

Caps Lock is a tap toggle, driven off the physical key's down/hold/up transitions
rather than the OS lock latch: a quick press-and-release starts or stops recording; a
long-press force-resets to idle. Every enabled startup and OFF-to-ON transition goes
through one shared acquisition sequence that clears any pre-existing logical Caps state
before the app finishes taking ownership of the key; only the suppression and clearing
mechanisms are platform-specific. Once owned, the Caps LED is a pure output DontSpeak
drives to reflect recording state — gesture handling never derives state from the light,
so it can't drift out of sync with recording. Windows reads the key via a low-level hook
(event-driven); macOS and Linux poll the physical key every 30 ms.

## TTS pipeline

The engine owns a single FIFO TTS queue served by a warm helper process that keeps the
Kokoro model loaded. What gets spoken is decided upstream by the `narrate` setting.
Barge-in (starting to record while speech is playing) pauses the queue and resumes on
cancel. Short audible earcons — a reply-done cue and a needs-input cue, each
independently configurable — play outside the queue, mixed over any in-flight speech.

Streaming (mid-turn) narration is ONE shared core (`ds-narrate`: per-message
accumulation → blockquote digests → the per-session on-disk state file that doubles as
the `Stop`-silencing witness). Claude Code feeds it through `MessageDisplay`; Qwen Code's
compatible adapter remains registry-gated until that event ships in a release; and OpenAI
Codex feeds it through a long-lived app-server subscriber inside the engine
(`dontspeakd::codex_stream`). See [docs/STREAMING-NARRATION.md](docs/STREAMING-NARRATION.md)
for internals and [docs/CLIENT-INTEGRATIONS.md](docs/CLIENT-INTEGRATIONS.md) for the
user-facing capability matrix and launch behavior.

## Local STT (Parakeet)

Built-in dictation runs through the same warm helper as TTS: on Caps-ON the helper
opens the mic and streams audio through a cache-aware FastConformer transducer (Apple
Silicon uses a Core ML path via FluidAudio) for live partial transcripts. On Caps-OFF
the engine takes the final transcript and pastes it via a focus-gated key injector.

## Models & ONNX runtime

`ds-model` is the single source of truth for model asset URLs, paths, and digests.
Models download on demand into the app's data dir with pinned SHA-256 checksums.
Inference runs on `ort` (ONNX Runtime), loaded dynamically at runtime so the host
binaries stay lean; Kokoro and Parakeet share one runtime instance. Where available,
CUDA acceleration downloads on demand on Windows/Linux (x86_64), and Apple Silicon gets
a Core ML / Neural Engine path — each platform reports back which execution provider
it's actually running on, so the UI's "Runtime" display reflects reality rather than
just the configured preference.

## FFI boundary

`ds-core` exposes a small, handle-free C ABI (~29 functions) covering engine
lifecycle, read-only status probes, the app-facing engine commands (provider
selection, mute, opening settings), and i18n. `dontspeak.h` is generated by cbindgen
from `src/ffi.rs`.

The engine-to-app status contract (`model_status`) is defined once in Rust
(`rust/crates/ds-status`) and shipped to each UI as JSON; each platform parses it into
its own hand-written DTOs, kept in lockstep by a round-trip test that pins the wire
shape. That single Rust schema plus the contract test cover this boundary's one real
drift risk at a fraction of the cost of a codegen toolchain (uniffi was evaluated and
set aside for this size of surface).

## Workspace layout

The Rust engine lives in `rust/` as a set of small single-purpose crates; the per-OS
apps live in `apps/macos/`, `apps/windows/winui/`, and `apps/linux/gtk/`. See
[rust/README.md](rust/README.md) for the crate-by-crate breakdown.
