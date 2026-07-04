# DontSpeak Windows package

DontSpeak ships on Windows as a **self-contained portable zip** — no installer, no
elevation, no runtime prerequisites. The one-command installer at
[dontspeak.org](https://dontspeak.org) (`web/install.ps1`) downloads it, extracts it to
`%LOCALAPPDATA%\Programs\DontSpeak`, wires the MCP server + voice hooks
(`dontspeak wire --all`), adds a Start-menu shortcut, and launches the app.

## What the zip contains
- The WinUI app (`ds-winui.exe`) + the in-process engine `ds_core.dll` + the warm-synth
  `ds-helper.exe` + the `dontspeak.exe` MCP server / hook executor + `AppIcon.ico`.
- The **.NET 10 runtime and the Windows App SDK, bundled** (self-contained publish), so the
  extracted app runs with nothing else installed.
- The voice models download on first launch (into the per-user model dir), the same as
  macOS/Linux — so the shipped zip stays small. (A fully-offline zip that also bundles the
  models is possible by dropping `-SkipModels`; see below.)

## Build it
Prereqs (one-time on the build machine): Rust (MSVC) · the repo's `~/.dotnet` .NET 10 SDK ·
NASM + LLVM on PATH (ring's crypto assembles with them).

```powershell
pwsh apps/windows/installer/build-portable.ps1 -Arch x64 -SkipModels
# → apps/windows/installer/Output/dontspeak-<version>-windows-x86_64.zip
```

- `-Arch arm64` cross-compiles (needs the arm64 MSVC tools + clang).
- Drop `-SkipModels` to bundle Kokoro + Parakeet + onnxruntime into the zip for a fully
  offline archive (~1 GB larger).

`build-portable.ps1` does: `cargo build --release` (core + helper + `dontspeak`) →
`dotnet publish` the WinUI app **self-contained** (`--self-contained` +
`WindowsAppSDKSelfContained`; the `StripUnusedWindowsAI` csproj target trims the unused
Windows-ML DLLs) → optional model prefetch into `models\` → `Compress-Archive`. It shares
`build-common.ps1` with the rest of the Windows build. The `payload/` and `Output/` folders
are build artifacts (git-ignored).

The full multi-arch release is built by tag-triggered CI (`.github/workflows/release.yml`),
which publishes `dontspeak-<version>-windows-<x86_64|aarch64>.zip` alongside a `checksums.txt`.
