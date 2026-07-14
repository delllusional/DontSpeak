# Build & deploy — what a code change actually deploys to

Each of the three OS hosts **hosts the engine in-process** and spawns a **warm
child** for synthesis, so one repo has three runtime pieces per OS that deploy by
three different routes. Rebuilding the wrong one leaves the app running stale code
that *looks* installed — always check which route a change needs before concluding
it works. The three pieces are the same shape on every OS; only the build/install
mechanics differ, so pick your OS's table below.

## The three pieces and how each reaches the running app

### macOS

| Piece | What it is | Built+installed by | What the RUNNING APP actually executes |
|---|---|---|---|
| `dontspeak` | the CLI: client launcher, MCP server, and hook entries (`notify`/`provide`) | `install-daemon.sh` → `~/.local/bin/dontspeak` | **`~/.local/bin/dontspeak`** — launch commands and wired hooks execute this copy, so this IS live after install-daemon |
| `ds-helper` | the warm TTS/STT synthesis child | `install-daemon.sh` → `~/.local/bin/ds-helper` **AND** `bundle.sh` → `DontSpeak.app/Contents/MacOS/ds-helper` | **the BUNDLED copy** — the app spawns `Contents/MacOS/ds-helper`; the `~/.local/bin` copy is not what the app uses |
| engine (`dontspeakd` logic) | the in-process engine (queue, IPC, playback, `ds_wire::reconcile` at boot) | `bundle.sh` → linked into the `DontSpeak.app` binary | **the app binary** — the engine is linked in and runs in-process |

Use the `build-macos` skill rather than hand-rolling `install-daemon.sh`/`bundle.sh`.

### Windows

| Piece | What it is | Built+installed by | What the RUNNING APP actually executes |
|---|---|---|---|
| `dontspeak.exe` | the CLI: client launcher, MCP server, and hook entries | `build-portable.ps1` → extracted into `%LOCALAPPDATA%\Programs\DontSpeak\dontspeak.exe` | **the extracted `dontspeak.exe`** — launch commands and wired hooks execute this copy, so a re-extract is live immediately |
| `ds-helper.exe` | the warm TTS/STT synthesis child | `build-portable.ps1` (`dotnet publish` output dir), extracted alongside `ds-winui.exe` | **the extracted copy** — `ds-winui.exe` spawns it from its own install dir |
| engine (`dontspeakd` logic) + `ds_core.dll` | the in-process engine, hosted via P/Invoke, `ds_wire::reconcile` at boot | `build-portable.ps1` (`dotnet publish` of `DontSpeak.WinUI.csproj`) → `ds-winui.exe` + `ds_core.dll` | **`ds-winui.exe`** with the extracted `ds_core.dll` — the engine is linked in and runs in-process |

Use the `build-windows` skill's flow 2 (build + extract over the per-user install
dir + `dontspeak.exe wire --reconcile` + relaunch `ds-winui.exe`) rather than hand-
rolling the extract/copy steps.

### Linux

| Piece | What it is | Built+installed by | What the RUNNING APP actually executes |
|---|---|---|---|
| `dontspeak` | the CLI: client launcher, MCP server, and hook entries | `scripts/install.sh` → `~/.local/bin/dontspeak` | **`~/.local/bin/dontspeak`** — launch commands and wired hooks execute this copy, so this IS live after `install.sh` |
| `ds-helper` | the warm TTS/STT synthesis child | `scripts/install.sh` → `~/.local/bin/ds-helper` | **the installed copy** — spawned from `~/.local/bin` |
| engine (`dontspeakd` logic) | the in-process engine (queue, IPC, playback, `ds_wire::reconcile` at boot) | `apps/linux/install-gui.sh` (`cargo build --release` in `apps/linux/gtk`) → `~/.local/bin/ds-gtk` | **`~/.local/bin/ds-gtk`** — the engine is linked in and runs in-process |

Use the `build-linux` skill (flow 2: `scripts/install.sh` then
`apps/linux/install-gui.sh`) rather than hand-rolling the two installs.

## The rule

- **Hook / MCP-surface change** (the `notify`/`provide` hook routing, `mcp`/`tools`,
  config parsing read by the hook): the CLI-only rebuild is enough — hooks invoke
  the installed `dontspeak`/`dontspeak.exe` fresh each time, so it's live
  immediately (re-run `wire claude_code` only if the hook set changed). macOS:
  `install-daemon.sh`. Windows: extract step of `build-portable.ps1`. Linux:
  `scripts/install.sh`. **Unless the change touches the IPC wire protocol** — see the
  lockstep rule below.
- **Engine or helper change** (`dontspeakd`, `ds-tts`/`ds-stt`, the TTS
  queue/synth/chunking, IPC handlers): rebuild the OS host app, then relaunch it.
  The CLI-only rebuild does not update the bundled helper or the in-process engine.
  macOS: `./apps/macos/bundle.sh`. Windows: `build-portable.ps1` (or the manual
  `ds_core.dll`/`ds-helper.exe` copy below). Linux: `apps/linux/install-gui.sh`.
- **Wiring-shaper change** (`ds-config::wire::*`, `ds-wire` — the code that decides
  *what* gets written to each client's hook/MCP config): rebuilding just the CLI is
  **not** enough, even though the CLI is what writes the wiring. The host app links
  the same wiring code and calls `ds_wire::reconcile` at boot (and again on config
  change) — a stale host app rewrites the OLD wiring at its next launch and silently
  reverts the fix. Rebuild the host app too (same route as an engine change, above),
  not just the CLI.

### The CLI and the engine deploy TOGETHER — the wire protocol is not versioned

`ds-ipc`'s `Request` schema is **strict in both directions**: an unknown field is an
error, and a *missing* required field is an error. There is no negotiated version, no
`#[serde(default)]` escape hatch, and backward compatibility across a skew is
explicitly not a goal. So the CLI (`~/.local/bin/dontspeak`, which is what every hook
and the MCP server execute) and the engine (linked into the running host app) are ONE
deployable, even though they reach the machine by two different routes above.

The live example: every client-originated request carries a **required
`source: ClientSource`** naming which client sent it (the hook's `--client <token>`
verb; the MCP `initialize` handshake's `clientInfo`). Reinstall only the CLI and the
old app keeps running an engine that has never heard of `source` — or, the way it
actually bites, rebuild only the app and the stale CLI keeps sending lines without one.
The engine then rejects **every** greet / mark_active / session_end / stop_speech /
speak / narration / earcon with ``bad request: missing field `source` ``.

**How you'd notice** — and the reason this section exists: you mostly *wouldn't*. The
hooks discard the engine's reply and exit 0, so nothing appears at the terminal; the
voice loop just goes quiet with no error anywhere the user looks. The one diagnostic is
engine-side, in the activity log (the app's Logs tab):

```
WARN engine rejected request (cmd=greet_session): missing field `source` … — caller and engine are out of sync; reinstall the CLI and restart the app (docs/BUILD-DEPLOY.md)
```

Seeing that line means exactly one thing: **rebuild and redeploy both pieces** (the
per-OS `build-*` skill does both), then relaunch the app. Treat "voice silently stopped
working after I reinstalled one piece" as this until the log says otherwise.

For fast iteration, a manual copy-and-relaunch can stand in for a full host rebuild:

**macOS** — the copied binary must be re-signed or the app SIGKILLs it:
```sh
cargo build --release -p ds-helper --manifest-path rust/Cargo.toml
osascript -e 'quit app "DontSpeak"'; pkill -9 -f dontspeak
cp rust/target/release/ds-helper "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"
codesign --force --sign - "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"  # required — a copied binary is SIGKILLed until re-signed
open "$HOME/Applications/DontSpeak.app"
```

**Windows** — stop processes before copying, or the copy fails with a file-in-use error.
`ds-core` (the FFI cdylib) must build under the `release-ffi` profile, not plain
`release` — see `DontSpeak.WinUI.csproj`'s comment on `CargoFfiOutDir`:
```powershell
cargo build --release -p ds-helper --manifest-path rust\Cargo.toml           # helper
cargo build --profile release-ffi -p ds-core --manifest-path rust\Cargo.toml # engine/FFI surface
$dest = Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak'
$destPrefix = [IO.Path]::GetFullPath($dest).TrimEnd('\') + '\'
$installed = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
  try { $_.Path -and [IO.Path]::GetFullPath($_.Path).StartsWith($destPrefix, [StringComparison]::OrdinalIgnoreCase) }
  catch { $false }
})
if ($installed) {
  $installed | Stop-Process -Force -ErrorAction Stop
  $installed | Wait-Process -ErrorAction Stop
}
Copy-Item rust\target\release\ds-helper.exe "$dest\ds-helper.exe" -Force
Copy-Item rust\target\release-ffi\ds_core.dll "$dest\ds_core.dll" -Force
Start-Process "$dest\ds-winui.exe"
```

**Linux** — `install-gui.sh` already IS the fast path (release build + install in one step):
```sh
apps/linux/install-gui.sh
```

### Symptom → diagnosis

Voice gone *entirely* quiet (no greet, no speech, no earcons) right after a partial
reinstall is the CLI/engine skew above — check the activity log for a `rejected request`
WARN before debugging anything else.

A source fix that "has no effect" on synthesis/queue/IPC while the binary and tests
are clearly updated means the app is running its stale bundled helper or engine.
Confirm with a functional probe rather than `strings` (release binaries are stripped,
so grepping for a symbol gives false negatives) — e.g. fire a long `speak` and check
the per-OS `ds-helper.log` (a sibling of the main activity log, same directory) for
`phonemeSequenceTooLong` to catch a stale helper:

| OS | `ds-helper.log` |
|---|---|
| macOS | `~/Library/Logs/DontSpeak/ds-helper.log` |
| Windows | `%LOCALAPPDATA%\DontSpeak\logs\ds-helper.log` |
| Linux | `${XDG_STATE_HOME:-~/.local/state}/dontspeak/logs/ds-helper.log` |

## Debugging Claude Code hooks

When a hook payload seems to be missing a field, capture the raw event instead of
guessing the schema — temporarily log it at the top of the `notify` entry in
`main.rs`, before `hook_core::notify(...)` runs, then install and trigger a tool.

Ground truths from doing this: `PostToolUse` fires with a rich payload —
`{ hook_event_name, session_id, tool_name, tool_input{...}, tool_response{...},
permission_mode, cwd, ... }` (for Bash, `tool_input.description` is the best spoken
cue). The hook itself is a fresh short-lived process that loads `VoiceConfig` from
`Paths::resolve().config_toml`, i.e.:

| OS | `config.toml` |
|---|---|
| macOS | `~/Library/Application Support/DontSpeak/config.toml` (the live directory is `DontSpeak/`, not the older `org.dontspeak.DontSpeak/`) |
| Windows | `%APPDATA%\DontSpeak\config.toml` |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/dontspeak/config.toml` |

## Config defaults can mask a deploy/read bug

`narrate` defaults to both kinds on (`["shorts", "digests"]`), and `greet_on_open`
also defaults on; most other flags (`full_duplex`, the needs-input earcon, …) default off. A config read
from the wrong path can leave default-on narration working while a default-off opt-in
stays silently off — reading as "the new feature is broken" when the real fault is
the config path or a stale deploy. When an opt-in stays silent, confirm the reader
sees it set (log `cfg.<field>` and `paths.config_toml`) before touching feature logic.

## TTS phoneme cap — both engines share one frontend

Route text through `ds_tts::g2p::phoneme_batches_for` before selecting a synthesis
backend. It parses spoken prose (as GitHub-flavored Markdown), normalizes English numbers,
runs the contextual G2P once, drops any character outside Kokoro's phoneme vocabulary (with a
warning — a stray character must not silence the reply), and returns typed batches no longer
than 509 phoneme characters. Vocabulary filtering maps each retained character to one token, so
the token count is no greater. An empty batch list means "nothing speakable", which is a
successful no-op, not a synthesis failure.
The effective limit is 509 rather than the advertised 510 because the ONNX voice-style table
has rows zero through 509 and is indexed by token count. ONNX and the FluidAudio Core ML shim
must consume those exact IPA batches; do not add a backend-local text splitter or G2P path.
