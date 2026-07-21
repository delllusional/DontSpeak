<#
build-portable.ps1 — produce the SELF-CONTAINED, no-install DontSpeak portable zip.

Bundles EVERYTHING needed to run with zero install: the WinUI app + the .NET 10 runtime +
the Windows App SDK (all self-contained) + the native engine DLL/helper + the merged
dontspeak bin + canonical uninstaller + default speech models (Kokoro + Parakeet + onnxruntime)
under a sibling `models\` dir. The app auto-detects that dir on launch (App.EnablePortableModelDir → DONTSPEAK_MODEL_DIR),
so an EXTRACTED copy runs fully offline — no .NET / Windows App Runtime install, no model
download.

Output: Output\dontspeak-<version>-windows-<x86_64|aarch64>.zip

Prereqs: Rust (MSVC) + the arm64 cross tools/clang for -Arch arm64 (ring assembles with
clang), .NET 10 SDK (~/.dotnet). The model prefetch runs the just-built ds-helper.exe,
so a FULLY-OFFLINE arm64 zip needs an ARM64 host — an x64 box cross-building arm64 must
pass -SkipModels (CI does; the models then download on first launch). Usage:
  pwsh apps/windows/installer/build-portable.ps1 [-Arch x64|arm64] [-SkipModels]
#>
param(
    [ValidateSet('x64','arm64')][string]$Arch = 'x64',
    [switch]$SkipModels   # skip the ~1 GB model prefetch (local mechanics check only)
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\build-common.ps1"   # Initialize-BuildEnv / Resolve-BuildArch / Invoke-CargoRelease
$repo = (Resolve-Path "$PSScriptRoot\..\..\..").Path
Initialize-BuildEnv
$b = Resolve-BuildArch -Arch $Arch -Repo $repo   # .Rel / .CargoTargetArg / .RustTarget / .DotnetPlatform
$rel    = $b.Rel
$dotnetPlatform = $b.DotnetPlatform
$stage  = "$PSScriptRoot\portable\dontspeak-portable-$Arch"
$outDir = "$PSScriptRoot\Output"
$ver    = Get-DsVersion -Repo $repo
# Uniform release-asset arch token (uname-style), shared with the macOS/Linux packagers.
$archToken = if ($Arch -eq 'arm64') { 'aarch64' } else { 'x86_64' }
$zipName = "dontspeak-$ver-windows-$archToken.zip"
# AssemblyVersion/FileVersion must be purely numeric X.Y.Z.W (no semver prerelease
# suffix like "-dev") — the shipped .exe's file-properties version would otherwise
# silently stay the .NET SDK's default 1.0.0.0 forever, regardless of $ver, since
# nothing else stamps it. Strip any "-suffix" and pad to 4 components; `-p:Version`
# itself (NuGet-style, shown nowhere on the .exe's Details tab) keeps the full string.
$fileVer = ($ver -split '-')[0]
if (($fileVer -split '\.').Count -eq 3) { $fileVer = "$fileVer.0" }

Write-Host "==> 1/4  cargo build --release ($($Arch): core + helper + dontspeak)" -ForegroundColor Cyan
Invoke-CargoRelease -Repo $repo -CargoTargetArg $b.CargoTargetArg -RustTarget $b.RustTarget

Write-Host "==> 2/4  dotnet publish WinUI (SELF-CONTAINED: .NET + Windows App SDK bundled)" -ForegroundColor Cyan
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
# --self-contained + WindowsAppSDKSelfContained bundle BOTH runtimes into the output, so the
# extracted app needs no .NET Desktop Runtime / Windows App Runtime installed. The csproj's
# StripUnusedWindowsAI target still trims the unused Windows-ML bits from the publish.
# Buffer the (voluminous) publish output; MSBuild reports errors on stdout, so echo it
# on failure — a bare "publish failed" is undebuggable from a CI transcript.
$publishOut = dotnet publish "$repo\apps\windows\winui\DontSpeak.WinUI.csproj" -c Release `
    -p:Platform=$dotnetPlatform -r "win-$Arch" --self-contained true `
    -p:WindowsAppSDKSelfContained=true `
    -p:Version=$ver -p:AssemblyVersion=$fileVer -p:FileVersion=$fileVer `
    -o "$stage" 2>&1
if ($LASTEXITCODE) { $publishOut | Write-Host; throw "dotnet publish failed" }
Copy-Item "$rel\dontspeak.exe" "$stage\" -Force
Copy-Item "$repo\apps\windows\winui\AppIcon.ico" "$stage\" -Force
# Ship the canonical standalone uninstaller as payload, just like the Linux tarball and
# macOS app bundle. scripts/install/web/install.ps1 registers this file; it must never embed another copy.
Copy-Item "$repo\scripts\install\bundle\uninstall.ps1" "$stage\uninstall.ps1" -Force
# The binary embeds Apache-licensed Misaki dictionary data through voice-g2p. Keep the product
# license, third-party notice, and referenced license copies together in every release archive.
Copy-Item "$repo\LICENSE" "$stage\LICENSE" -Force
Copy-Item "$repo\NOTICE.md" "$stage\NOTICE.md" -Force
New-Item -ItemType Directory -Force "$stage\licenses" | Out-Null
Copy-Item "$repo\licenses\*" "$stage\licenses\" -Force

Write-Host "==> 3/4  prefetch default models → $stage\models (Kokoro + Parakeet + onnxruntime, no CUDA)" -ForegroundColor Cyan
$models = "$stage\models"
New-Item -ItemType Directory -Force $models | Out-Null
if ($SkipModels) {
    Write-Host "    (skipped — -SkipModels; the zip will NOT be fully offline)" -ForegroundColor DarkYellow
} else {
    # The prefetch EXECUTES the just-built target-arch ds-helper.exe — impossible when
    # cross-building arm64 on an x64 host. Fail up front with the fix instead of dying
    # in a Win32 "not compatible" error mid-build.
    $hostArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($Arch -eq 'arm64' -and $hostArch -ne 'ARM64') {
        throw "-Arch arm64 on this $hostArch host cannot run the arm64 ds-helper.exe for the model prefetch — pass -SkipModels (models download on first launch) or build on an ARM64 machine"
    }
    # `--prefetch models` = kokoro + parakeet (each ensures onnxruntime); DONTSPEAK_MODEL_DIR
    # redirects the download into the bundle instead of the per-user cache.
    $prev = $env:DONTSPEAK_MODEL_DIR
    $env:DONTSPEAK_MODEL_DIR = $models
    & "$rel\ds-helper.exe" --prefetch models
    $code = $LASTEXITCODE
    if ($null -ne $prev) { $env:DONTSPEAK_MODEL_DIR = $prev } else { Remove-Item Env:\DONTSPEAK_MODEL_DIR -ErrorAction SilentlyContinue }
    if ($code) { throw "model prefetch failed ($code) — see %TEMP%\ds-prefetch-error.log" }
}

Write-Host "==> 4/4  zip → Output\$zipName" -ForegroundColor Cyan
New-Item -ItemType Directory -Force $outDir | Out-Null
$zip = "$outDir\$zipName"
if (Test-Path $zip) { Remove-Item $zip -Force }
# ZipFile, not Compress-Archive: the cmdlet's `Optimal` is deflate level 6 and it exposes no
# way to ask for level 9. Same container/method, ~3.5% smaller, and `includeBaseDirectory:$false`
# reproduces `-Path "$stage\*"` so install.ps1's Expand-Archive sees the same top-level layout.
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $stage, $zip, [System.IO.Compression.CompressionLevel]::SmallestSize, $false)
$mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ("DONE → {0} ({1} MB)" -f $zip, $mb) -ForegroundColor Green
