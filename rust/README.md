# dontspeak — Rust workspace

In-process built-in TTS (`ort` + model pipelines + `rodio`). `dontspeakd::engine_run`
is a library each OS app hosts via `ds-core` C ABI. Hooks and MCP are the `dontspeak`
binary over `dontspeak.sock` (NDJSON); they never load models. Design:
[../ARCHITECTURE.md](../ARCHITECTURE.md).

## Status

| Target | `ds-platform` | CI |
| --- | --- | --- |
| macOS Apple Silicon | IOKit, CGEventPost, NSWorkspace | `macos-26` |
| Windows | GetKeyState / SendInput / GetForegroundWindow | `windows-2025` |
| Linux | evdev + uinput + x11rb (Wayland degraded) | `ubuntu-latest` |

## Crates

```
rust/crates/
  ds-config/       # paths, config.toml, wire registry/shapers, config enums
  ds-client/       # WiredClient enum (leaf: wiring, hooks, IPC, MCP, logs)
  ds-log/          # unified activity log
  ds-earcon/       # OS sound introspection + cue resolution
  ds-ipc/          # NDJSON RPC (server=engine; fuzz workspace under fuzz/)
  ds-proc/         # pid parse/liveness + process-group kill
  ds-platform/     # KeyInjector / FrontmostWindow / CapsKeyMonitor per OS
  ds-http/         # bounded blocking HTTP + native trust roots
  ds-agent-usage/  # read-only weekly/monthly coding-agent quotas
  ds-model/        # parallel download + checksum; ORT session; MLX Audio shim loader
  ds-voices/       # voice/language enum
  ds-tts/          # built-in TTS pipelines: Kokoro, Chatterbox, Qwen, OmniVoice
  ds-stt/          # Parakeet / ClaudeNative / SystemStt
  ds-aec/          # echo-cancelled duplex (VPIO / WASAPI / Pulse)
  ds-helper/       # warm TTS+STT child (one-shot + --serve)
  ds-helper-proto/ # stdout reply tokens
  ds-engines/      # STT factory
  ds-tools/        # MCP tool catalog
  ds-i18n/         # UI strings (locales/en.yml)
  ds-status/       # model_status contract
  ds-narrate/      # streaming narration core
  ds-wire/         # client wire CLI + boot reconcile (hooks/MCP)
  ds-core/         # cdylib/staticlib FFI
  dontspeakd/      # engine library (Caps, helper, IPC)
  dontspeak/       # multi-call CLI: MCP / notify / provide / wire / launch
```

## macOS platform

Physical Caps (`iohid.rs`) drives gestures; LED is pure output. Acquisition clears
pre-existing logical Caps. `CGEvent` posts `voice:pushToTalk`. Accessibility gates only
the caps loop — TTS/STT work without it; late trust picked up on reload.

## Hook protocol

Hooks read session JSON from stdin, talk to engine over the socket, never synthesize.
`dontspeak notify` (fire-and-forget) vs `dontspeak provide` (query). Event table:
[../docs/HOOKS.md](../docs/HOOKS.md).

`wire claude` merges only DontSpeak's `hooks` in `settings.json`.
DontSpeak settings stay in `config.toml` via MCP `set_config`.

## Build / test

```sh
cargo build --release
cargo test
```

Comment style: [../AGENTS.md](../AGENTS.md) § Code comments. Hosting:
[../docs/BUILD-DEPLOY.md](../docs/BUILD-DEPLOY.md).

## Synthesis

Dynamic `ort` (`ORT_DYLIB_PATH`); shared by built-in ORT TTS + Parakeet. Kokoro uses
`voice-g2p` plus BART ONNX for English and eSpeak for Spanish/French/Hindi/Italian/
Portuguese. MLX Kokoro still uses the same Rust frontend assets. Other models use plain-text chunks.
Playback: `rodio` 24 kHz mono; the warm `ds-helper` stops it in-process on `stop`/`stopfade`.
