---
name: build-windows
description: Build / clean-reinstall / package DontSpeak on Windows. Two use cases — (1) local clean build + reinstall for dev testing, (2) build a distributable package (installer .exe or portable .zip). Use when asked to build, reinstall, package, or cut the Windows app. Runs natively here (x64); arm64 cross-compiles.
---

# DontSpeak — Windows (build / reinstall / package)

> **Runs on:** this Windows box. **Working dir:** repo root (`C:\Users\usr\Develop\git\dontspeak`). Use the **PowerShell** tool. Symmetrical with `build-macos` / `build-linux`.

The scripts under `apps/windows/installer/` are the source of truth (`build.ps1`, `build-portable.ps1`, shared `build-common.ps1`). This skill runs them and handles the install/uninstall dance — **don't duplicate build logic here**; edit the scripts.

**Prereqs (one-time):** Rust (MSVC, via rustup) · .NET 10 SDK in `~/.dotnet` · NASM + LLVM on PATH (ring's crypto) · Inno Setup 6 (`winget install -e --id JRSoftware.InnoSetup`). A missing-tool error → install that one tool and re-run.

## Use case 1 — local clean build + reinstall (dev)

The shipping app is the WinUI app installed from the Inno `setup.exe`. The engine + Caps hook run **in-process** inside `ds-winui` via `ds_core.dll` (no separate daemon on Windows).

1. **Build** the installer:
   ```powershell
   pwsh -NoProfile -File apps\windows\installer\build.ps1 -Arch x64
   ```
   (`-Arch arm64` cross-compiles.) Slow (~min) — run in the background + read the tee'd log. On success: `DONE → ...\dontspeak-setup-x64.exe`.
2. **Copy to Desktop** (handy for the user):
   ```powershell
   Copy-Item apps\windows\installer\Output\dontspeak-setup-x64.exe ([Environment]::GetFolderPath('Desktop')) -Force
   ```
3. **Stop running processes** so the upgrade can replace files in use:
   ```powershell
   Get-Process | Where-Object { $_.ProcessName -match 'dontspeak' } | Stop-Process -Force
   ```
4. *(clean install only)* **uninstall the old build first** — see Uninstall below. For a plain in-place upgrade, skip this.
5. **Install elevated + silent** (tell the user to approve the **UAC** prompt):
   ```powershell
   Start-Process ([Environment]::GetFolderPath('Desktop') + '\dontspeak-setup-x64.exe') `
     -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' -Verb RunAs -Wait -PassThru
   ```
   Exit code `0` = success. Same `AppId` ⇒ in-place upgrade (the workspace version need not change).
6. **Relaunch** (silent install does NOT auto-launch):
   ```powershell
   Start-Process 'C:\Program Files\DontSpeak\ds-winui.exe'
   ```
7. **Verify**: binaries under `C:\Program Files\DontSpeak\` stamped with the just-built time; `ds-winui` + `ds-helper` running.

## Use case 2 — build a distributable package

- **Installer** (the release artifact): `build.ps1 -Arch x64` / `-Arch arm64` → `Output\dontspeak-setup-<arch>.exe` (~15 MB, framework-dependent).
- **Portable zip** (self-contained, no install, bundles models): `build-portable.ps1 -Arch x64 [-SkipModels]` → `Output\dontspeak-portable-<arch>.zip`.
- **Signing** is gated on SignPath secrets (CI `release.yml` only); local builds are unsigned → first launch may hit SmartScreen.
- The full multi-arch signed release is tag-triggered CI (`release.yml`) — this skill is the fast local path.

## Uninstall / clean

- **App**: run the Inno uninstaller — `C:\Program Files\DontSpeak\unins000.exe` (silent: append `/VERYSILENT`), elevated.
- **CLI/daemon stack** (the alternative `~/.local/bin` deployment via `install.ps1`/`enable.ps1`): `apps\windows\disable.ps1` unregisters the logon task + stops the daemon.

## Notes

- The **CLI/daemon model** is separate: `apps\windows\install.ps1` builds the 3 console bins (`dontspeakd/dontspeak/ds-helper`) into `~/.local/bin`, `enable.ps1` registers the logon task. Use that only if testing the headless stack rather than the WinUI app.
