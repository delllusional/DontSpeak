# Architecture

Hands-free voice loop for Claude Code (and siblings): Caps Lock dictation, spoken
replies. One native host per OS — macOS (SwiftUI), Windows (WinUI), Linux (GTK4) —
links the same Rust engine **in-process** via `ds-core` C ABI. Hooks and MCP are
thin clients over Unix-domain socket (`ds-ipc`, NDJSON). All three hosts ship in CI;
macOS is the most polished.

## In-process engine

The engine (Caps dictation, TTS queue, local STT, models, RPC) is Rust over a small
C ABI (`ds_engine_start/stop/reload`). Each app links it on a background thread so
OS permissions (Accessibility, Microphone) and the login item live in one signed
bundle.

## Configuration

`config.toml` under the OS data dir (e.g. macOS
`~/Library/Application Support/DontSpeak/`). Separate from any client config —
`~/.claude/settings.json` stays Claude's (hooks + `voice`). Hot-reload by mtime:
`stt_engine` / `tts_engine` rebuilds that subsystem; voice/rate/narrate apply next call.

## Pluggable STT/TTS

Two fields each, resolved by `resolved_stt` / `resolved_tts`:

- **Ladder** (`stt_engine_ladder` / `tts_engine_ladder`, file only): ordered preference;
  empty = off.
- **User preference** (`stt_engine` / `tts_engine`, also MCP `set_config`): unset →
  ladder; `"off"` → off; named engine → that engine only, no auto-substitution.
  Unusable choice: `set_config` rejects. Runtime construct failure → inert off
  placeholder (not a different engine).

**STT ladder default:** `system` → `built_in` (Parakeet) → `claude_code`. `system` is
macOS-only today, so Parakeet leads on Windows/Linux.

**TTS:** `Kokoro` (in-process `ort` + `voice-g2p` + `rodio`; Core ML/ANE on Apple
Silicon) or `System` (OS voice).

## Caps Lock

Tap toggle from physical down/hold/up (not the OS latch): short press starts/stops
recording; long-press force-resets idle. Startup and OFF→ON share one acquisition that
clears pre-existing logical Caps before ownership. LED is pure output for recording
state — gestures never read the light. Windows: low-level hook. macOS/Linux: 30 ms poll.

## TTS pipeline

One FIFO TTS queue; warm helper keeps Kokoro loaded. `narrate` selects content.
Barge-in pauses and resumes on cancel. Session-scoped earcons (reply-done,
needs-input) queue after admitted narration; they don't mix over in-flight speech
except needs-input under pause-in-background focus hold (idle playback → sound
immediately). Spec: [docs/TTS-PIPELINE.md](docs/TTS-PIPELINE.md).

Streaming mid-turn: shared `ds-narrate` (accumulate → blockquote digests → on-disk
witness for `Stop` silence). Claude: `MessageDisplay`; Qwen: registry-gated adapter;
Codex: in-engine app-server subscriber (`dontspeakd::codex_stream`); Grok: in-engine
file-tail of interactive `updates.jsonl` (`dontspeakd::grok_stream`). See
[docs/STREAMING-NARRATION.md](docs/STREAMING-NARRATION.md) and
[docs/CLIENT-INTEGRATIONS.md](docs/CLIENT-INTEGRATIONS.md).

## Local STT (Parakeet)

Same warm helper as TTS: Caps-ON opens mic → streaming FastConformer (Core ML via
FluidAudio on Apple Silicon). Caps-OFF: final transcript → focus-gated key injector.
Details: [docs/STT-PIPELINE.md](docs/STT-PIPELINE.md).

## Models & ONNX

`ds-model`: URLs, paths, digests. On-demand download into app data dir, SHA-256 pinned.
`ort` loaded dynamically; Kokoro and Parakeet share one runtime. CUDA on demand
(Windows/Linux x86_64); Core ML/ANE on Apple Silicon. UI "Runtime" reflects the EP
actually in use.

Kokoro English frontend uses checksum-pinned BART G2P (ORT) before backend selection —
Core ML TTS path still needs the ORT dylib. See
[docs/TTS-PIPELINE.md](docs/TTS-PIPELINE.md#models-runtime-and-backends).

## FFI boundary

`ds-core`: handle-free C ABI (~32 fns) — lifecycle, status, app commands, i18n.
`dontspeak.h` from cbindgen on `src/ffi.rs`.

`model_status`: defined once in `ds-status`, shipped as JSON; each UI has hand-written
DTOs locked by a round-trip contract test. No uniffi/codegen for this surface.

## Workspace layout

Engine: `rust/`. Hosts: `apps/macos/`, `apps/windows/winui/`, `apps/linux/gtk/`.
Crate map: [rust/README.md](rust/README.md).
