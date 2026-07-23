# Architecture

Hands-free voice loop for Claude Code (and siblings): Caps Lock dictation, spoken
replies. One native host per OS — macOS (SwiftUI), Windows (WinUI), Linux (GTK4) —
links the same Rust engine **in-process** via `ds-core` C ABI. Hooks and MCP are
thin clients over Unix-domain socket (`ds-ipc`, NDJSON).

## In-process engine

The engine (Caps dictation, TTS queue, local STT, models, RPC) is Rust over a small
C ABI (`ds_engine_start` / `ds_engine_stop`). Each app links it on a background
thread so OS permissions (Accessibility, Microphone) and the login item live in
one signed bundle.

## Configuration

`config.toml` under the OS data dir (e.g. macOS
`~/Library/Application Support/DontSpeak/`). Separate from any client config —
`~/.claude/settings.json` stays Claude's (hooks + `voice`). Hot-reload by mtime:
engine, built-in model, and provider changes reconcile the shared helper;
voice/model parameters/narrate apply on the next call.

## Pluggable STT/TTS

Two fields each, resolved by `resolved_stt` / `resolved_tts`:

- **Ladder** (`stt_engine_ladder` / `tts_engine_ladder`, file only): ordered preference;
  empty = off.
- **User preference** (`stt_engine` / `tts_engine`, also MCP `set_config`): unset →
  ladder; `"off"` → off; named engine → that engine only, no auto-substitution.
  Unusable choice: `set_config` rejects. Runtime construct failure → inert off
  placeholder.

**STT ladder default:** `system` → `built_in` (Parakeet) → `claude_code`. `system` is
macOS-only today, so Parakeet leads on Windows/Linux.

**TTS engines:** `built_in` or `system`. The built-in model registry contains Kokoro,
Chatterbox Multilingual, Qwen3-TTS, and OmniVoice. Every model has ORT CPU, ORT CUDA,
and MLX; Kokoro alone has Core ML, rate control, and full-duplex. Model capabilities
drive voice, rate, full-duplex, download, and provider selection. Speech language is
detected per utterance in the shared text pipeline and never persisted as a synthesis setting.

## Caps Lock

Tap toggle from physical down/hold/up (not the OS latch): short press starts/stops
recording; long-press force-resets idle. Startup and OFF→ON share one acquisition that
clears pre-existing logical Caps before ownership. LED is pure output for recording
state — gestures never read the light. Windows: low-level hook. macOS/Linux: 30 ms poll.

## TTS pipeline

One FIFO TTS queue; the warm helper keeps the selected built-in model loaded. `narrate` selects content.
Barge-in pauses and resumes on cancel. Session-scoped earcons (reply-done,
needs-input) queue after admitted narration; they don't mix over in-flight speech
except needs-input under pause-in-background focus hold (idle playback → sound
immediately). Spec: [docs/TTS-PIPELINE.md](docs/TTS-PIPELINE.md).

Streaming mid-turn: shared `ds-narrate` (accumulate → blockquote digests → on-disk
witness for `Stop` silence). Claude: `MessageDisplay`; Qwen: registry-gated adapter;
Codex: in-engine app-server subscriber (`dontspeakd::codex_stream`); Grok: in-engine
file-tail of interactive `updates.jsonl` (`dontspeakd::grok_stream`). Kimi Code:
non-streaming — `Stop` voices the reply from the session `wire.jsonl`. Hermes Agent:
non-streaming shell hooks — remapped `post_llm_call` → `Stop` voices
`extra.assistant_response`. See [docs/STREAMING-NARRATION.md](docs/STREAMING-NARRATION.md)
and [docs/CLIENT-INTEGRATIONS.md](docs/CLIENT-INTEGRATIONS.md).

## Local STT (built-in)

Same warm helper as TTS: Caps-ON opens mic → Parakeet TDT 0.6b v3 over ORT on
Windows/Linux, or the same model via MLX Audio on Apple Silicon. Caps-OFF: final
transcript → focus-gated key injector. The model is full-context, so dictation is decoded
one speech segment at a time at the pauses a VAD endpointer finds.
Details: [docs/STT-PIPELINE.md](docs/STT-PIPELINE.md).

## Models & ONNX

`ds-model`: URLs, paths, digests. On-demand parallel download into app data dir,
SHA-256 pinned. The engine also exposes an on-disk inventory (sizes per model) and
removal of non-active models over `ds-ipc`, surfaced as the `models` MCP tool.
`ort` is loaded dynamically; all ORT TTS models and Parakeet share one runtime. CUDA on demand
(Windows/Linux x86_64); explicit ORT Core ML for Kokoro on macOS; MLX on Apple Silicon for every
built-in model. Intel macOS never builds or bundles MLX code; its built-in path remains ORT CPU
when an Intel-compatible runtime is present. A dependency-free Swift shim retains Apple System
STT on Intel. UI "Runtime" reflects the backend in use.

Kokoro English frontend uses checksum-pinned BART G2P (ORT) before backend selection —
MLX Kokoro still needs that ORT dylib. See
[docs/TTS-PIPELINE.md](docs/TTS-PIPELINE.md#models-and-backends).

## FFI boundary

`ds-core`: handle-free C ABI (35 fns) — lifecycle, status, app commands, i18n.
`dontspeak.h` from cbindgen on `src/ffi.rs` (count the `pub extern "C" fn` items
there; the preamble in `cbindgen.toml` is narrative only).

`model_status`: defined once in `ds-status`, shipped as JSON; each UI has hand-written
DTOs locked by a round-trip contract test (`ds-status` Rust fixture, Windows
`HealthSnapshotTests`, macOS `ModelStatusContractTests`). No uniffi/codegen for
this surface.

Crate map: [rust/README.md](rust/README.md).
