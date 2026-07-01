# install.ps1 — build + install the DontSpeak RUST stack on Windows (parity with scripts/install.sh).
#
# Builds the Rust workspace and installs the CLI binaries
#   dontspeak.exe, ds-helper.exe
# into one install dir (default %USERPROFILE%\.local\bin, override with
# $env:DONTSPEAK_INSTALL_DIR), merges the keybindings snippet, wires the voice hooks via
# `dontspeak.exe wire` (the single cross-platform definition — no placeholder
# substitution here any more), builds the WinUI host app (which hosts the engine
# in-process via ds_core.dll), and PRINTS the next steps.
# There is no more speak.py / uv / Kokoro model download — the Rust dontspeak.exe
# does in-process Kokoro synth and fetches its model assets to the per-OS data dir on
# first synth.
#
# What it does:
#   1. Build the Rust workspace (cargo --release: the 2 console bins + ds_core.dll) and
#      install the .exe's -> $INSTALL_DIR
#   2. Copy hooks -> %USERPROFILE%\.claude\hooks\ with the binary path substituted
#   3. Install keybindings.json (space:null, ctrl+g:voice:pushToTalk)
#   4. Wire the voice hooks into %USERPROFILE%\.claude\settings.json via
#      `dontspeak.exe wire` (the single cross-platform hook definition + merge)
#   5. Report any missing prerequisites (cargo / pwsh)
#
# Run:  pwsh -NoProfile -ExecutionPolicy Bypass -File install.ps1
#
# ##############################################################################
# # NEEDS VALIDATION ON A REAL WINDOWS 10/11 MACHINE. Authored on macOS — the     #
# # cargo build + .exe install + hook substitution are host-validated only.       #
# ##############################################################################

$ErrorActionPreference = 'Stop'

$REPO_WIN   = $PSScriptRoot
$RUST_DIR   = (Resolve-Path (Join-Path $REPO_WIN '..\..\rust')).Path
$HOME_DIR   = $env:USERPROFILE
$CLAUDE     = Join-Path $HOME_DIR '.claude'
$SETTINGS   = Join-Path $CLAUDE 'settings.json'
$KEYBINDS   = Join-Path $CLAUDE 'keybindings.json'
# Install dir for the binaries: override with $env:DONTSPEAK_INSTALL_DIR (mirrors the
# DONTSPEAK_INSTALL_DIR env var honored by scripts/install.sh; default %USERPROFILE%\.local\bin).
$INSTALL_DIR = if ($env:DONTSPEAK_INSTALL_DIR) { $env:DONTSPEAK_INSTALL_DIR } else { Join-Path $HOME_DIR '.local\bin' }

function Backup-File {
    param([string]$Path)
    if (Test-Path -LiteralPath $Path) {
        $bak = "$Path.bak.$((Get-Date).ToString('yyyyMMddHHmmss'))"
        Copy-Item -LiteralPath $Path -Destination $bak -Force
        Write-Host "    backed up $Path -> $bak"
    }
}

# Merge the keybindings snippet into an existing keybindings.json WITHOUT clobbering the
# user's other contexts. keybindings.json is { "bindings": [ { context, bindings{} }, ... ] }
# — an array keyed by the `context` name, NOT by index, so the generic Merge-Json (which
# replaces arrays wholesale) is wrong here. For each context in the snippet we find the same
# context in the existing array and merge its inner key map (snippet keys win); if the
# context is absent we append it. Returns the merged top-level object.
function Merge-Keybindings {
    param($Existing, $Snippet)
    if ($null -eq $Existing) { return $Snippet }
    # Carry over any top-level keys the user has ($schema, etc.); snippet's scalars win.
    $out = $Existing | Select-Object *
    foreach ($prop in $Snippet.PSObject.Properties) {
        if ($prop.Name -ne 'bindings') {
            if ($out.PSObject.Properties[$prop.Name]) { $out.$($prop.Name) = $prop.Value }
            else { $out | Add-Member -NotePropertyName $prop.Name -NotePropertyValue $prop.Value -Force }
        }
    }
    # Normalize the existing bindings array to a mutable list.
    $list = New-Object System.Collections.ArrayList
    if ($out.PSObject.Properties['bindings'] -and $out.bindings) {
        foreach ($c in @($out.bindings)) { [void]$list.Add($c) }
    }
    foreach ($sctx in @($Snippet.bindings)) {
        $match = $null
        foreach ($ectx in $list) { if ($ectx.context -eq $sctx.context) { $match = $ectx; break } }
        if ($null -eq $match) {
            [void]$list.Add($sctx)   # context absent — append the whole snippet context
        } else {
            # Merge the inner key map: snippet keys (space:null, ctrl+g:...) win; keep the rest.
            if (-not $match.PSObject.Properties['bindings'] -or $null -eq $match.bindings) {
                $match | Add-Member -NotePropertyName 'bindings' -NotePropertyValue ([pscustomobject]@{}) -Force
            }
            foreach ($kb in $sctx.bindings.PSObject.Properties) {
                if ($match.bindings.PSObject.Properties[$kb.Name]) { $match.bindings.$($kb.Name) = $kb.Value }
                else { $match.bindings | Add-Member -NotePropertyName $kb.Name -NotePropertyValue $kb.Value -Force }
            }
        }
    }
    if ($out.PSObject.Properties['bindings']) { $out.bindings = @($list) }
    else { $out | Add-Member -NotePropertyName 'bindings' -NotePropertyValue @($list) -Force }
    return $out
}

# NOTE: hook wiring into ~/.claude/settings.json is NOT done here — it's the shared,
# cross-platform `dontspeak.exe wire` (Rust) step below, the SINGLE source of
# truth for the hook set + merge on every platform. (A PowerShell `Merge-Hooks` used to
# live here and drifted from the Rust merge; it's gone — never reintroduce a per-platform
# copy.) Our own settings live in ~/.dontspeak/config.toml, also written via the Rust core.

# ── Prerequisite checks (report; hard-fail only on cargo, which the build needs) ─────────
Write-Host "==> 0. Prerequisites"
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found — install the Rust toolchain (https://rustup.rs) and re-run. The Phase-4 install builds the Rust workspace."
}
Write-Host "    found cargo"
if (Get-Command pwsh -ErrorAction SilentlyContinue) { Write-Host "    found pwsh" }
else { Write-Host "    !! pwsh (PowerShell 7) recommended; the hooks also run under Windows PowerShell 5.1." }

New-Item -ItemType Directory -Force -Path $INSTALL_DIR | Out-Null
Write-Host "    install dir: $INSTALL_DIR"

# ── 0b. build the Rust workspace + install the .exe binaries ────────────────────────
# Mirrors scripts/install.sh: plain --release for the merged dontspeak bin + the kokoro
# helper bin (the kokoro helper is a [[bin]] in ds-tts, so `-p ds-tts` MUST be in scope to
# select --bin ds-helper). The merged dontspeak bin is the MCP server, the hook executor,
# and the wire tool, dispatched by subcommand. The engine has no standalone binary — it is
# hosted in-process by the WinUI app via ds_core.dll (built below).
Write-Host ""
Write-Host "==> 0c. build the Rust workspace"
Push-Location $RUST_DIR
try {
    # -p ds-core builds the cdylib (ds_core.dll) the WinUI app P/Invokes to host the engine
    # in-process (the WinUI app is the resident host — the old ds-tray crate was merged into
    # it; see windows/winui/TrayIcon.cs).
    cargo build --release `
        -p dontspeak -p ds-core -p ds-tts `
        --bin dontspeak --bin ds-helper
    if ($LASTEXITCODE -ne 0) { throw "cargo build (console bins) failed" }
} finally {
    Pop-Location
}

# Stop any engine from a PREVIOUS install BEFORE copying over its binaries. Two
# reasons: (1) on Windows a running .exe is locked against overwrite, so the
# Copy-Item below would fail outright; (2) more importantly, leaving the old engine
# alive lets it race the new one for the RPC socket, which is heard as the same
# reply spoken TWICE after the upgrade. The engine also self-evicts on boot (see
# the engine's boot.rs evict_stale_engine), but stopping here makes the handoff clean
# and unblocks the copy. The MCP server / hook CLI (`dontspeak.exe`) is a stateless
# client that holds no socket, so it is intentionally left alone.
Write-Host "==> 0c. stop any DontSpeak engine from a previous install"
foreach ($p in 'ds-winui','ds-helper') {
    Get-Process -Name $p -ErrorAction SilentlyContinue | ForEach-Object {
        Write-Host "    stopping $($_.ProcessName) (pid $($_.Id))"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "==> 0d. install the binaries -> $INSTALL_DIR"
$REL    = Join-Path $RUST_DIR 'target\release'
$bins = @(
    @{ src = (Join-Path $REL    'dontspeak.exe');         dst = (Join-Path $INSTALL_DIR 'dontspeak.exe') },
    @{ src = (Join-Path $REL    'ds-helper.exe');  dst = (Join-Path $INSTALL_DIR 'ds-helper.exe') }
)
foreach ($b in $bins) {
    Copy-Item -LiteralPath $b.src -Destination $b.dst -Force
    Write-Host "    installed $(Split-Path $b.dst -Leaf)"
}
# TODO(on-target): Authenticode-sign the .exe's here if signtool + a code-signing cert are
# present (otherwise first run hits SmartScreen). Left unsigned by default (ad-hoc signing
# has no Windows analog; macOS scripts/install.sh ad-hoc signs via codesign).

# ── 0e. register the MCP server with Claude Code (the control surface) ───────────────────
# The dontspeak MCP server is registered together with the voice hooks by
# `dontspeak.exe wire claude_code` in step 3 below (which owns the ~/.claude.json merge) —
# no external `claude` CLI needed, and it's the SAME cross-platform path macOS/Linux use.

# ── 0f. WinUI app — the resident host + Fluent UI (needs the .NET SDK) ────────────────────
# The macOS SwiftUI-app analogue, and the Windows resident host: it shows the tray icon,
# hosts the engine in-process (ds_core.dll), and has the Status/Tools window.
# Framework-dependent unpackaged: needs the Windows App Runtime (preinstalled on most
# Win11). Published into $INSTALL_DIR\winui. The WinUI app is the ONLY engine host on
# Windows (it hosts the engine in-process via ds_core.dll) — so if dotnet is missing it is
# skipped and there is NO engine host: install the .NET SDK and re-run to get it.
Write-Host ""
Write-Host "==> 0f. WinUI app (resident host + Fluent UI)"
$WINUI_PROJ = Join-Path $REPO_WIN 'winui\DontSpeak.WinUI.csproj'
if ((Get-Command dotnet -ErrorAction SilentlyContinue) -and (Test-Path $WINUI_PROJ)) {
    $WINUI_OUT = Join-Path $INSTALL_DIR 'winui'
    dotnet publish $WINUI_PROJ -c Release -o $WINUI_OUT --nologo
    if ($LASTEXITCODE -eq 0) { Write-Host "    published WinUI app -> $WINUI_OUT" }
    else { Write-Host "    !! dotnet publish failed; no engine host until it builds." }
} else {
    Write-Host "    skipped (no .NET SDK on PATH) — no engine host; install the .NET SDK and re-run."
}

# The speak/narrate hooks are EXEC-FORM (the settings snippet calls the .exe directly —
# see below), so there are no pwsh hook wrappers and no hook helper scripts at all —
# matching the macOS/Linux design ("no shell wrappers").

# ── keybindings (deep-merge — never clobber other contexts) ───────────────────────────
Write-Host ""
Write-Host "==> 2. keybindings -> $KEYBINDS"
New-Item -ItemType Directory -Force -Path $CLAUDE | Out-Null
$kbSnippet = Get-Content -LiteralPath (Join-Path $REPO_WIN 'keybindings.snippet.json') -Raw | ConvertFrom-Json
if (Test-Path -LiteralPath $KEYBINDS) {
    Backup-File $KEYBINDS
    $kbExisting = $null
    try { $kbExisting = Get-Content -LiteralPath $KEYBINDS -Raw | ConvertFrom-Json } catch {
        Write-Host "    !! existing keybindings.json is not valid JSON; leaving it AS-IS and skipping merge."
        Write-Host "       Add manually:  Chat context -> { \"space\": null, \"ctrl+g\": \"voice:pushToTalk\" }"
        $kbExisting = $null
    }
    if ($null -ne $kbExisting) {
        $kbMerged = Merge-Keybindings $kbExisting $kbSnippet
        $kbMerged | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $KEYBINDS -Encoding utf8
        Write-Host "    deep-merged keybindings (space:null, ctrl+g:voice:pushToTalk; other contexts preserved)"
    }
} else {
    $kbSnippet | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $KEYBINDS -Encoding utf8
    Write-Host "    installed keybindings.json (space:null, ctrl+g:voice:pushToTalk)"
}

# ── 3. settings deep-merge ───────────────────────────────────────────────────────────────
Write-Host ""
Write-Host "==> 3. wire client integrations (Claude Code hooks + MCP, Desktop MCP, Codex hooks)"
# One cross-platform per-client step: dontspeak owns the ONE integration definition + a SAFE
# merge (additive, idempotent, timestamped backup first, malformed file left untouched), so the
# wired set never drifts from macOS/Linux. `wire claude_code` does hooks AND the ~/.claude.json
# MCP registration together; Desktop/Codex self-skip if absent.
# (undo: dontspeak.exe wire <client> --remove)
$_dsBin = Join-Path $INSTALL_DIR 'dontspeak.exe'
& $_dsBin wire --all   # every known client (the list lives in the binary); each self-skips if absent

# NOTE: there is NO Kokoro/speak.py step anymore. The Rust dontspeak.exe does
# in-process Kokoro synthesis and downloads its model assets (onnx + voices + the
# onnxruntime dll) to the per-OS data dir on first synth — nothing to install here.

Write-Host ""
Write-Host @"
Done (binaries built + installed; config merged).

Installed:
  • $INSTALL_DIR\{dontspeak,ds-helper}.exe
  • $INSTALL_DIR\winui\ds-winui.exe (the resident host + Fluent UI), if the .NET SDK was found.
  • MCP server 'dontspeak' registered with Claude Code (user scope), if the claude CLI was found.

Next:
  • Restart Claude Code so it loads the 'dontspeak' MCP server + the merged hooks.
  • Ensure Claude Code voice is on (/voice on).
  • Launch the resident host (the Windows analogue of the macOS menu-bar app), which
    hosts the engine IN-PROCESS — it is REQUIRED for the warm engine + Caps-Lock:
    "$INSTALL_DIR\winui\ds-winui.exe". It shows a status-dot tray icon (idle /
    orange recording / purple speaking) and a Status/Tools window. Closing the window
    hides it to the tray; quit from the tray's Exit. Enable "Start at login" from the
    tray menu. (No .NET SDK? Re-run this script after installing it — without the WinUI
    host there is no engine.)
  • First synth downloads the Kokoro model assets to the per-OS data dir (one-time).
  • The .exe's are UNSIGNED — first run may hit SmartScreen ('More info -> Run anyway'),
    or Authenticode-sign them with your cert.

NOTE: all Windows code in this port needs validation on a real Windows 10/11 machine.
"@
