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
    ds-platform/       # lib: CapsLockReader / KeyInjector / FrontmostWindow traits
      build.rs               #      links IOKit + ApplicationServices on macOS
      src/macos.rs           #      CGEvent (caps read + key tap) + NSWorkspace + AX
      src/macos/             #      iohid.rs (physical Caps via IOHIDManager) + iokit.rs (LED write)
      src/windows.rs         #      cfg, SendInput/GetKeyState/GetForegroundWindow (CI: windows-2025)
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
                             #      wire-hooks / wire-desktop installers. Stdio only.
```

The macOS GUI is the native SwiftUI app in `../apps/macos/` (not a Rust crate); it links the
`ds-core` FFI staticlib.

### Crate roles

- **ds-config** — resolves the fixed live locations (the data dir `config.toml`,
  `speak-hook.pid`, `dontspeak.sock`, the unified `~/Library/Logs/dontspeak.log`) and reads
  OUR `"dontspeak"` block from `~/.claude/settings.json` (separate from Claude Code's own
  `voice` block, which DontSpeak never writes — the `claude_code` STT engine only reads
  it); model assets resolve to the per-OS data dir via `directories` (not bundled). Owns the
  subsystem selectors (`caps_enabled`, `stt_engine`, `tts_engine`), the `ConfigChange` /
  `changes_since` diff the engine uses to reload surgically, and the IPC wire form.
- **ds-ipc** — newline-delimited-JSON RPC over the Unix-domain socket: the
  `Request`/`Response` protocol, a blocking server (the engine hosts it), and a client (the
  SwiftUI app via `ds-core`, and the hooks). One JSON request per line; streaming
  requests (download progress, STT partials) emit several lines then a terminal one. A
  missing socket means "engine down", and every call is fallible so clients no-op.
- **ds-proc** — the single-speaker contract: the pidfile holds a process-GROUP id,
  barge-in is `killpg(-pgid, SIGTERM)` on unix (Windows terminates the job leader); pidfile
  writes are atomic (tempfile + rename) so a half-written pgid is never read.
- **ds-platform** — `CapsLockReader`/`KeyInjector`/`FrontmostWindow` traits + per-OS
  impls. All three are complete real-API backends behind `cfg`, built + tested in CI.
- **dontspeakd** — the resident **engine** that owns everything model/engine behind the
  selectors, served over the `ds-ipc` socket: the Caps-Lock loop (30 ms poll that
  **mirrors the latched Caps-Lock LED** — an OFF→ON edge starts a dictation tap, ON→OFF
  stops it; the physical key via `IOHIDManager` is used only to detect a long-press reset);
  the **one warm helper child** it supervises (`ds-helper --serve`, spawned/killed by
  the `tts_engine` / `stt_engine` selectors) hosting **both** Kokoro TTS and Parakeet STT;
  test recognition; and model presence + removability. Its `reload()` is **surgical**: it
  diffs incoming config via `changes_since` and touches only what changed. Accessibility
  denial is **non-fatal** — the engine runs without AX trust and the caps loop self-gates on
  `AXIsProcessTrusted()` (re-probed each reload, so granting trust later is picked up without
  a restart).
- **dontspeak `notify`** — the **command** hook sink (SessionStart / UserPromptSubmit /
  SessionEnd / MessageDisplay / Stop / Notification). It reads the hook JSON on stdin, routes
  on `hook_event_name`, runs the side effect (greet / mark-active / narrate / barge / earcon)
  via a best-effort socket ping to the warm engine, and replies with nothing. No synthesis
  here — the engine owns playback; engine down ⇒ no-op (never blocks Claude). The
  `MessageDisplay` route is the **single narration pipeline**: it accumulates each streamed
  assistant message per `message_id` and forwards EVERY completed top-level blockquote to the
  engine's `SpeakNarration` queue — prose, the lines Claude leads each tool step with, and the
  final reply alike (no separate Stop/PostToolUse path, no final-reply dedup).
- **dontspeak `provide`** — the lone **query** hook (UserPromptSubmit): re-reads the `narrate`
  setting each turn and returns the narration spec as `hookSpecificOutput` when on. The only
  hook Claude Code blocks on.
- **ds-tts** — the TTS engines + native Kokoro pipeline: `vocab`/`voices`/`trim`,
  `g2p` (English phonemization via `voice-g2p`, espeak-free), `numbers`/`batch` (text
  normalization + chunking), `synth` (the `ort` session I/O), `play` (`rodio` streaming), and
  the `ds-helper` bin. The helper has two modes: a one-shot `ds-helper <text>
  <voice> <rate>` (the cold fallback for synthesis, own process group) and `ds-helper
  --serve`, the **warm child** the engine supervises — it loads the model once and speaks a
  JSON protocol on stdin: `speak`/`stop` for Kokoro TTS **and** `listen`/`lstop` for Parakeet
  STT (so STT dictation no longer loads a model in the engine itself). `--serve` does not
  auto-download — it fails if the model is missing.
- **ds-stt** — the STT engines + Parakeet pieces: `Capture` (mic) + the **streaming
  cache-aware FastConformer transducer** (`ds-stt::streaming` over the shared `ort`; on macOS
  the `ane` rung uses FluidAudio's `StreamingAsrManager`, `coreml.rs`). `transcribe-rs` is kept
  ONLY for its energy VAD now (not inference). Plus the `ClaudeNative` (taps Claude Code's
  `voice:pushToTalk` key — read from its keybindings.json, default `Space` — to drive CC's own
  dictation) and `SystemStt` engines. The Parakeet pieces run inside the warm helper
  (`--serve listen`) for dictation and inside the engine directly for always-listening mode.
- **ds-aec** — the platform duplex-audio primitive (`DuplexAudio`) for full-duplex
  coexist (mic open while TTS plays, with acoustic echo cancellation). macOS = a
  VoiceProcessing I/O AudioUnit (`macos.rs`); Windows = WASAPI Communications-category
  capture (`windows.rs`); `stub.rs` elsewhere. See `../docs/AEC.md` and
  `../docs/FULL-DUPLEX-PORT.md`.
- **ds-engines** — the `make_stt` / `make_tts` factories that build a boxed engine from
  config. `make_stt` first resolves the `stt_engine` ladder (`cfg.resolved_stt()` — the first
  usable rung) then maps that single engine: `claude_code` ⇒ `ClaudeNative` (taps
  Claude Code's dictation key); `built_in` ⇒ `ClaudeNative` in this helper-less factory
  (degrade-when-no-model path — the engine itself builds `built_in` as `HelperStt` through the
  warm helper); `off`/`system` ⇒ the inert `SystemStt`. Default `built_in`.
- **ds-core** — the C ABI (`cdylib`/`staticlib`) the macOS SwiftUI app links. Small and
  handle-free: engine lifecycle (`ds_engine_start` / `_stop` / `_reload`), read-only
  probes (`ds_*_present_global`, `ds_engine_running_global`,
  `ds_model_status_json`, `ds_tools_json`), the i18n FFI (`ds_t` /
  `ds_t_args` / `ds_set_locale`), and one engine command (`ds_set_provider`)
  + `ds_set_muted`. There are no voice/engine config setters — that control is in the
  MCP — and no download command: the engine fetches a missing model for any enabled engine
  automatically on first activation. Model **download** file IO lives here (worker threads);
  the engine is the authority on model presence + removability.
- **ds-model** — downloads + checksum-verifies `kokoro-v1.0.onnx`, `voices-v1.0.bin`,
  the matching `libonnxruntime` dylib (ONNX Runtime 1.27.0), and the streaming FastConformer
  Parakeet assets (`encoder.int8.onnx` / `decoder.int8.onnx` / `joiner.int8.onnx` /
  `tokens.txt`), atomic temp+rename.
- **ds-tools** — the single source of truth for the **MCP tool catalog**: one
  `catalog()` returning the `{name, description, inputSchema}` array. The MCP server
  (`dontspeak` with no args) and the app's Tools view (`ds_tools_json`) both read it, so
  the list never drifts.
- **ds-i18n** — the shared UI-string catalog (`locales/en.yml`, embedded via
  `rust-i18n`) rendered by every platform UI over the FFI. See `../docs/localization.md`.
- **dontspeak (no args)** — the stdio **MCP server** Claude Code connects to; a client of the
  engine socket. Exposes the `ds-tools` catalog and dispatches each tool over
  `ds-ipc` (`speak`/`stop_speak`/`listen`/`status`, `list_voices`/`set_voice` (set or
  clear the session voice), `set_config` (one atomic setter for the persistent settings),
  `wire`, and the diarization tools).

## macOS platform impl (`crates/ds-platform/src/macos.rs` + `macos/`)

- **Caps-Lock state.** Two reads with distinct jobs:
  - **Latched LED** (`macos.rs`, `caps_lock_on()`): the recording mirror. We bind
    `CGEventSourceFlagsState(HIDSystemState)` directly (the `core-graphics` crate exposes the
    flag bitset but not this query) and mask the `AlphaShift` (Caps-Lock lock) bit. This
    reflects the OS-latched bit set by **any** keyboard, so even a tap too fast to see as a
    key-down is caught on the next poll.
  - **Physical key** (`iohid.rs`, via `IOHIDManager`): the down/up of the actual Caps key,
    published to an `AtomicBool` from a dedicated run-loop thread. Used only to detect the
    long-press reset (covered by the **Accessibility** grant, which subsumes Input Monitoring).
  > The per-keyboard lock read `IOHIDGetModifierLockState` is deliberately NOT used for the
  > read: it never tracks toggles on some external keyboards. IOKit (`iokit.rs`) is kept only
  > for the LED **write** (`IOHIDSetModifierLockState`, driving the LED off on long-press
  > reset); the IOKit framework is linked in `build.rs`.
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
wire-hooks` — the single cross-platform definition + safe merge in `ds-config`
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
