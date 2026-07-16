# uninstall.ps1 — THE DontSpeak uninstaller (Windows): the single source of truth.
# build-portable.ps1 ships this file in the archive; scripts/install/web/install.ps1 registers that
# payload as the Settings > Apps UninstallString. packaging_sync.rs pins the route.
# Removes everything the one-command installer created. Missing resources are clean
# skips; real cleanup failures are collected and reported instead of hidden.
$ErrorActionPreference = 'Stop'
$failures = New-Object 'System.Collections.Generic.List[string]'
function Invoke-CleanupStep ($name, [scriptblock]$action) {
  try { & $action }
  catch { $failures.Add("${name}: $($_.Exception.Message)") }
}
# Only the PLACED copy sits next to dontspeak.exe — any other copy (repo checkout,
# standalone download) must target the standard install dir, NOT its own folder:
# $PSScriptRoot alone once resolved to a repo's scripts/ dir and step 7 deleted it.
$dest = if ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'dontspeak.exe'))) { $PSScriptRoot } else { Join-Path $env:LOCALAPPDATA 'Programs\DontSpeak' }
# 1. Stop the resident app + engine + warm helper so no files are locked.
Invoke-CleanupStep 'stop running processes' {
  $running = @(Get-Process ds-winui,dontspeak,ds-helper -ErrorAction SilentlyContinue)
  if ($running) { $running | Stop-Process -Force -ErrorAction Stop }
}
Start-Sleep -Milliseconds 500
# 2. Unwire every client (MCP server + voice hooks) via the app's own remover — waited.
$cli = Join-Path $dest 'dontspeak.exe'
Invoke-CleanupStep 'remove client integrations' {
  if (Test-Path -LiteralPath $cli) {
    $wire = Start-Process -FilePath $cli -ArgumentList 'wire','--all','--remove' -Wait -PassThru -WindowStyle Hidden
    if ($wire.ExitCode -ne 0) { throw "dontspeak wire exited with code $($wire.ExitCode)" }
  }
}
# 3. Start-menu shortcut + start-at-login entry.
Invoke-CleanupStep 'remove Start-menu shortcut' {
  $shortcut = Join-Path ([Environment]::GetFolderPath('Programs')) 'DontSpeak.lnk'
  if (Test-Path -LiteralPath $shortcut) { Remove-Item -LiteralPath $shortcut -Force }
}
Invoke-CleanupStep 'remove start-at-login entry' {
  $runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
  if (Get-ItemProperty $runKey -Name DontSpeak -ErrorAction SilentlyContinue) {
    Remove-ItemProperty $runKey -Name DontSpeak
  }
}
# 4. Remove only our exact install directory from the user PATH.
Invoke-CleanupStep 'remove install directory from user PATH' {
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  $destKey = $dest.TrimEnd('\')
  $keptPath = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ine $destKey }) -join ';'
  [Environment]::SetEnvironmentVariable('Path', $keptPath, 'User')
}
# 5. Downloaded models + logs + config (everything DontSpeak wrote outside the install dir).
Invoke-CleanupStep 'remove application data' {
  foreach ($path in @("$env:LOCALAPPDATA\DontSpeak", "$env:APPDATA\DontSpeak")) {
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Recurse -Force }
  }
}
# 6. The uninstall registry entry itself (so it drops out of Settings > Apps).
Invoke-CleanupStep 'remove uninstall registration' {
  $uninstallKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak'
  if (Test-Path $uninstallKey) { Remove-Item $uninstallKey -Recurse -Force }
}
# 7. Delete the install dir LAST — from a detached cmd after a short delay, so this running
#    script's own folder is free to remove once powershell exits.
Invoke-CleanupStep 'schedule install-directory removal' {
  if (Test-Path -LiteralPath $dest) {
    Start-Process cmd.exe -ArgumentList '/c',"timeout /t 2 >nul & rmdir /s /q `"$dest`"" -WindowStyle Hidden
  }
}

if ($failures.Count -gt 0) {
  Write-Error ("DontSpeak was only partially removed:`n  " + ($failures -join "`n  ")) -ErrorAction Continue
  exit 1
}
Write-Host 'DontSpeak removed; install-directory cleanup is scheduled.'
