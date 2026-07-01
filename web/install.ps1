#Requires -Version 5
<#
  DontSpeak one-command installer — Windows.

      irm https://dontspeak.org/install.ps1 | iex

  Downloads the self-contained portable zip for this arch from the latest GitHub Release,
  verifies its SHA-256, extracts it to %LOCALAPPDATA%\Programs\DontSpeak (no elevation, no
  runtime install — .NET + the Windows App SDK are bundled), wires the MCP server + voice
  hooks into every client (`dontspeak wire --all`), adds a Start-menu shortcut, and launches
  the app so the voice models download themselves on first boot. No compiler required.

  Programmers who want a from-source build should clone the repo and use the
  apps/windows/installer/build-portable.ps1 path instead (this script never builds).

  Env overrides:
    DONTSPEAK_REPO            owner/repo (default delllusional/DontSpeak)
    DONTSPEAK_DOWNLOAD_BASE   override the asset base URL (e.g. a dontspeak.org mirror)
    DONTSPEAK_DRY_RUN=1       resolve + print the plan, download nothing
#>
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0
$repo = if ($env:DONTSPEAK_REPO) { $env:DONTSPEAK_REPO } else { 'delllusional/DontSpeak' }
$api  = "https://api.github.com/repos/$repo/releases/latest"
$dry  = $env:DONTSPEAK_DRY_RUN -eq '1'

function Say  ($m) { Write-Host "==> $m" }
function Warn ($m) { Write-Warning $m }

# arch: ARM64 → arm64, everything else (AMD64) → x64
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$zipName = "dontspeak-portable-$arch.zip"

# Resolve an asset URL by literal filename off the latest release (or the override base).
function Resolve-Asset ($name) {
  if ($env:DONTSPEAK_DOWNLOAD_BASE) { return ($env:DONTSPEAK_DOWNLOAD_BASE.TrimEnd('/') + "/$name") }
  $rel = Invoke-RestMethod -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $api
  $a = $rel.assets | Where-Object { $_.name -eq $name } | Select-Object -First 1
  if ($a) { return $a.browser_download_url } else { return $null }
}

$zipUrl = Resolve-Asset $zipName
if (-not $zipUrl) { throw "no Windows asset ($zipName) on the latest release of $repo" }
Say "Windows $arch -> $zipUrl"

if ($dry) { Write-Host "(dry run) would unzip to %LOCALAPPDATA%\Programs\DontSpeak then wire --all"; return }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dontspeak-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zip = Join-Path $tmp $zipName
  Say "downloading"
  Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $zipUrl -OutFile $zip

  # SHA-256 verify against checksums.txt (skips cleanly if the release lacks it).
  $sumsUrl = Resolve-Asset 'checksums.txt'
  if ($sumsUrl) {
    try {
      $sums = (Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $sumsUrl).Content
      $want = ($sums -split "`n" | Where-Object { $_ -match ("\*?" + [regex]::Escape($zipName) + '\s*$') } |
               Select-Object -First 1) -replace '\s.*$', ''
      if ($want) {
        $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($got -ne $want.ToLower()) { throw "checksum mismatch for $zipName (want $want, got $got)" }
        Say "verified $zipName (sha256 ok)"
      } else { Warn "$zipName not listed in checksums.txt — skipping integrity check" }
    } catch { if ($_.Exception.Message -match 'checksum mismatch') { throw } else { Warn "checksum step skipped: $($_.Exception.Message)" } }
  } else { Warn "no checksums.txt on the release — skipping integrity check" }

  # Extract to a per-user location (no elevation). Replace any prior copy.
  $dest = Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak'
  Say "installing to $dest"
  # Stop a running instance so its files aren't locked, then replace the folder.
  Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
  New-Item -ItemType Directory -Path $dest -Force | Out-Null
  Expand-Archive -Path $zip -DestinationPath $dest -Force

  $cli = Join-Path $dest 'dontspeak.exe'
  if (Test-Path $cli) {
    Say "wiring clients (MCP + hooks)"
    # Windows PowerShell 5.1 (the stock `irm | iex` host) raises NativeCommandError when a
    # native command writes to a redirected stderr under ErrorActionPreference=Stop — a mere
    # wire warning would abort the install after extraction. Contain it and warn instead
    # (parity with install.sh's `|| warn`).
    try {
      & $cli wire --all 2>$null
      if ($LASTEXITCODE -ne 0) { Warn "wire --all reported an issue (exit $LASTEXITCODE)" }
    } catch { Warn "wire --all reported an issue: $($_.Exception.Message)" }
  }
  else { Warn "dontspeak.exe not found under $dest — the zip layout may have changed" }

  # Start-menu shortcut so DontSpeak is launchable like any app.
  $ui = Join-Path $dest 'ds-winui.exe'
  if (Test-Path $ui) {
    $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) 'DontSpeak.lnk'
    $w = New-Object -ComObject WScript.Shell
    $s = $w.CreateShortcut($lnk); $s.TargetPath = $ui
    $ico = Join-Path $dest 'AppIcon.ico'; if (Test-Path $ico) { $s.IconLocation = $ico }
    $s.Save()
    Say "launching DontSpeak (first boot downloads the voice models)"
    Start-Process $ui
  }

  Write-Host ""
  Write-Host "Done. Start a NEW Claude Code session to load the DontSpeak MCP server."
  Write-Host "Models download automatically in the background; watch progress in the app."
  Write-Host "Undo any time:  & '$cli' wire --all --remove"
  Write-Host "Uninstall: close DontSpeak, run the unwire above, then delete $dest"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
