#Requires -Version 5
<#
  DontSpeak one-command installer — Windows.

      irm https://dontspeak.org/install.ps1 | iex

  Downloads the self-contained portable zip for this arch from the latest GitHub Release,
  verifies its SHA-256, extracts it to %LOCALAPPDATA%\Programs\DontSpeak (no elevation, no
  runtime install — .NET + the Windows App SDK are bundled), wires the MCP server + voice
  hooks into every client (`dontspeak wire --reconcile`), adds a Start-menu shortcut, and launches
  the app so the voice models download themselves on first boot. No compiler required.

  Programmers can build with apps/windows/installer/build-portable.ps1, then run this
  same installer against that artifact with DONTSPEAK_ARCHIVE. This script never builds.

  Env overrides:
    DONTSPEAK_REPO            owner/repo (default delllusional/DontSpeak)
    DONTSPEAK_DOWNLOAD_BASE   serve the fixed-name checksums.txt from a mirror; versioned
                              assets always resolve via the GitHub API regardless
    DONTSPEAK_ARCHIVE         install an explicit local portable zip instead of downloading
                              latest (development path; the archive must match this machine)
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
$archive = if ($env:DONTSPEAK_ARCHIVE) {
  (Resolve-Path -LiteralPath $env:DONTSPEAK_ARCHIVE -ErrorAction Stop).Path
} else { $null }

function Say  ($m) { Write-Host "==> $m" }
function Warn ($m) { Write-Warning $m }

# Release-asset arch token is uname-style everywhere: ARM64 → aarch64, AMD64 → x86_64.
# Detect the MACHINE, not this process: an x64-emulated shell on Windows-on-ARM reports
# PROCESSOR_ARCHITECTURE=AMD64 (which would install the x86_64 build on an aarch64
# machine); PROCESSOR_ARCHITEW6432 carries the real one for emulated processes.
$machineArch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
$arch = if ($machineArch -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$zipPattern = "^dontspeak-.+-windows-$arch\.zip$"   # dontspeak-<ver>-windows-<arch>.zip

# ONE API call for a release install — both assets (the zip and checksums.txt) resolve
# from this response. A local development archive needs no network or GitHub release.
$release = if ($archive) { $null } else {
  Invoke-RestMethod -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $api
}

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

$zipUrl = if ($archive) { $null } else { Resolve-Asset $zipPattern -Pattern }
if (-not $archive -and -not $zipUrl) { throw "no Windows asset (dontspeak-<ver>-windows-$arch.zip) on the latest release of $repo" }
$zipName = if ($archive) { Split-Path -Leaf $archive } else { [System.IO.Path]::GetFileName(([uri]$zipUrl).AbsolutePath) }
if ($zipName -notmatch $zipPattern) {
  throw "archive '$zipName' does not match this machine (expected dontspeak-<ver>-windows-$arch.zip)"
}
$sourceLabel = if ($archive) { $archive } else { $zipUrl }
Say "Windows $arch -> $sourceLabel"

if ($dry) { Write-Host "(dry run) would unzip to %LOCALAPPDATA%\Programs\DontSpeak then wire --reconcile"; return }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dontspeak-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zip = if ($archive) { $archive } else { Join-Path $tmp $zipName }
  if ($archive) { Say "using local archive" }
  else {
    Say "downloading"
    # -UseBasicParsing: without it, 5.1 routes responses through the IE engine, which throws
    # on hosts where IE is removed/unconfigured (a no-op on PowerShell 7).
    Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $zipUrl -OutFile $zip
  }

  if (-not $archive) {
    # SHA-256 verify downloaded releases against checksums.txt (a local dev artifact has
    # no separately published checksum and was selected explicitly by the developer).
    $sumsUrl = Resolve-Asset 'checksums.txt'
    if ($sumsUrl) {
      try {
        $sums = (Invoke-WebRequest -UseBasicParsing -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $sumsUrl).Content
        # GitHub serves checksums.txt as application/octet-stream, so PowerShell 7 hands back a
        # byte[] (5.1 gives a string). Decode before splitting into checksum lines.
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
  }

  # Extract into the temp dir FIRST, swap after — a failed/partial extraction (corrupt
  # zip, disk full) must not leave the machine with the old install already deleted.
  $stagedApp = Join-Path $tmp 'app'
  Expand-Archive -Path $zip -DestinationPath $stagedApp -Force
  if (-not (Test-Path -LiteralPath (Join-Path $stagedApp 'uninstall.ps1') -PathType Leaf)) {
    throw "incomplete archive: missing canonical uninstall.ps1 payload"
  }

  # Per-user location (no elevation). Replace any prior copy.
  $dest = Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak'
  Say "installing to $dest"
  # Stop every process launched from this install, not unrelated same-named developer
  # builds. Wait for those exact process objects, then retain the delete retry because
  # Windows can release runtime file handles a beat after process exit.
  $destPrefix = [IO.Path]::GetFullPath($dest).TrimEnd('\') + '\'
  $installed = @(Get-Process -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Path -and [IO.Path]::GetFullPath($_.Path).StartsWith($destPrefix, [StringComparison]::OrdinalIgnoreCase) }
    catch { $false }
  })
  if ($installed) {
    $installed | Stop-Process -Force -ErrorAction Stop
    $installed | Wait-Process -ErrorAction Stop
  }
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
  # PER-USER key (HKCU — the install is per-user, no admin) for the uninstall.ps1 payload
  # shipped beside the app. Its UninstallString runs the SAME teardown the retired installer did: unwire
  # every client (dontspeak wire --all --remove), then remove the app, shortcut, autostart entry,
  # downloaded models/config, and the uninstall key itself.
  $ver = if ($zipName -match 'dontspeak-(.+?)-windows') { $Matches[1] } else { '' }
  $unps = Join-Path $dest 'uninstall.ps1'
  # build-portable.ps1 copied the canonical scripts/uninstall.ps1 file into the archive;
  # registering that payload keeps one source instead of embedding a second script body.

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
