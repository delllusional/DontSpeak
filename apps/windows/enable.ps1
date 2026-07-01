# enable.ps1 — wire the DontSpeak voice hooks + start the dontspeakd daemon.
#
# ##############################################################################
# # The logon task runs the Rust dontspeakd.exe (built + installed by install.ps1). #
# # NOTE Windows has NO SIGHUP: the daemon's settings.json mtime-watch is the      #
# # ONLY hot-reload path there (a config change auto-applies on the next tick —    #
# # no restart, no explicit nudge). A named-event / service-control reload         #
# # trigger is a further on-target TODO.                                           #
# ##############################################################################
#
# Windows port of linux/enable-daemon.sh. Ensures voice is enabled, the hooks are wired,
# and registers a Task Scheduler logon task that keeps the daemon resident.
#
# Abort-safe ordering (mirrors the macOS enable-daemon): the settings merge happens first
# and is the ONLY mutation that needs undoing; the scheduled task is registered LAST, so if
# task registration fails the settings are restorable from the timestamped .bak.
#
# Runs the Rust dontspeakd.exe (built + installed by install.ps1) as the LOGGED-IN USER —
# never SYSTEM, which is a different session and cannot GetForegroundWindow / SendInput to
# your terminal.
#
# Run:  pwsh -NoProfile -ExecutionPolicy Bypass -File enable.ps1
# Revert with disable.ps1.
#
# ##############################################################################
# # NEEDS VALIDATION ON A REAL WINDOWS 10/11 MACHINE. Authored on macOS.        #
# ##############################################################################

param([switch]$Help)

if ($Help) {
    Write-Host @"
Usage: pwsh -NoProfile -File enable.ps1

Ensures Claude Code voice is enabled and wires the DontSpeak hooks. Registers the
dontspeakd daemon as a Task Scheduler logon task.

Requires:
  - %USERPROFILE%\.claude\settings.json exists (run install.ps1 first)
  - %USERPROFILE%\.claude\keybindings.json has ctrl+g -> voice:pushToTalk

Revert: disable.ps1
"@
    exit 0
}

$ErrorActionPreference = 'Stop'

$DAEMON_DIR   = $PSScriptRoot
$HOME_DIR     = $env:USERPROFILE
$INSTALL_DIR  = if ($env:DONTSPEAK_INSTALL_DIR) { $env:DONTSPEAK_INSTALL_DIR } else { Join-Path $HOME_DIR '.local\bin' }
$DAEMON_EXE   = Join-Path $INSTALL_DIR 'dontspeakd.exe'
$CLAUDE       = Join-Path $HOME_DIR '.claude'
$SETTINGS     = Join-Path $CLAUDE 'settings.json'
$KEYBINDS     = Join-Path $CLAUDE 'keybindings.json'
$DAEMON_LOG   = Join-Path $CLAUDE 'daemon.log'
$TASK_NAME    = 'org.dontspeak.daemon'

function Backup-File {
    param([string]$Path)
    if (Test-Path -LiteralPath $Path) {
        $bak = "$Path.bak.$((Get-Date).ToString('yyyyMMddHHmmss'))"
        Copy-Item -LiteralPath $Path -Destination $bak -Force
        Write-Host "    backed up $Path -> $bak"
        return $bak
    }
    return $null
}

# ── 0. Preflight ─────────────────────────────────────────────────────────────────────────
Write-Host "==> 0. Preflight checks"
if (-not (Test-Path -LiteralPath $SETTINGS)) {
    Write-Host "ABORT: settings.json missing: $SETTINGS  (run install.ps1 first)"
    exit 1
}
Write-Host "    found $SETTINGS"

# The daemon is INERT without the ctrl+g binding (it would just inject BEL into the input),
# so a missing binding is a hard ABORT, not a warning — fix it by running install.ps1, which
# deep-merges the binding without clobbering your other contexts.
if (-not (Test-Path -LiteralPath $KEYBINDS)) {
    Write-Host "ABORT: keybindings.json missing: $KEYBINDS"
    Write-Host "       The daemon's Ctrl+G is inert unless ctrl+g -> voice:pushToTalk is bound."
    Write-Host "       Run install.ps1 first:  pwsh -NoProfile -ExecutionPolicy Bypass -File install.ps1"
    exit 1
}
$hasBinding = $false
try {
    $kb = Get-Content -LiteralPath $KEYBINDS -Raw | ConvertFrom-Json
    foreach ($ctx in @($kb.bindings)) {
        if ($ctx.context -eq 'Chat' -and $ctx.bindings.'ctrl+g' -eq 'voice:pushToTalk') { $hasBinding = $true }
    }
} catch {
    Write-Host "ABORT: could not parse $KEYBINDS : $($_.Exception.Message)"
    exit 1
}
if ($hasBinding) {
    Write-Host "    keybindings.json has ctrl+g -> voice:pushToTalk"
} else {
    Write-Host "ABORT: ctrl+g -> voice:pushToTalk not found in $KEYBINDS (the daemon would be inert)."
    Write-Host "       Run install.ps1 first to merge the binding:"
    Write-Host "       pwsh -NoProfile -ExecutionPolicy Bypass -File install.ps1"
    exit 1
}

# ── 1. Merge settings: ensure voice enabled + hooks wired ────────────────────────────────
Write-Host ""
Write-Host "==> 1. settings.json -> Claude Code voice enabled+tap, wire hooks"
$bak = Backup-File $SETTINGS
try {
    $json = Get-Content -LiteralPath $SETTINGS -Raw | ConvertFrom-Json

    # Ensure Claude Code's OWN voice block is enabled + tap (the claude_native path drives
    # CC's dictation via Ctrl+G, which needs CC in tap mode). Our own config lives under the
    # separate `dontspeak` block, seeded by install's wire.
    if (-not $json.PSObject.Properties['voice']) {
        $json | Add-Member -NotePropertyName 'voice' -NotePropertyValue ([pscustomobject]@{ enabled = $true; mode = 'tap' })
    } else {
        if ($json.voice.PSObject.Properties['enabled']) { $json.voice.enabled = $true } else { $json.voice | Add-Member -NotePropertyName 'enabled' -NotePropertyValue $true }
        if ($json.voice.PSObject.Properties['mode'])    { $json.voice.mode = 'tap' }    else { $json.voice | Add-Member -NotePropertyName 'mode' -NotePropertyValue 'tap' }
    }

    # NOTE: the voice HOOKS are wired by `dontspeak.exe wire` (step 1b below) —
    # the SINGLE cross-platform source of truth for the full canonical hook set + a safe
    # merge. This used to hand-roll just the Stop + PostToolUse hooks here (2 of the 8),
    # which could drift from the Rust merge and, if enable.ps1 was run without install.ps1,
    # left the other 6 hooks unwired. Here we only flip the Claude Code voice block on.
    $json | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $SETTINGS -Encoding utf8
    Write-Host "    voice enabled"
} catch {
    Write-Host "ABORT: failed to update settings.json: $($_.Exception.Message)"
    # ConvertTo-Json / Set-Content can throw partway and leave settings.json corrupt; restore
    # the timestamped backup so we never strand the user with a half-written config.
    if ($bak) {
        Copy-Item -LiteralPath $bak -Destination $SETTINGS -Force
        Write-Host "       restored settings.json from $bak"
    }
    exit 1
}

# ── 1b. Wire the voice hooks via the shared cross-platform installer step ─────────────────
Write-Host ""
Write-Host "==> 1b. Wire client integrations (Claude Code hooks + MCP, Desktop MCP, Codex hooks)"
$MCP_BIN = Join-Path $INSTALL_DIR 'dontspeak.exe'
if (Test-Path -LiteralPath $MCP_BIN) {
    foreach ($client in 'claude_code', 'claude_desktop', 'codex') {
        & $MCP_BIN wire $client
        if ($LASTEXITCODE -ne 0) { Write-Host "    !! wire $client exited $LASTEXITCODE; check the engine log" }
    }
} else {
    Write-Host "    !! $MCP_BIN not found — run install.ps1 first to install the binaries + wire hooks."
}

# ── 2. Register the daemon as a logon task (registered LAST so a failure is restorable) ──
Write-Host ""
Write-Host "==> 2. Register Task Scheduler logon task: $TASK_NAME"
try {
    # Run the Rust dontspeakd.exe (built + installed by install.ps1).
    if (-not (Test-Path -LiteralPath $DAEMON_EXE)) {
        Write-Host "ABORT: $DAEMON_EXE not found. Run install.ps1 first (it builds + installs dontspeakd.exe via 'cargo build --bin dontspeakd')."
        exit 1
    }
    $inner = "`"$DAEMON_EXE`""
    Write-Host "    using dontspeakd.exe"

    # Task Scheduler can't redirect stream output itself, so wrap in cmd /c "... >> log 2>&1".
    $cmdLine = "/c $inner >> `"$DAEMON_LOG`" 2>&1"
    $action  = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument $cmdLine -WorkingDirectory $DAEMON_DIR
    $trigger = New-ScheduledTaskTrigger -AtLogOn
    $settings = New-ScheduledTaskSettingsSet `
        -AllowStartIfOnBatteries `
        -DontStopIfGoingOnBatteries `
        -ExecutionTimeLimit ([TimeSpan]::Zero)   # Zero == no time limit (run indefinitely)
    # Run as the interactive logged-in user (NOT SYSTEM) so it can see the foreground window.
    $principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited

    Register-ScheduledTask -TaskName $TASK_NAME -Action $action -Trigger $trigger `
        -Settings $settings -Principal $principal -Force -ErrorAction Stop | Out-Null
    Write-Host "    registered $TASK_NAME (runs as $env:USERDOMAIN\$env:USERNAME at logon)"

    # Stop any engine already running (a previous version's daemon or the in-app
    # WinUI host) BEFORE we start the task's dontspeakd, so the new engine doesn't
    # race the old one for the RPC socket — that race is heard as the same reply
    # spoken twice. The new engine also self-evicts on boot (dontspeakd
    # evict_stale_engine), so this is the belt to that braces; the MCP/hook CLI
    # (`dontspeak.exe`) holds no socket and is left running.
    foreach ($p in 'ds-winui','dontspeakd','ds-helper') {
        Get-Process -Name $p -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }

    # Start it now so you don't have to log out/in to try it.
    Start-ScheduledTask -TaskName $TASK_NAME -ErrorAction SilentlyContinue
} catch {
    Write-Host "ABORT: failed to register the scheduled task: $($_.Exception.Message)"
    if ($bak) {
        Copy-Item -LiteralPath $bak -Destination $SETTINGS -Force
        Write-Host "       restored settings.json from $bak (cutover rolled back)."
    }
    exit 1
}

# ── 3. Verify ────────────────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "==> 3. Verify"
Start-Sleep -Seconds 1
try {
    $task = Get-ScheduledTask -TaskName $TASK_NAME -ErrorAction Stop
    Write-Host "    task state: $($task.State)"
} catch {
    Write-Host "WARNING: could not read task state: $($_.Exception.Message)"
}

Write-Host ""
Write-Host @"
Setup complete.
  • Voice enabled and hooks wired.
  • Daemon registered as a logon task and started now.
  • Logs: $DAEMON_LOG  (set DONTSPEAK_DEBUG=1 in the task for per-event lines)
  • Test manually:  & "$DAEMON_EXE"
  • Revert:         pwsh -NoProfile -File "$DAEMON_DIR\disable.ps1"
"@
