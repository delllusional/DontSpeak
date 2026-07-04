# uninstall.ps1 — THE DontSpeak uninstaller (Windows): the single source of truth.
# web/install.ps1 embeds it verbatim into the install dir and registers it as the
# Settings > Apps UninstallString. packaging_sync.rs pins the copies in sync.
# Removes everything the one-command installer created.
$ErrorActionPreference = 'SilentlyContinue'
# Only the PLACED copy sits next to dontspeak.exe — any other copy (repo checkout,
# standalone download) must target the standard install dir, NOT its own folder:
# $PSScriptRoot alone once resolved to a repo's scripts/ dir and step 6 deleted it.
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
# 4. Downloaded models + logs + config (everything DontSpeak wrote outside the install dir).
Remove-Item "$env:LOCALAPPDATA\DontSpeak" -Recurse -Force
Remove-Item "$env:APPDATA\DontSpeak" -Recurse -Force
# 5. The uninstall registry entry itself (so it drops out of Settings > Apps).
Remove-Item 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\DontSpeak' -Recurse -Force
# 6. Delete the install dir LAST — from a detached cmd after a short delay, so this running
#    script's own folder is free to remove once powershell exits.
if (Test-Path $dest) { Start-Process cmd.exe -ArgumentList '/c',"timeout /t 2 >nul & rmdir /s /q `"$dest`"" -WindowStyle Hidden }
Write-Host 'DontSpeak removed.'
