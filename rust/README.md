# dontspeak — Rust workspace

The all-Rust engine: synthesis is in-process native Kokoro (`ort` + `voice-g2p` +
`rodio`), no Python at runtime. Under the single-process model (see
[../ARCHITECTURE.md](../ARCHITECTURE.md)), `dontspeakd`'s `engine_run` is a library
that each OS app hosts in-process via the `ds-core` C ABI — there is no standalone
engine binary or daemon. The Claude Code hooks and the MCP server are both the one
merged `dontspeak` binary, talking to the hosted engine over a Unix-domain socket
(`dontspeak.sock`, NDJSON) and never loading a model themselves.

## Status

| Target | `ds-platform` impl | Built + tested in CI |
| --- | --- | --- |
| macOS (Apple Silicon) | IOKit lock-state FFI, core-graphics CGEventPost, NSWorkspace | **YES — `macos-26`** |
| Windows | `windows` crate: GetKeyState / SendInput / GetForegroundWindow | **YES — `windows-2025`** |
| Linux | evdev (LED) + uinput + x11rb (Wayland degraded) | **YES — `ubuntu-latest`** |

Build host: cargo (Homebrew), `aarch64-apple-darwin`; the full OS matrix runs in CI.

## Crates

```
rust/
  crates/
    ds-config/    # paths (data dir, pidfile, socket) + config.toml + the Claude Code
                  #   settings.json hooks/voice merge + config enums + changes_since diff
    ds-ipc/       # NDJSON RPC over dontspeak.sock — protocol + server + client
                  #   (engine is the server; app/hooks are clients). Its own
                  #   independent cargo-fuzz workspace lives at fuzz/ — see
                  #   fuzz/README.md (Unix/nightly-only, scheduled CI, not part of
                  #   this workspace's build/clippy/test)
    ds-proc/      # pidfile single-speaker (atomic tempfile) + process-group kill
    ds-platform/  # KeyInjector / FrontmostWindow / CapsKeyMonitor traits, one impl
                  #   per OS (macos.rs, windows.rs, linux.rs)
    ds-model/     # download + checksum-verify Kokoro/onnx/Parakeet assets; the shared
                  #   ORT session builder; the macOS FluidAudio shim loader
    ds-tts/       # native Kokoro TTS pipeline (the Tts trait)
    ds-stt/       # STT engines: streaming FastConformer Parakeet, ClaudeNative
                  #   (delegates to Claude Code's own push-to-talk), SystemStt
    ds-aec/       # echo-cancelled duplex-audio primitive (macOS VPIO, Windows WASAPI)
    ds-helper/    # bin: the warm native-media child process — unions ds-tts + ds-stt +
                  #   ds-aec, one-shot (cold) and --serve (warm) modes
    ds-helper-proto/ # the helper's stdout reply-token vocabulary (shared emit/parse consts)
    ds-engines/   # make_stt engine factory (config → boxed STT engine)
    ds-tools/     # the MCP tool catalog — single source for MCP and the app's Tools view
    ds-i18n/      # the shared UI string catalog (locales/en.yml), rendered over the FFI
    ds-status/    # the model_status engine→UI contract (serde source of truth)
    ds-core/      # cdylib/staticlib FFI each app links; engine-client calls
    dontspeakd/   # the engine itself (Caps loop, warm TTS+STT helper, IPC server) —
                  #   a library, hosted in-process via ds-core
    dontspeak/    # bin: the one multi-call client — no args runs the stdio MCP server;
                  #   `notify` is the command hook sink; `provide` is the query hook;
                  #   `wire <client>` is the per-client installer
```

See [../ARCHITECTURE.md](../ARCHITECTURE.md) for the cross-cutting roles (engine, hooks,
FFI surface, pluggable engines); this file is the crate-level map.

## macOS platform impl

Caps-Lock state is one read and one write, kept independent: the physical key
(`iohid.rs`, via `IOHIDManager`) publishes down/up edges that drive the engine's tap /
long-press gesture machine, gated on the Accessibility grant; the LED (`macos.rs` /
`iokit.rs`) is a pure output the engine lights on each gesture edge, never read back.
Dictation's push-to-talk tap is a `CGEvent` posted at the session level for the
configured `voice:pushToTalk` chord. `AXIsProcessTrusted()` gates only the caps loop —
TTS/STT keep working without Accessibility, and trust granted later is picked up on
the next reload.

## Hook protocol

Every voice hook reads one hook JSON object from stdin for its ambient `session_id`
and talks to the warm engine over the socket; none of them synthesize themselves — the
engine owns playback — and all are best-effort (engine down means no-op, never
blocking Claude). The two entries split by contract: `dontspeak notify` (fire-and-forget
command sink) and `dontspeak provide` (query, returns `hookSpecificOutput`), both
routing internally on `hook_event_name`. See
[../claude/hooks/HOOKS-README.md](../claude/hooks/HOOKS-README.md) for the full
event→verb table.

`dontspeak wire claude_code` writes the exec-form hooks into `settings.json` via
`ds-config`'s safe merge, touching only the `hooks` object and
`preferredNotifChannel`. DontSpeak's own settings (voice pool, engine selectors,
`caps_enabled`, …) live in `config.toml`, set via the `set_config` MCP tool —
`settings.json` stays purely Claude Code's own.

## Build / test / run

```sh
cargo build --release            # all binaries, lto + codegen-units=1
cargo test                       # whole workspace
```

On macOS the engine runs in-process inside `DontSpeak.app`
(`../apps/macos/bundle.sh`); Caps-Lock dictation needs the Accessibility grant. See
[../README.md](../README.md) for install + the smoke test, and
[../docs/BUILD-DEPLOY.md](../docs/BUILD-DEPLOY.md) for which change deploys by which
route.

## Synthesis pipeline

Synthesis is fully in-process, with no runtime Python call. Inference runs on
[`ort`](https://crates.io/crates/ort) with the `load-dynamic` strategy, so
`libonnxruntime` resolves at runtime via `ORT_DYLIB_PATH` rather than being baked into
the binary; the same runtime instance is shared by Kokoro TTS and Parakeet STT.
English G2P is [`voice-g2p`](https://crates.io/crates/voice-g2p), a pure-Rust misaki
port with an embedded dictionary — English-only and espeak-free, so out-of-dictionary
words degrade silently rather than aborting. Playback is
[`rodio`](https://crates.io/crates/rodio), streaming 24 kHz mono PCM per phoneme
batch. Model assets and a version-matched `libonnxruntime` download on first use
(`ds-model`: pinned SHA-256 checksums, atomic rename into the data dir). Because
in-process audio can't hand back a child pgid, `ds-helper` runs synth + playback in
its own process group so barge-in and pidfile-takeover still work as designed.
