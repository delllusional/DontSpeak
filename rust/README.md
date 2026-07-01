# dontspeak — Rust workspace (shipped)

The all-Rust DontSpeak: synthesis is **in-process native Kokoro** (`ort` + `voice-g2p`
+ `rodio`), no Python at runtime. `../apps/macos/bundle.sh` builds the binaries here and
assembles **`DontSpeak.app`**, which **hosts the engine in-process** by linking the
`ds-core` C-ABI staticlib. The Claude Code hooks `exec` the merged `dontspeak` binary
(`dontspeak notify` / `dontspeak provide`).

Under the **single-process model** (see `../ARCHITECTURE.md`), the engine —
`dontspeakd`'s `engine_run` (Caps loop, TTS queue, RPC server, hot-reload, behind the
`caps_enabled` toggle + the `stt_engine` / `tts_engine` path selectors) — runs INSIDE the
app via `ds_engine_start()`, so all OS permissions land on the one signed app. The
`dontspeakd` binary is the **headless host** for Linux/CLI; future Win/Linux apps link the
cdylib and call the same FFI. The hooks and the MCP server are the one merged `dontspeak`
binary (the MCP server when run with no args) — both **clients** that talk to the engine
over a Unix-domain socket (`dontspeak.sock` in DontSpeak's data dir, NDJSON) and never load a
model.

## Status

| Target | dontspeakd platform impl | Built + tested in CI |
| --- | --- | --- |
| macOS (Apple Silicon) | IOKit lock-state FFI, core-graphics CGEventPost, NSWorkspace | **YES — `macos-26`** |
| Windows | `windows` crate: GetKeyState / SendInput / GetForegroundWindow | **YES — `windows-2025`** |
| Linux | evdev (LED) + uinput + x11rb (Wayland degraded) | **YES — `ubuntu-latest`** |

`dontspeak` (the hook executor + MCP server) and all shared library crates compile and run
on all three OSes. The Windows/Linux platform impls are complete real-API backends behind
`cfg`; the CI release matrix builds and tests them on `windows-2025` and `ubuntu-latest`
(macOS is the local build host below).

Build host: cargo (Homebrew), `aarch64-apple-darwin`. The full OS matrix runs in CI.

## Architecture

```
rust/
  Cargo.toml                 # workspace, pinned dep versions, release profile (lto, cgu=1)
  Cargo.lock                 # gitignored (a workspace of binaries; not committed)
  README.md
  crates/
    ds-config/         # lib: paths (data dir, pidfile, socket) + config.toml + the
                             #      ~/.claude/settings.json read/write (Claude Code hooks +
                             #      its `voice` block) + the config enums + changes_since diff
    ds-ipc/            # lib: ndjson RPC over the data-dir dontspeak.sock (protocol +
      src/protocol.rs        #      blocking server + client). Engine = server; app/hooks = clients.
      src/server.rs
      src/client.rs
    ds-proc/           # lib: pidfile single-speaker (atomic tempfile) + pgroup kill
    ds-platform/       # lib: KeyInjector / FrontmostWindow / CapsKeyMonitor traits
      build.rs               #      links IOKit + ApplicationServices on macOS
      src/macos.rs           #      CGEvent (key tap) + NSWorkspace + AX
      src/macos/             #      iohid.rs (physical Caps via IOHIDManager) + iokit.rs (LED write)
      src/windows.rs         #      cfg, SendInput/GetForegroundWindow (CI: windows-2025)
      src/linux.rs           #      cfg, evdev/uinput/x11rb (CI: ubuntu-latest)
    ds-model/          # lib: download + checksum-verify Kokoro/onnx/Parakeet assets
    ds-tts/            # lib + bin: native Kokoro pipeline + the ds-helper bin
      src/bin/ds_helper/   # one-shot (cold) + `--serve` (warm, JSON protocol) modes
    ds-stt/            # lib: STT engines — streaming FastConformer Parakeet (ds-stt::streaming
                             #      over the shared ort; macOS Core ML/ANE) + capture, ClaudeNative
                             #      (CC pushToTalk), SystemStt
    ds-aec/            # lib: the echo-cancelled duplex-audio primitive (macOS VPIO;
                             #      Windows WASAPI; stub elsewhere) for full-duplex coexist
    ds-engines/        # lib: make_stt / make_tts engine factories (config → boxed engine)
    ds-tools/          # lib: the MCP tool catalog (single source for MCP + app Tools view)
    ds-i18n/           # lib: the shared UI string catalog (locales/en.yml) over the FFI
    ds-status/         # lib: the model_status engine→UI contract (serde source of truth)
    ds-core/           # lib: cdylib/staticlib FFI for the SwiftUI app; engine-client calls
    dontspeakd/               # bin+lib: the engine (caps loop, warm TTS+STT helper, IPC server)
    dontspeak/                # bin: the one multi-call client — no args = stdio MCP server;
                             #      `notify` = command hook sink; `provide` = query hook;
                             #      `wire <client>` per-client installer. Stdio only.
```

The macOS GUI is the native SwiftUI app in `../apps/macos/` (not a Rust crate); it links the
`ds-core` FFI staticlib. For the cross-cutting roles of each crate (engine, hooks,
FFI surface, pluggable engines) see [../ARCHITECTURE.md](../ARCHITECTURE.md); the tree
comments above are the quick reference and the rest of this file covers
workspace-specific build/impl detail.

## macOS platform impl (`crates/ds-platform/src/macos.rs` + `macos/`)

- **Caps-Lock state.** One read + one write:
  - **Physical key** (`iohid.rs`, via `IOHIDManager`): the down/up of the actual Caps key,
    published to an `AtomicBool` from a dedicated run-loop thread. This is the **gesture
    source** — the engine derives the whole tap / long-press machine from these edges
    (covered by the **Accessibility** grant, which subsumes Input Monitoring). There is no
    latch/lock-state read: an earlier `caps_lock_on()` poll (via `CGEventSourceFlagsState`)
    drove a latch-mirror model that has been removed.
  - **LED write** (`macos.rs` `set_caps_lock`): the Caps LED is a pure **output** the engine
    drives on each gesture edge (lit = recording), never read back. `led.rs`'s HID writer
    lights the LED on every keyboard; `iokit.rs`'s lock-coupled `IOHIDSetModifierLockState` is
    the fallback and drives the LED off on the long-press reset. IOKit is linked in `build.rs`.
- **Dictation key tap** (`macos.rs` `tap_key`, via the `core-graphics` crate): a
  `CGEvent::new_keyboard_event` for the configured `voice:pushToTalk` chord (default `Space`,
  modifiers from the `KeyChord`) on the `HIDSystemState` source, posted with
  `event.post(CGEventTapLocation::Session)` with a ~24 ms down→up hold. One tap toggles Claude
  Code's voice recording (the `claude_code` STT path).
- **Frontmost app** (`macos.rs`, via `objc2-app-kit`):
  `NSWorkspace::sharedWorkspace().frontmostApplication().bundleIdentifier()`, matched against
  the terminal bundle-id set (e.g. `com.googlecode.iterm2`, `com.apple.Terminal`,
  `com.mitchellh.ghostty`).
- **Accessibility gate**: `AXIsProcessTrusted()` (from ApplicationServices, also linked in
  `build.rs`) — read-only, no prompt. Denial is **non-fatal**: the engine still runs (STT/TTS
  work without AX); only the caps loop self-gates on it, re-probing each reload so granting
  trust later is picked up without a restart.

## Hook protocol

Every voice hook reads one hook JSON object from **stdin** (typed serde) for its ambient
`session_id` and talks to the warm engine over the socket. None of them synthesize — the
engine owns playback — and all are best-effort: engine down ⇒ no-op, never blocking Claude.
The two entries are split by CONTRACT, not by event: `dontspeak notify` (command sink, replies
nothing, wired on every fire-and-forget event) and `dontspeak provide` (query, returns the
event's `hookSpecificOutput`). Both route internally on `hook_event_name`. See
`../claude/hooks/HOOKS-README.md` for the full event→verb table.

### settings.json wiring

Exec-form hooks (the binary in `command`, the subcommand in `args`), wired by `dontspeak
wire claude_code` — the single cross-platform definition + safe merge in `ds-config`
(`merge_hooks`/`strip_hooks`). `../claude/settings.snippet.json` mirrors what it writes;
abbreviated:

```jsonc
{
  "hooks": {
    // every fire-and-forget event is the SAME `notify` command (it routes on hook_event_name)
    "MessageDisplay":   [{ "hooks": [{ "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true, "timeout": 10 }] }],
    "SessionStart":     [{ "hooks": [{ "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true }] }],
    "SessionEnd":       [{ "hooks": [{ "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true }] }],
    "Stop":             [{ "hooks": [{ "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true }] }],
    "Notification":     [{ "hooks": [{ "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true }] }],
    "UserPromptSubmit": [{ "hooks": [
        { "type": "command", "command": "~/.local/bin/dontspeak", "args": ["notify"], "async": true, "timeout": 5 },
        { "type": "command", "command": "~/.local/bin/dontspeak", "args": ["provide"], "timeout": 5 }] }]
  },
  "voice": { "enabled": true, "mode": "tap" },   // CLAUDE CODE's own block (not ours)
  "dontspeak": {                                   // OUR block
    // voice pool: [0] is the default/current voice; the rest are assigned per-terminal
    "voices": ["af_sarah", "af_bella", "af_nicole"],
    // STT path selector: off | built_in (local Parakeet, DEFAULT) | system | claude_code
    //   (claude_code delegates to Claude Code's own voice dictation key)
    "stt_engine": "built_in",
    // TTS path selector: off | built_in (local Kokoro, DEFAULT) | system
    //   ("built_in" is the config token; "kokoro" is its brand/voice-family name)
    "tts_engine": "built_in",
    "caps_enabled": true
  }
}
```

## Build / test / run

```sh
cargo build --release            # all binaries, lto + codegen-units=1
cargo test                       # whole workspace
```

On macOS the engine runs in-process inside `DontSpeak.app` (`../apps/macos/bundle.sh`);
Caps-Lock dictation needs the Accessibility grant (TTS/STT work without it). See
[../README.md](../README.md) for install + the smoke test, and `../docs/BUILD-DEPLOY.md` for
which code change deploys by which route (the stale-helper trap).

## Synthesis pipeline (in-process native Kokoro — shipped)

Synthesis is **fully in-process**; there is no runtime Python call anywhere. Key choices:

- **ONNX inference** via [`ort`](https://crates.io/crates/ort) `=2.0.0-rc.12` with the
  **`load-dynamic`** strategy (`default-features = false`, `features = ["load-dynamic",
  "api-24", "coreml", "cuda"]`): onnxruntime is not baked into the binary; `libonnxruntime`
  is resolved at **runtime** via `ORT_DYLIB_PATH`. (`api-24` works around pykeio/ort#547,
  where rc.12's Vitis EP registration references an `ort-sys` field gated behind a higher API
  level than `load-dynamic` alone pulls in.) The same runtime is shared by Kokoro TTS and
  Parakeet STT.
- **English G2P** via [`voice-g2p`](https://crates.io/crates/voice-g2p) `0.2.2` (pure-Rust
  misaki port, 90k-gold + 93k-silver dict embedded), **English-only, espeak-free**:
  out-of-dictionary words degrade silently rather than aborting. NOTE: misaki ≠ espeak, so
  tokens are **not** byte-identical to an espeak path — owner-accepted as functional English.
- **Playback** via [`rodio`](https://crates.io/crates/rodio) `0.22`, streaming 24 kHz mono PCM
  per phoneme batch.
- **Assets download** (`ds-model`: attohttpc + sha2 pinned-checksum + atomic rename +
  `directories` data dir), not bundled: `kokoro-v1.0.onnx` (~310 MB), `voices-v1.0.bin`
  (~28 MB), and a version-matched `libonnxruntime` — route A (default): download the prebuilt
  ONNX Runtime 1.27.0 `.tgz` (pinned SHA-256), extract the dylib; route B (fallback): `ort`'s
  `download-binaries` feature (bakes the lib at build time, heavier binary).
- **The pidfile / barge-in contract.** In-process audio can't hand back a child pgid, so the
  thin `ds-helper` does synth + playback in its OWN process group (`setsid`); hooks
  record its pgid, so `killpg` barge-in and the narrate pidfile-takeover work as designed.

## Notes / risks

- Windows/Linux platform impls are complete real-API backends, built and tested in CI
  (`windows-2025` / `ubuntu-latest`): Windows GetForegroundWindow→image-name match and Linux
  evdev/uinput wiring. macOS is the local build host; the other two run in the CI matrix.
</content>
</invoke>
