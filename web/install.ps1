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

# Release-asset arch token is uname-style everywhere: ARM64 → aarch64, AMD64 → x86_64.
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
$zipPattern = "^dontspeak-.+-windows-$arch\.zip$"   # dontspeak-<ver>-windows-<arch>.zip

# Resolve an asset URL off the latest release: by regex pattern (the versioned zip) or
# literal name (checksums.txt — the only fixed-name asset, and the only thing the
# DONTSPEAK_DOWNLOAD_BASE override can still serve).
function Resolve-Asset ($nameOrPattern, [switch]$Pattern) {
  if ($env:DONTSPEAK_DOWNLOAD_BASE) {
    if ($Pattern) { throw "DONTSPEAK_DOWNLOAD_BASE can't resolve the versioned asset '$nameOrPattern' — unset it" }
    return ($env:DONTSPEAK_DOWNLOAD_BASE.TrimEnd('/') + "/$nameOrPattern")
  }
  $rel = Invoke-RestMethod -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $api
  $a = $rel.assets | Where-Object { if ($Pattern) { $_.name -match $nameOrPattern } else { $_.name -eq $nameOrPattern } } |
       Select-Object -First 1
  if ($a) { return $a.browser_download_url } else { return $null }
}

$zipUrl = Resolve-Asset $zipPattern -Pattern
if (-not $zipUrl) { throw "no Windows asset (dontspeak-<ver>-windows-$arch.zip) on the latest release of $repo" }
Say "Windows $arch -> $zipUrl"

if ($dry) { Write-Host "(dry run) would unzip to %LOCALAPPDATA%\Programs\DontSpeak then wire --all"; return }

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("dontspeak-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zipName = [System.IO.Path]::GetFileName(([uri]$zipUrl).AbsolutePath)
  $zip = Join-Path $tmp $zipName
  Say "downloading"
  Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $zipUrl -OutFile $zip

  # SHA-256 verify against checksums.txt (skips cleanly if the release lacks it).
  $sumsUrl = Resolve-Asset 'checksums.txt'
  if ($sumsUrl) {
    try {
      $sums = (Invoke-WebRequest -Headers @{ 'User-Agent' = 'dontspeak-install' } -Uri $sumsUrl).Content
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
      # dontspeak.exe is a GUI-subsystem binary: the call operator (`&`) launches it DETACHED
      # and does not wait, so `$LASTEXITCODE` is never set (a hard error under StrictMode 2.0)
      # AND the script races ahead before the wiring lands — leaving Claude Code unwired.
      # Start-Process -Wait blocks on the process handle regardless of subsystem, and -PassThru
      # surfaces the real exit code.
      $wp = Start-Process -FilePath $cli -ArgumentList 'wire','--all' -Wait -PassThru -WindowStyle Hidden
      if ($wp.ExitCode -ne 0) { Warn "wire --all reported an issue (exit $($wp.ExitCode))" }
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
  # when the uninstaller RUNS, not now. The dir is deleted last, from a detached cmd, because
  # this script lives inside it (a running script can't delete its own folder).
  @'
# DontSpeak uninstaller — invoked by Windows (Settings > Apps) via the registered
# UninstallString, or run directly. Removes everything the one-command installer created.
$ErrorActionPreference = 'SilentlyContinue'
$dest = $PSScriptRoot
# 1. Stop the resident app + engine + warm helper so no files are locked.
Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
# 2. Unwire every client (MCP server + voice hooks) via the app's own remover — waited.
$cli = Join-Path $dest 'dontspeak.exe'
if (Test-Path $cli) { Start-Process -FilePath $cli -ArgumentList 'wire','--all','--remove' -Wait -WindowStyle Hidden }
# 3. Start-menu shortcut + start-at-login entry.
Remove-Item (Join-Path ([Environment]::GetFolderPath('Programs')) 'DontSpeak.lnk') -Force
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name DontSpeak
# 4. Downloaded models + logs + config (everything DontSpeak wrote outside the install dir).
Remove-Item "$env:LOCALAPPDATA\DontSpeak" -Recurse -Force
Remove-Item "$env:APPDATA\DontSpeak" -Recurse -Force
# 5. The uninstall registry entry itself (so it drops out of Settings > Apps).
Remove-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak' -Recurse -Force
# 6. Delete the install dir LAST — from a detached cmd after a short delay, so this running
#    script's own folder is free to remove once powershell exits.
Start-Process cmd.exe -ArgumentList '/c',"timeout /t 2 >nul & rmdir /s /q `"$dest`"" -WindowStyle Hidden
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
