---
name: build-windows
description: Uninstall / build+install / package DontSpeak on Windows. Three flows — (1) uninstall/clean, (2) local build + reinstall for dev testing, (3) build the distributable portable .zip. Use when asked to build, reinstall, package, uninstall, or cut the Windows app. Runs on Windows only (x64 native; arm64 cross-compiles).
---

# DontSpeak — Windows (uninstall / install / package)

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Windows only (PowerShell), repo root. Portable zip + `scripts/install/web/install.ps1`.
Host: `ds-winui.exe` + `ds_core.dll` + `ds-helper.exe`. Build logic lives in
`apps/windows/installer/` — edit scripts, don't duplicate.

**Prereqs:** Rust (MSVC) · .NET 10 in `~/.dotnet` · NASM + LLVM on PATH.

## 1 — Uninstall

Use canonical uninstaller (Run key, Settings Apps, both data dirs) — don't hand-roll:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\install\bundle\uninstall.ps1
# or installed:
powershell -NoProfile -ExecutionPolicy Bypass -File "$env:LOCALAPPDATA\Programs\DontSpeak\uninstall.ps1"
```

## 2 — Build + install (dev)

Same installer as end users (PATH, wire, shortcut, startup, Settings Apps).

1. Build (`-SkipModels` for speed; arm64 cross-compile: pair with `-SkipModels` on x64):
   ```powershell
   pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels
   ```
   If a user-global `sccache` wrapper panics with `Unable to get config directory` before
   `rustc` starts, preserve the Cargo config and retry with the wrapper disabled for this
   build process only:
   ```powershell
   $previousWrapper = $env:CARGO_BUILD_RUSTC_WRAPPER
   try {
     $env:CARGO_BUILD_RUSTC_WRAPPER = ''
     pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels
   } finally {
     if ($null -eq $previousWrapper) { Remove-Item Env:\CARGO_BUILD_RUSTC_WRAPPER -ErrorAction SilentlyContinue }
     else { $env:CARGO_BUILD_RUSTC_WRAPPER = $previousWrapper }
   }
   ```
2. Install local artifact:
   ```powershell
   $archive = Get-Item apps\windows\installer\Output\dontspeak-*-windows-x86_64.zip |
     Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
   $previousArchive = $env:DONTSPEAK_ARCHIVE
   try {
     $env:DONTSPEAK_ARCHIVE = $archive
     & .\scripts\install\web\install.ps1
   } finally {
     if ($null -eq $previousArchive) { Remove-Item Env:\DONTSPEAK_ARCHIVE -ErrorAction SilentlyContinue }
     else { $env:DONTSPEAK_ARCHIVE = $previousArchive }
   }
   ```
3. Verify:
   ```powershell
   $dest = "$env:LOCALAPPDATA\Programs\DontSpeak"
   Get-Item "$dest\dontspeak.exe","$dest\ds-winui.exe","$dest\uninstall.ps1"
   Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak' |
     Select-Object DisplayVersion,InstallLocation,UninstallString
   Get-Process ds-winui,ds-helper
   ```

## 3 — Package only

```powershell
pwsh -NoProfile -File apps\windows\installer\build-portable.ps1 -Arch x64 -SkipModels
```

Output: `Output\dontspeak-<version>-windows-<arch>.zip` (self-contained .NET + WASDK +
`uninstall.ps1`). Unsigned → possible SmartScreen. Multi-arch release = tag CI.
