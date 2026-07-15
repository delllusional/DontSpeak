---
name: build-windows
description: Uninstall / build+install / package DontSpeak on Windows. Three flows — (1) uninstall/clean, (2) local build + reinstall for dev testing, (3) build the distributable portable .zip. Use when asked to build, reinstall, package, uninstall, or cut the Windows app. Runs on Windows only (x64 native; arm64 cross-compiles).
---

# DontSpeak — Windows (uninstall / install / package)

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

> **Runs on:** Windows only (PowerShell). **Working dir:** repo root. Same three flows as `build-macos` / `build-linux`.

DontSpeak ships on Windows as a **self-contained portable zip** — no installer. The WinUI app (`ds-winui.exe`) hosts the engine in-process via `ds_core.dll` (P/Invoke) + a warm `ds-helper.exe` synth child; no separate daemon. Scripts under `apps/windows/installer/` (`build-portable.ps1`, shared `build-common.ps1`) are the source of truth — **don't duplicate build logic; edit the scripts**.

**Prereqs (one-time):** Rust (MSVC, via rustup) · .NET 10 SDK in `~/.dotnet` · NASM + LLVM on PATH (ring's crypto). A missing-tool error → install that one tool and re-run.

## 1 — Uninstall / clean

Run the canonical uninstaller — do NOT hand-roll the teardown (a partial copy drifts; the full one also removes the Run key, the Settings > Apps entry, and both data dirs). `scripts/uninstall.ps1` is the single source; an installed box carries the same bytes at `%LOCALAPPDATA%\Programs\DontSpeak\uninstall.ps1` (also reachable via Settings > Apps > DontSpeak > Uninstall):
```powershell
# from a repo checkout
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\uninstall.ps1
# or on any installed box
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\DontSpeak\uninstall.ps1"
```

## 2 — Build + install (dev)

Install = extract the portable zip to the per-user folder (the same thing `web/install.ps1` does for end users).

1. **Build** the zip (fast dev loop: `-SkipModels`; `-Arch arm64` cross-compiles — needs an ARM64 host to also prefetch models, so pair `arm64` with `-SkipModels` on an x64 box). Takes a few minutes (cargo release build + `dotnet publish`); success ends with `DONE → …windows-x86_64.zip`:
   ```powershell
   pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels
   ```
2. **Stop every process launched from the install directory and wait for those exact
   process objects to exit** so their runtime files are no longer locked:
   ```powershell
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
   ```
3. **Remove the old install completely, then extract.** Removal is terminating and
   extraction must not run if anything remains:
   ```powershell
   if (Test-Path -LiteralPath $dest) {
     Remove-Item -LiteralPath $dest -Recurse -Force -ErrorAction Stop
   }
   if (Test-Path -LiteralPath $dest) { throw "cannot remove the previous install at $dest" }
   Expand-Archive (Get-Item apps\windows\installer\Output\dontspeak-*-windows-x86_64.zip) $dest -Force
   ```
4. **Wire + launch**:
   ```powershell
   & "$dest\dontspeak.exe" wire --reconcile
   Start-Process "$dest\ds-winui.exe"
   ```
5. **Verify:** binaries under `$dest` stamped with the just-built time; `ds-winui` + `ds-helper` running.

## 3 — Build the package

```powershell
pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels    # or -Arch arm64
```
- Output: `Output\dontspeak-<version>-windows-<x86_64|aarch64>.zip` — self-contained (bundles .NET + Windows App SDK). With `-SkipModels`, models download on first launch; omit it to bundle them.
- **Signing:** none — the app runs from an extracted folder, nothing to code-sign; first launch may hit SmartScreen until download reputation accrues.

## Notes

- The full multi-arch release is tag-triggered CI (`release.yml`) — this skill is the fast local path.
