# Build & deploy

Three runtime pieces, three deploy routes — wrong rebuild = stale code that *looks*
installed.

## Pieces per OS

### macOS

| Piece | Built+installed by | Running app uses |
|---|---|---|
| `dontspeak` (CLI / MCP / hooks) | `install-engine.sh` → `~/.local/bin/dontspeak` | that path (live after install) |
| `ds-helper` | `install-engine.sh` **and** `bundle.sh` → app bundle | **bundled** `Contents/MacOS/ds-helper` |
| engine (`dontspeakd`) | `bundle.sh` → linked into app | **app binary** |

Use `build-macos` skill.

### Windows

| Piece | Built+installed by | Running app uses |
|---|---|---|
| `dontspeak.exe` | `build-portable.ps1` → `install.ps1` → `%LOCALAPPDATA%\Programs\DontSpeak\` | installed `dontspeak.exe` |
| `ds-helper.exe` | `build-portable.ps1` next to `ds-winui.exe` | install-dir copy |
| engine + `ds_core.dll` | `dotnet publish` WinUI | `ds-winui.exe` + extracted DLL |

Use `build-windows` flow 2 (local archive + `scripts/install/web/install.ps1`) so
registration matches release.

### Linux

| Piece | Built+installed by | Running app uses |
|---|---|---|
| `dontspeak` | `scripts/install/local/install.sh` → `~/.local/bin/dontspeak` | that path |
| `ds-helper` | same install.sh | `~/.local/bin/ds-helper` |
| engine | `apps/linux/install-gui.sh` → `~/.local/bin/ds-gtk` | `ds-gtk` |

Use `build-linux` flow 2 (`install.sh` then `install-gui.sh`).

## Which route for which change

- **Hook / MCP surface only** (notify/provide routing, tools, config the hook reads):
  CLI rebuild is enough. macOS `install-engine.sh`; Windows portable install; Linux
  `install.sh`. Re-wire only if the hook set changed. **Exception:** IPC schema
  change → lockstep below.
- **Engine or helper** (`dontspeakd`, `ds-tts`/`ds-stt`, queue/synth, IPC handlers):
  deploy the changed runtime and relaunch its host. macOS: `bundle.sh` for both, or
  the helper-only copy + codesign below. Windows: `build-portable.ps1`. Linux:
  `apps/linux/install-gui.sh` for the engine; `scripts/install/local/install.sh` for
  the helper.
- **Wiring shapers** (`ds-config::wire::*`, `ds-wire`): host app also links this and
  runs `ds_wire::reconcile` at boot — stale host rewrites old wiring. Rebuild host
  too, not just CLI.

### CLI + engine lockstep (unversioned wire protocol)

`ds-ipc` `Request` is strict both ways: unknown or missing required fields error. No
negotiated version. CLI and engine are one deployable despite two install routes.

Example: every request requires a `source` field containing a wired-agent token or
`null`. Partial reinstall → engine rejects greet/speak/etc. with
``missing field `source` ``. Hooks discard the
reply and exit 0 — voice goes quiet with no terminal error. Activity log:

```
WARN engine rejected request (cmd=greet_session): missing field `source` …
```

Fix: redeploy **both** CLI and host (`build-*` skill), relaunch app.

### Fast iteration (manual copy)

**macOS** — re-sign or the app SIGKILLs the helper:

```sh
cargo build --release -p ds-helper --manifest-path rust/Cargo.toml
osascript -e 'quit app "DontSpeak"'; pkill -9 -f dontspeak
cp rust/target/release/ds-helper "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"
codesign --force --sign - "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"
open "$HOME/Applications/DontSpeak.app"
```

**Windows** — stop processes first; `ds-core` needs `release-ffi`:

```powershell
cargo build --release -p ds-helper --manifest-path rust\Cargo.toml
cargo build --profile release-ffi -p ds-core --manifest-path rust\Cargo.toml
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

**Linux:** `scripts/install/local/install.sh` rebuilds and installs `ds-helper`;
`apps/linux/install-gui.sh` rebuilds only the host.

### Symptom → diagnosis

Voice fully quiet after partial reinstall → check activity log for `rejected request`
before anything else.

"Fix has no effect" on synth/queue/IPC while binaries look updated → stale helper or
engine. Probe functionally (release bins are stripped — `strings` false-negatives).
e.g. long `speak` + `ds-helper.log` for `phonemeSequenceTooLong`:

| OS | `ds-helper.log` |
|---|---|
| macOS | `~/Library/Logs/DontSpeak/ds-helper.log` |
| Windows | `%LOCALAPPDATA%\DontSpeak\logs\ds-helper.log` |
| Linux | `${XDG_STATE_HOME:-~/.local/state}/dontspeak/logs/ds-helper.log` |

## Debugging hooks

Missing hook field: log raw stdin at top of `notify` in `main.rs` before
`hook_core::notify`, reinstall, trigger. `PostToolUse` carries rich payload
(`tool_name`, `tool_input`, …). Hook process loads `VoiceConfig` from
`Paths::resolve().config_toml`:

| OS | `config.toml` |
|---|---|
| macOS | `~/Library/Application Support/DontSpeak/config.toml` |
| Windows | `%APPDATA%\DontSpeak\config.toml` |
| Linux | `${XDG_CONFIG_HOME:-~/.config}/dontspeak/config.toml` |

## Config defaults can mask deploy bugs

`narrate` and `greet` default on; many opt-ins default off. Wrong config path
→ default-on still "works", opt-in stays silent. Log `cfg.<field>` and
`paths.config_toml` before debugging feature logic.

## TTS frontend (Kokoro vs plain-text)

Kokoro only: `ds_tts::g2p::phoneme_batches_for` before backend selection — GFM prose,
English number expansion, G2P once, drop OOV phonemes (warn), batches ≤ 509 IPA chars.
Empty list = successful no-op. Cap is 509 (not 510) because style table rows are
`0..=509` by token count. Both ONNX and MLX Kokoro consume those IPA batches.

Chatterbox / Qwen / OmniVoice: shared markdown→prose then plain-text chunks
(`ds_tts::chatterbox::frontend`); model-side tokenization stays in each pipeline.
See [TTS-PIPELINE.md](TTS-PIPELINE.md).
