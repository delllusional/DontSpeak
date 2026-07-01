---
name: build-windows
description: Build / clean-reinstall / package DontSpeak on Windows. Two use cases — (1) local clean build + reinstall for dev testing, (2) build the distributable portable .zip. Use when asked to build, reinstall, package, or cut the Windows app. Runs natively here (x64); arm64 cross-compiles.
---

# DontSpeak — Windows (build / reinstall / package)

> **Runs on:** this Windows box. **Working dir:** repo root (`C:\Users\usr\Develop\git\dontspeak`). Use the **PowerShell** tool. Symmetrical with `build-macos` / `build-linux`.

DontSpeak ships on Windows as a **self-contained portable zip** — no installer. The scripts under `apps/windows/installer/` are the source of truth (`build-portable.ps1`, shared `build-common.ps1`). This skill runs them and handles the install/uninstall dance — **don't duplicate build logic here**; edit the scripts.

**Prereqs (one-time):** Rust (MSVC, via rustup) · .NET 10 SDK in `~/.dotnet` · NASM + LLVM on PATH (ring's crypto). A missing-tool error → install that one tool and re-run.

## Use case 1 — local clean build + reinstall (dev)

The shipping app is the WinUI app (`ds-winui`); the engine + Caps hook run **in-process** via `ds_core.dll` (no separate daemon on Windows). Install = extract the portable zip to a per-user folder (the same thing `web/install.ps1` does for end users).

1. **Build** the portable zip (fast dev loop: add `-SkipModels`):
   ```powershell
   pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels
   ```
   (`-Arch arm64` cross-compiles.) Slow (~min) — run in the background + read the tee'd log. On success: `DONE → ...\dontspeak-portable-x64.zip`.
2. **Stop running processes** so files aren't locked:
   ```powershell
   Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force
   ```
3. **Extract** over the per-user install dir (replace in place):
   ```powershell
   $dest = "$env:LOCALAPPDATA\Programs\DontSpeak"
   if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
   Expand-Archive apps\windows\installer\Output\dontspeak-portable-x64.zip $dest -Force
   ```
4. **Wire + launch**:
   ```powershell
   & "$dest\dontspeak.exe" wire --all
   Start-Process "$dest\ds-winui.exe"
   ```
5. **Verify**: binaries under `$dest` stamped with the just-built time; `ds-winui` + `ds-helper` running.

## Use case 2 — build the distributable package

- **Portable zip** (the release artifact, self-contained): `build-portable.ps1 -Arch x64` / `-Arch arm64` → `Output\dontspeak-portable-<arch>.zip`. Bundles .NET + the Windows App SDK; the app downloads models on first launch (add nothing) or bundle them by dropping `-SkipModels`.
- **Signing**: none — the app runs from an extracted folder, so there is nothing to code-sign; first launch may hit SmartScreen until download reputation accrues.
- The full multi-arch release is tag-triggered CI (`release.yml`) — this skill is the fast local path.

## Uninstall / clean

- Close DontSpeak, un-wire, then delete the folder + shortcut:
  ```powershell
  Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force
  & "$env:LOCALAPPDATA\Programs\DontSpeak\dontspeak.exe" wire --all --remove
  Remove-Item "$env:LOCALAPPDATA\Programs\DontSpeak" -Recurse -Force
  Remove-Item ([Environment]::GetFolderPath('Programs') + '\DontSpeak.lnk') -ErrorAction SilentlyContinue
  ```
