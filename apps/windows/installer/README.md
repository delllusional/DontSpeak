# DontSpeak Windows package

Self-contained portable zip — no installer elevation, no runtime prereqs.
[dontspeak.org](https://dontspeak.org) (`scripts/install/web/install.ps1`) extracts to
`%LOCALAPPDATA%\Programs\DontSpeak`, updates per-user PATH, runs
`dontspeak wire --reconcile`, Start-menu shortcut, launches. Local archives use the same
script with `DONTSPEAK_ARCHIVE`.

## Zip contents

- `ds-winui.exe` + `ds_core.dll` + `ds-helper.exe` + `dontspeak.exe` + icon +
  `uninstall.ps1`
- Self-contained .NET 10 + Windows App SDK
- Models download on first launch (or bundle with build without `-SkipModels`)

## Build

Prereqs: Rust (MSVC), repo `.NET 10` SDK, NASM + LLVM on PATH.

```powershell
pwsh apps/windows/installer/build-portable.ps1 -Arch x64 -SkipModels
# → apps/windows/installer/Output/dontspeak-<version>-windows-x86_64.zip
```

```powershell
$env:DONTSPEAK_ARCHIVE = Get-Item apps\windows\installer\Output\dontspeak-*-windows-x86_64.zip |
  Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
try { & .\scripts\install\web\install.ps1 } finally { Remove-Item Env:\DONTSPEAK_ARCHIVE }
```

- `-Arch arm64` needs arm64 MSVC + clang
- Drop `-SkipModels` for offline zip (~1 GB larger)

Pipeline: `cargo build --release` → self-contained `dotnet publish` (strips unused
Windows-ML DLLs) → uninstaller payload → optional models → `Compress-Archive`.
`payload/` and `Output/` are gitignored. Release CI publishes multi-arch zips +
`checksums.txt`.
