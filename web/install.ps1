#Requires -Version 5
<#
  DontSpeak one-command installer — Windows.

      irm https://dontspeak.org/install.ps1 | iex

  Downloads the self-contained portable zip for this arch from the latest GitHub Release,
  verifies its SHA-256, extracts it to %LOCALAPPDATA%\Programs\DontSpeak (no elevation, no
  runtime install — .NET + the Windows App SDK are bundled), wires the MCP server + voice
  hooks into every client (`dontspeak wire --reconcile`), adds a Start-menu shortcut, and launches
  the app so the voice models download themselves on first boot. No compiler required.

  Programmers who want a from-source build should clone the repo and use the
  apps/windows/installer/build-portable.ps1 path instead (this script never builds).

  Env overrides:
    DONTSPEAK_REPO            owner/repo (default delllusional/DontSpeak)
    DONTSPEAK_DOWNLOAD_BASE   serve the fixed-name checksums.txt from a mirror; versioned
                              assets always resolve via the GitHub API regardless
    DONTSPEAK_DRY_RUN=1       resolve + print the plan, download nothing
#>
# `irm | iex` runs this text in the CALLER's scope — the preference/StrictMode changes
# below would leak into the user's interactive session. The & { } wrapper scopes them
# (and everything else) to the install.
& {
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0
# 5.1's Invoke-WebRequest renders a progress bar per chunk, slowing large downloads by
# an order of magnitude.
$ProgressPreference = 'SilentlyContinue'
$repo = if ($env:DONTSPEAK_REPO) { $env:DONTSPEAK_REPO } else { 'delllusional/DontSpeak' }
$api  = "https://api.github.com/repos/$repo/releases/latest"
$dry  = $env:DONTSPEAK_DRY_RUN -eq '1'

function Say  ($m) { Write-Host "==> $m" }
function Warn ($m) { Write-Warning $m }

# Release-asset arch token is uname-style everywhere: ARM64 → aarch64, AMD64 → x86_64.
# Detect the MACHINE, not this process: an x64-emulated shell on Windows-on-ARM reports
# PROCESSOR_ARCHITECTURE=AMD64 (which would install the x86_64 build on an aarch64
# machine); PROCESSOR_ARCHITEW6432 carries the real one for emulated processes.
$machineArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$arch = if ($machineArch -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$zipPattern = "^dontspeak-.+-windows-$arch\.zip$"   # dontspeak-<ver>-windows-<arch>.zip

# ONE API call for the whole install — both assets (the zip and checksums.txt) resolve
# from this response. Anonymous callers are rate-limited (60/hr/IP), and a second call
# failing must not abort an install whose zip already downloaded.
$release = Invoke-RestMethod -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $api

# Resolve an asset URL off the fetched release: by regex pattern (the versioned zip) or
# literal name (checksums.txt — the only fixed-name asset, and the only thing the
# DONTSPEAK_DOWNLOAD_BASE override can serve; a static mirror can't know versioned
# names, so those always resolve via the release above).
function Resolve-Asset ($nameOrPattern, [switch]$Pattern) {
  if ($env:DONTSPEAK_DOWNLOAD_BASE -and -not $Pattern) {
    return ($env:DONTSPEAK_DOWNLOAD_BASE.TrimEnd('/') + "/$nameOrPattern")
  }
  $a = $release.assets | Where-Object { if ($Pattern) { $_.name -match $nameOrPattern } else { $_.name -eq $nameOrPattern } } |
       Select-Object -First 1
  if ($a) { return $a.browser_download_url } else { return $null }
}

$zipUrl = Resolve-Asset $zipPattern -Pattern
if (-not $zipUrl) { throw "no Windows asset (dontspeak-<ver>-windows-$arch.zip) on the latest release of $repo" }
Say "Windows $arch -> $zipUrl"

if ($dry) { Write-Host "(dry run) would unzip to %LOCALAPPDATA%\Programs\DontSpeak then wire --reconcile"; return }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dontspeak-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zipName = [System.IO.Path]::GetFileName(([uri]$zipUrl).AbsolutePath)
  $zip = Join-Path $tmp $zipName
  Say "downloading"
  # -UseBasicParsing: without it, 5.1 routes responses through the IE engine, which throws
  # on hosts where IE is removed/unconfigured (a no-op on PowerShell 7).
  Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $zipUrl -OutFile $zip

  # SHA-256 verify against checksums.txt (skips cleanly if the release lacks it).
  $sumsUrl = Resolve-Asset 'checksums.txt'
  if ($sumsUrl) {
    try {
      $sums = (Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $sumsUrl).Content
      # GitHub serves checksums.txt as application/octet-stream, so PowerShell 7 hands back a
      # byte[] (5.1 gives a string). Splitting a byte[] on "`n" stringifies it to "104 101 …"
      # with no newlines, so the zip is never "found" and the integrity check silently skips.
      # Decode to text first when the body came back as bytes.
      if ($sums -is [byte[]]) { $sums = [System.Text.Encoding]::UTF8.GetString($sums) }
      $want = ($sums -split "`n" | Where-Object { $_ -match ("\*?" + [regex]::Escape($zipName) + '\s*$') } |
               Select-Object -First 1) -replace '\s.*$', ''
      if ($want) {
        $got = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($got -ne $want.ToLower()) { throw "checksum mismatch for $zipName (want $want, got $got)" }
        Say "verified $zipName (sha256 ok)"
      } else { Warn "$zipName not listed in checksums.txt — skipping integrity check" }
    } catch { if ($_.Exception.Message -match 'checksum mismatch') { throw } else { Warn "checksum step skipped: $($_.Exception.Message)" } }
  } else { Warn "no checksums.txt on the release — skipping integrity check" }

  # Extract into the temp dir FIRST, swap after — a failed/partial extraction (corrupt
  # zip, disk full) must not leave the machine with the old install already deleted.
  $stagedApp = Join-Path $tmp 'app'
  Expand-Archive -Path $zip -DestinationPath $stagedApp -Force

  # Per-user location (no elevation). Replace any prior copy.
  $dest = Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak'
  Say "installing to $dest"
  # Stop a running instance so its files aren't locked. Handles release a beat AFTER
  # Stop-Process returns, so retry the delete briefly instead of dying mid-removal.
  Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
  for ($i = 0; (Test-Path $dest) -and $i -lt 10; $i++) {
    try { Remove-Item $dest -Recurse -Force -ErrorAction Stop } catch { Start-Sleep -Milliseconds 300 }
  }
  if (Test-Path $dest) { throw "cannot remove the previous install at $dest (files still in use)" }
  New-Item -ItemType Directory -Path (Split-Path $dest) -Force | Out-Null
  try { Move-Item -Path $stagedApp -Destination $dest -ErrorAction Stop }
  catch { Copy-Item -Path $stagedApp -Destination $dest -Recurse -Force }  # TEMP on another volume

  # Publish the CLI for `dontspeak <client>` in NEW terminals. Keep the user PATH
  # additive/idempotent and never rewrite the machine PATH or require elevation.
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  $pathEntries = @($userPath -split ';' | Where-Object { $_ })
  $destKey = $dest.TrimEnd('\')
  if (-not ($pathEntries | Where-Object { $_.TrimEnd('\') -ieq $destKey })) {
    $userPath = (@($pathEntries) + $dest) -join ';'
    [Environment]::SetEnvironmentVariable('Path', $userPath, 'User')
    Say 'added DontSpeak to your user PATH (available in new terminals)'
  }
  if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $destKey })) {
    $env:Path = "$dest;$env:Path"
  }

  $cli = Join-Path $dest 'dontspeak.exe'
  if (Test-Path $cli) {
    Say "wiring clients (MCP + hooks)"
    # Windows PowerShell 5.1 (the stock `irm | iex` host) raises NativeCommandError when a
    # native command writes to a redirected stderr under ErrorActionPreference=Stop — a mere
    # wire warning would abort the install after extraction. Contain it and warn instead
    # (parity with install.sh's `|| warn`).
    try {
      # Keep this hidden and waited: the console-subsystem CLI is interactive when launched
      # by a user, but installation-time reconciliation must not flash a window or race ahead.
      $wp = Start-Process -FilePath $cli -ArgumentList 'wire','--reconcile' -Wait -PassThru -WindowStyle Hidden
      if ($wp.ExitCode -ne 0) { Warn "wire --reconcile reported an issue (exit $($wp.ExitCode))" }
    } catch { Warn "wire --reconcile reported an issue: $($_.Exception.Message)" }
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

    # Start-at-login: bring DontSpeak up minimized to the tray on sign-in (the resident-host
    # model — same as the retired Inno installer's Finished-page checkbox). The value NAME and
    # the `--hidden` argument match the app's own tray toggle (winui TrayIcon.cs: RunValue
    # "DontSpeak"), so the tray's "Start at login" checkmark stays in sync and toggling it there
    # cleanly removes this. Opt out of the install-time enable with DONTSPEAK_NO_AUTOSTART=1.
    if ($env:DONTSPEAK_NO_AUTOSTART -ne '1') {
      $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
      New-ItemProperty -Path $runKey -Name 'DontSpeak' -Value ('"' + $ui + '" --hidden') -PropertyType String -Force | Out-Null
      Say "enabled start-at-login (toggle in the tray menu; DONTSPEAK_NO_AUTOSTART=1 to skip)"
    }

    Say "launching DontSpeak (first boot downloads the voice models)"
    Start-Process $ui
  }

  # ── Windows uninstall entry (Settings > Apps / Control Panel > Programs) ──────────────
  # The portable zip has no installer framework, so nothing would otherwise register DontSpeak
  # in the standard uninstall UI (the retired Inno build did, via unins000.exe). Register a
  # PER-USER key (HKCU — the install is per-user, no admin) and ship a small uninstall.ps1 next
  # to the app. Its UninstallString runs the SAME teardown the retired installer did: unwire
  # every client (dontspeak wire --all --remove), then remove the app, shortcut, autostart entry,
  # downloaded models/config, and the uninstall key itself.
  $ver = if ($zipName -match 'dontspeak-(.+?)-windows') { $Matches[1] } else { '' }
  $unps = Join-Path $dest 'uninstall.ps1'
  # Single-quoted here-string: the $PSScriptRoot / $env: refs below are LITERAL — they resolve
  # when the uninstaller RUNS, not now. The here-string IS scripts/uninstall.ps1, byte-for-byte —
  # never edit it here: edit scripts/uninstall.ps1 and re-embed; packaging_sync.rs (cargo test)
  # fails on any drift. The dir is deleted last, from a detached cmd, because this script
  # lives inside it (a running script can't delete its own folder).
  @'
# uninstall.ps1 — THE DontSpeak uninstaller (Windows): the single source of truth.
# web/install.ps1 embeds it verbatim into the install dir and registers it as the
# Settings > Apps UninstallString. packaging_sync.rs pins the copies in sync.
# Removes everything the one-command installer created.
$ErrorActionPreference = 'SilentlyContinue'
# Only the PLACED copy sits next to dontspeak.exe — any other copy (repo checkout,
# standalone download) must target the standard install dir, NOT its own folder:
# $PSScriptRoot alone once resolved to a repo's scripts/ dir and step 7 deleted it.
$dest = if ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'dontspeak.exe'))) { $PSScriptRoot } else { Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak' }
# 1. Stop the resident app + engine + warm helper so no files are locked.
Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
# 2. Unwire every client (MCP server + voice hooks) via the app's own remover — waited.
$cli = Join-Path $dest 'dontspeak.exe'
if (Test-Path $cli) { Start-Process -FilePath $cli -ArgumentList 'wire','--all','--remove' -Wait -WindowStyle Hidden }
# 3. Start-menu shortcut + start-at-login entry.
Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'DontSpeak.lnk') -Force
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name DontSpeak
# 4. Remove only our exact install directory from the user PATH.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$destKey = $dest.TrimEnd('\')
$keptPath = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $destKey }) -join ';'
[Environment]::SetEnvironmentVariable('Path', $keptPath, 'User')
# 5. Downloaded models + logs + config (everything DontSpeak wrote outside the install dir).
Remove-Item "$env:LOCALAPPDATA\DontSpeak" -Recurse -Force
Remove-Item "$env:APPDATA\DontSpeak" -Recurse -Force
# 6. The uninstall registry entry itself (so it drops out of Settings > Apps).
Remove-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak' -Recurse -Force
# 7. Delete the install dir LAST — from a detached cmd after a short delay, so this running
#    script's own folder is free to remove once powershell exits.
if (Test-Path $dest) { Start-Process cmd.exe -ArgumentList '/c',"timeout /t 2 >nul & rmdir /s /q `"$dest`"" -WindowStyle Hidden }
Write-Host 'DontSpeak removed.'
'@ | Set-Content -Path $unps -Encoding UTF8

  $unkey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak'
  New-Item -Path $unkey -Force | Out-Null
  $uninstallCmd = 'powershell.exe -NoProfile -ExecutionPolicy Bypass -File "' + $unps + '"'
  $unprops = [ordered]@{
    DisplayName          = 'DontSpeak'
    DisplayVersion       = $ver
    Publisher            = 'DontSpeak'
    DisplayIcon          = (Join-Path $dest 'AppIcon.ico')
    InstallLocation      = $dest
    UninstallString      = $uninstallCmd
    QuietUninstallString = $uninstallCmd
    NoModify             = 1
    NoRepair             = 1
  }
  foreach ($k in $unprops.Keys) {
    $type = if ($unprops[$k] -is [int]) { 'DWord' } else { 'String' }
    New-ItemProperty -Path $unkey -Name $k -Value $unprops[$k] -PropertyType $type -Force | Out-Null
  }
  Say "registered uninstall entry (Settings > Apps > DontSpeak)"

  Write-Host ""
  Write-Host "Done. Start a NEW Claude Code session to load the DontSpeak MCP server."
  Write-Host "Models download automatically in the background; watch progress in the app."
  Write-Host "Undo any time:  & '$cli' wire --all --remove"
  Write-Host "Uninstall: Settings > Apps > DontSpeak > Uninstall (or run '$unps')"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
}
