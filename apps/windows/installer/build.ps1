<#
build.ps1 — produce the DontSpeak Windows installer (setup.exe).

What it does, end to end:
  1. cargo build --release  the engine cdylib + the helper + the dontspeak bin.
  2. dotnet publish  the WinUI app FRAMEWORK-DEPENDENT (the minimal ~54 MB payload;
     the csproj's StripUnusedWindowsAI target trims the unused Windows-ML runtime).
  3. Stage the payload (app publish + dontspeak.exe + AppIcon.ico).
  4. Compile dontspeak.iss with Inno Setup (ISCC) → Output\dontspeak-setup.exe.

The installer itself (dontspeak.iss) lays down that minimal app, ensures the two
runtimes (.NET 10 Desktop + Windows App Runtime 2.0), optionally pulls the
voice models / CUDA GPU runtime via `ds-helper --prefetch` (so the download
URLs stay single-sourced in ds-model), wires Start-menu/Desktop shortcuts with the
brand icon, and best-effort registers the dontspeak MCP.

Prereqs to RUN this build script (one-time):
  - Rust (MSVC), .NET 10 SDK (the repo uses ~/.dotnet), Inno Setup 6
    (winget install -e --id JRSoftware.InnoSetup).
Usage:  pwsh windows/installer/build.ps1
#>
param([ValidateSet('x64','arm64')][string]$Arch = 'x64')

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\build-common.ps1"   # Initialize-BuildEnv / Resolve-BuildArch / Invoke-CargoRelease
$repo = (Resolve-Path "$PSScriptRoot\..\..\..").Path
Initialize-BuildEnv
$b = Resolve-BuildArch -Arch $Arch -Repo $repo   # .Rel / .CargoTargetArg / .RustTarget / .DotnetPlatform
$stage = "$PSScriptRoot\payload"
$outDir = "$PSScriptRoot\Output"

# 1/4 — the in-process engine cdylib, the warm-synth helper, and the merged dontspeak bin
# (MCP server + Claude Code hook executor + wire). See build-common.ps1.
Write-Host "==> 1/4  cargo build --release ($($Arch): core + helper + dontspeak)" -ForegroundColor Cyan
Invoke-CargoRelease -Repo $repo -CargoTargetArg $b.CargoTargetArg -RustTarget $b.RustTarget

Write-Host "==> 2/4  dotnet publish WinUI (framework-dependent, minimal)" -ForegroundColor Cyan
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
dotnet publish "$repo\apps\windows\winui\DontSpeak.WinUI.csproj" -c Release -p:Platform=$($b.DotnetPlatform) -r "win-$Arch" -o "$stage" | Out-Null
if ($LASTEXITCODE) { throw "dotnet publish failed" }

Write-Host "==> 3/4  stage payload (+ dontspeak + icon)" -ForegroundColor Cyan
Copy-Item "$($b.Rel)\dontspeak.exe" "$stage\" -Force
# The dontspeak bin is also the hook executor. Hooks are wired into
# settings.json by `dontspeak.exe wire` (the single cross-platform installer step).
Copy-Item "$repo\apps\windows\winui\AppIcon.ico" "$stage\" -Force
"    payload: {0} MB, {1} files" -f `
    [math]::Round((Get-ChildItem $stage -Recurse -File | Measure-Object Length -Sum).Sum/1MB), `
    (Get-ChildItem $stage -Recurse -File).Count | Write-Host

Write-Host "==> 4/4  ISCC compile dontspeak.iss" -ForegroundColor Cyan
$iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "ISCC.exe not found — install Inno Setup 6 (winget install -e --id JRSoftware.InnoSetup)" }
# Stamp the installer version from the SINGLE source of truth — rust/Cargo.toml
# [workspace.package] version (the same line scripts/version.sh reads for the macOS
# bundle + the CI tag check). Read natively here so the Windows build needs no bash.
$ver = (Select-String -Path "$repo\rust\Cargo.toml" -Pattern '^\s*version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches.Groups[1].Value
if (-not $ver) { $ver = '0.1.0' }
& $iscc "/DPayloadDir=$stage" "/DAppVersion=$ver" "/DTargetArch=$Arch" "/O$outDir" "$PSScriptRoot\dontspeak.iss"
if ($LASTEXITCODE) { throw "ISCC failed" }

$setup = Get-ChildItem "$outDir\*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host ("DONE → {0} ({1} MB)" -f $setup.FullName, [math]::Round($setup.Length/1MB,1)) -ForegroundColor Green
