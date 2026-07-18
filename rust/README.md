# dontspeak — Rust workspace

In-process Kokoro TTS (`ort` + `voice-g2p` + `rodio`); no Python runtime.
`dontspeakd::engine_run` is a library each OS app hosts via `ds-core` C ABI — no
standalone daemon. Hooks and MCP are the `dontspeak` binary over `dontspeak.sock`
(NDJSON); they never load models. Design: [../ARCHITECTURE.md](../ARCHITECTURE.md).

## Status

| Target | `ds-platform` | CI |
| --- | --- | --- |
| macOS Apple Silicon | IOKit, CGEventPost, NSWorkspace | `macos-26` |
| Windows | GetKeyState / SendInput / GetForegroundWindow | `windows-2025` |
| Linux | evdev + uinput + x11rb (Wayland degraded) | `ubuntu-latest` |

## Crates

```
rust/crates/
  ds-config/       # paths, config.toml, client wiring, config enums
  ds-client/       # ClientSource enum (leaf: wiring, hooks, IPC, MCP, logs)
  ds-log/          # unified activity log
  ds-earcon/       # OS sound introspection + cue resolution
  ds-ipc/          # NDJSON RPC (server=engine; fuzz workspace under fuzz/)
  ds-proc/         # pidfile + process-group kill
  ds-platform/     # KeyInjector / FrontmostWindow / CapsKeyMonitor per OS
  ds-http/         # bounded blocking HTTP + native trust roots
  ds-agent-usage/  # read-only weekly/monthly coding-agent quotas
  ds-model/        # download + checksum; ORT session; FluidAudio shim loader
  ds-voices/       # voice/language enum (no full synth stack)
  ds-tts/          # Kokoro TTS (Tts trait)
  ds-stt/          # Parakeet / ClaudeNative / SystemStt
  ds-aec/          # echo-cancelled duplex (VPIO / WASAPI / Pulse)
  ds-helper/       # warm TTS+STT child (one-shot + --serve)
  ds-helper-proto/ # stdout reply tokens
  ds-engines/      # STT factory
  ds-tools/        # MCP tool catalog
  ds-i18n/         # UI strings (locales/en.yml)
  ds-status/       # model_status contract
  ds-narrate/      # streaming narration core
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

`wire claude_code` merges only `hooks` + `preferredNotifChannel` in `settings.json`.
DontSpeak settings stay in `config.toml` via MCP `set_config`.

## Build / test

```sh
cargo build --release
cargo test
```

Comment style: [../AGENTS.md](../AGENTS.md) § Code comments. Hosting:
[../docs/BUILD-DEPLOY.md](../docs/BUILD-DEPLOY.md).

## Synthesis

Dynamic `ort` (`ORT_DYLIB_PATH`); shared by Kokoro + Parakeet. G2P: released
`voice-g2p` (eSpeak fallback disabled; BART ONNX for misses — Core ML path needs ORT
dylib too). OOV phonemes dropped with warning. Batches ≤ 509 IPA chars. Playback:
`rodio` 24 kHz mono. `ds-helper` process group for barge-in/pidfile takeover.
