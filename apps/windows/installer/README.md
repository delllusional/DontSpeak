# DontSpeak Windows installer

A small `setup.exe` (~13 MB) that installs the **minimal, framework-dependent** WinUI
app and brings up everything it needs.

## What the installer does
- Installs the app (~58 MB on disk) to `%ProgramFiles%\DontSpeak` — the engine
  `ds_core.dll`, the warm-synth `ds-helper.exe`, the `dontspeak.exe`
  MCP server / hook executor, and `ds-winui.exe`.
- **Ensures the runtimes, only if missing** (standard `Check:` gating — never reinstalls):
  - **.NET 10 Desktop Runtime** — folder probe under `…\dotnet\shared\Microsoft.WindowsDesktop.App\10.*`; if absent, fetched from `aka.ms/dotnet/10.0/windowsdesktop-runtime-win-x64.exe`.
  - **Windows App Runtime 2.0** — detected via `winget list` exit code; if absent, `winget install Microsoft.WindowsAppRuntime.2.0`.
- **Optional downloads** — a standard **Components** tree where **ONNX is the parent**
  and each model + CUDA is a child that pulls ONNX in when ticked:
  ```
  [x] ONNX runtime  (required by every model, ~16 MB)
      [x] Kokoro — text-to-speech (~330 MB)
      [x] Parakeet — speech-to-text (~660 MB)
      [ ] CUDA GPU acceleration — NVIDIA only (~1.4 GB)   ← selectable under ONNX
  ```
  Setup types: *Recommended* (models, CPU) / *Full* (+ CUDA) / *App only* (nothing) /
  *Custom*.
- **Real download progress** — on the Ready page (`NextButtonClick`/`wpReady`) the
  installer extracts the helper to `{tmp}`, asks it `--print-manifest <component>`
  (which writes the **still-needed** URL+SHA list from `ds-model` to a file — already
  present, sha-valid assets are skipped, so re-runs download nothing), and feeds those
  into Inno's built-in **`TDownloadWizardPage`** → a real progress bar with bytes/speed,
  separate from the file-copy step. After download, `[Run]` calls
  `ds-helper --install-prefetched {tmp} <component>` (run as the original user) to
  verify + place/extract the files — no second download. If the download page is skipped
  or fails, `--install-prefetched` falls back to a normal fetch.
  **The installer never hardcodes a model/CUDA/ONNX URL or size** — they come from the
  helper → `ds-model` at runtime, so they can't drift. A `ds-model` unit test asserts the
  per-URL basenames (the prefetch keying) stay unique.
- On the final **Select Additional Tasks** page: a **desktop shortcut** and **"Start
  DontSpeak when I sign in"** (the latter writes the same HKCU `Run` value the in-app
  "Start at login" toggle uses, so they stay in sync). Plus a Start-menu shortcut
  (brand icon) and best-effort `claude mcp add --scope user dontspeak …` (as the real user).

## Build it
Prereqs (one-time on the build machine):
- Rust (MSVC), the repo's `~/.dotnet` .NET 10 SDK, and **Inno Setup 6**
  (`winget install -e --id JRSoftware.InnoSetup`).

```powershell
pwsh windows/installer/build.ps1
# → windows/installer/Output/ds-setup.exe
```

`build.ps1` does: `cargo build --release` (core + helper + mcp) → `dotnet publish`
the WinUI app framework-dependent (the `StripUnusedWindowsAI` csproj target trims the
unused Windows-ML DLLs) → stage the payload → `ISCC dontspeak.iss`.

The `payload/` and `Output/` folders are build artifacts (git-ignored).

The modern wizard's branded **Welcome** page uses committed BMPs (`wizard-large*.bmp`,
`wizard-small*.bmp`, two sizes each for high-DPI). Regenerate with
`python windows/installer/gen_wizard_images.py` (needs **ImageMagick** on PATH): it
rasterizes the **real** app glyph from `apps/macos/AppIcon.icon/Assets/Foreground.svg` and
reads the gradient straight from `assets/icon.svg`, so the images can't drift from the
app icon. The intro text is in the `.iss` `[Messages]` `WelcomeLabel1/2`.

## Notes
- `PrivilegesRequired=admin` (Program Files + machine-wide runtime installs). The
  prefetch + MCP registration run with `runasoriginaluser` so the models land in the
  *real* user's `%APPDATA%\dontspeak` and the MCP registers for that user.
- winget (App Installer) must be present for the runtime auto-install. On a machine
  that already has .NET 10 Desktop + Windows App Runtime 2.0, both prereq steps are skipped.
- The prerequisite versions live as **winget IDs** (Microsoft maintains the mapping),
  not hardcoded download URLs.
