# disable.ps1 — stop and unregister the dontspeakd daemon.
#
# The logon task runs the Rust dontspeakd.exe; this revert just unregisters that task
# and stops any running instance.
#
# Windows port of linux/disable-daemon.sh. Unregisters the daemon's logon task and stops
# any running instance.
#
# Run:  pwsh -NoProfile -ExecutionPolicy Bypass -File disable.ps1
# Re-enable with enable.ps1.
#
# ##############################################################################
# # NEEDS VALIDATION ON A REAL WINDOWS 10/11 MACHINE. Authored on macOS.        #
# ##############################################################################

param([switch]$Help)

if ($Help) {
    Write-Host @"
Usage: pwsh -NoProfile -File disable.ps1

Stops and unregisters the dontspeakd logon task.

Re-enable: enable.ps1
"@
    exit 0
}

$ErrorActionPreference = 'Continue'

$HOME_DIR    = $env:USERPROFILE
$SETTINGS    = Join-Path $HOME_DIR '.claude\settings.json'
$TASK_NAME   = 'org.dontspeak.daemon'
$INSTALL_DIR = if ($env:DONTSPEAK_INSTALL_DIR) { $env:DONTSPEAK_INSTALL_DIR } else { Join-Path $HOME_DIR '.local\bin' }

# ── 1. Stop + unregister the scheduled task ──────────────────────────────────────────────
Write-Host "==> 1. Stop + unregister the scheduled task"
try {
    $task = Get-ScheduledTask -TaskName $TASK_NAME -ErrorAction SilentlyContinue
    if ($task) {
        Stop-ScheduledTask -TaskName $TASK_NAME -ErrorAction SilentlyContinue
        Unregister-ScheduledTask -TaskName $TASK_NAME -Confirm:$false -ErrorAction Stop
        Write-Host "    unregistered $TASK_NAME"
    } else {
        Write-Host "    (task not found; already disabled)"
    }
} catch {
    Write-Host "WARNING: failed to unregister task: $($_.Exception.Message)"
}

# ── 2. Unwire the voice hooks (symmetric with enable.ps1) ─────────────────────────────────
# Without this the Stop/PostToolUse hooks stay in settings.json and re-spawn the engine on
# demand (via `dontspeak speak` → MCP/IPC), so "disabled" voice could come back to life.
Write-Host ""
Write-Host "==> 2. Unwire Claude Code voice hooks (dontspeak.exe wire-hooks --remove)"
$MCP_BIN = Join-Path $INSTALL_DIR 'dontspeak.exe'
if (Test-Path -LiteralPath $MCP_BIN) {
    & $MCP_BIN wire-hooks --remove
    if ($LASTEXITCODE -ne 0) { Write-Host "    !! wire-hooks --remove exited $LASTEXITCODE" }
} else {
    Write-Host "    (dontspeak.exe not found; leaving settings.json hooks as-is)"
}

Write-Host ""
Write-Host @"
Disabled.
  • The daemon is stopped and unregistered (won't start at next logon).
  • Voice hooks unwired from settings.json (won't re-spawn the engine).
  • Re-enable: enable.ps1
"@
