<#
build-portable.ps1 — produce the SELF-CONTAINED, no-install DontSpeak portable zip.

Bundles EVERYTHING needed to run with zero install: the WinUI app + the .NET 10 runtime +
the Windows App SDK (all self-contained) + the native engine DLL/helper + the merged
dontspeak bin + ALL voice models (Kokoro + Parakeet + onnxruntime) under a sibling `models\`
dir. The app auto-detects that dir on launch (App.EnablePortableModelDir → DONTSPEAK_MODEL_DIR),
so an EXTRACTED copy runs fully offline — no .NET / Windows App Runtime install, no model
download.

Output: Output\ds-portable-<arch>.zip

Prereqs: Rust (MSVC) + the arm64 cross tools/clang for -Arch arm64 (ring assembles with
clang), .NET 10 SDK (~/.dotnet). Usage:
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
$stage  = "$PSScriptRoot\portable\ds-portable-$Arch"
$outDir = "$PSScriptRoot\Output"

Write-Host "==> 1/4  cargo build --release ($($Arch): core + helper + dontspeak)" -ForegroundColor Cyan
Invoke-CargoRelease -Repo $repo -CargoTargetArg $b.CargoTargetArg -RustTarget $b.RustTarget

Write-Host "==> 2/4  dotnet publish WinUI (SELF-CONTAINED: .NET + Windows App SDK bundled)" -ForegroundColor Cyan
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
# --self-contained + WindowsAppSDKSelfContained bundle BOTH runtimes into the output, so the
# extracted app needs no .NET Desktop Runtime / Windows App Runtime installed. The csproj's
# StripUnusedWindowsAI target still trims the unused Windows-ML bits from the publish.
dotnet publish "$repo\apps\windows\winui\DontSpeak.WinUI.csproj" -c Release `
    -p:Platform=$dotnetPlatform -r "win-$Arch" --self-contained true `
    -p:WindowsAppSDKSelfContained=true -o "$stage" | Out-Null
if ($LASTEXITCODE) { throw "dotnet publish failed" }
Copy-Item "$rel\dontspeak.exe" "$stage\" -Force
Copy-Item "$repo\apps\windows\winui\AppIcon.ico" "$stage\" -Force

Write-Host "==> 3/4  prefetch ALL models → $stage\models (Kokoro + Parakeet + onnxruntime, no CUDA)" -ForegroundColor Cyan
$models = "$stage\models"
New-Item -ItemType Directory -Force $models | Out-Null
if ($SkipModels) {
    Write-Host "    (skipped — -SkipModels; the zip will NOT be fully offline)" -ForegroundColor DarkYellow
} else {
    # `--prefetch models` = kokoro + parakeet (each ensures onnxruntime); DONTSPEAK_MODEL_DIR
    # redirects the download into the bundle instead of the per-user cache.
    $prev = $env:DONTSPEAK_MODEL_DIR
    $env:DONTSPEAK_MODEL_DIR = $models
    & "$rel\ds-helper.exe" --prefetch models
    $code = $LASTEXITCODE
    if ($null -ne $prev) { $env:DONTSPEAK_MODEL_DIR = $prev } else { Remove-Item Env:\DONTSPEAK_MODEL_DIR -ErrorAction SilentlyContinue }
    if ($code) { throw "model prefetch failed ($code) — see %TEMP%\ds-prefetch-error.log" }
}

Write-Host "==> 4/4  zip → Output\ds-portable-$Arch.zip" -ForegroundColor Cyan
New-Item -ItemType Directory -Force $outDir | Out-Null
$zip = "$outDir\ds-portable-$Arch.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$stage\*" -DestinationPath $zip -CompressionLevel Optimal
$mb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
Write-Host ("DONE → {0} ({1} MB)" -f $zip, $mb) -ForegroundColor Green
