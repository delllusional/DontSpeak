#!/bin/bash
# enable-daemon.sh — set up the RUST dontspeakd systemd service on Linux
# UNTESTED — validate on Linux before production use.
#
# Linux engine host: on macOS the engine runs in-process inside DontSpeak.app (no
# standalone daemon); this is the Linux equivalent — a headless systemd user service.
# Sets up ~/.config/systemd/user/ds-daemon.service and enables it.
#
# Phase 4 cutover: this runs the RUST dontspeakd binary (parity with macOS), NOT the
# legacy Python dontspeakd.py port. Build + install it first (scripts/install.sh, or
# `cd ../../rust && cargo build --release -p dontspeakd` then copy target/release/dontspeakd
# to $DONTSPEAK_INSTALL_DIR). The systemd unit gets `ExecReload=/bin/kill -HUP $MAINPID`
# so `systemctl --user reload ds-daemon` nudges the daemon's §E.4 hot-reload (it
# re-reads settings.json + rebuilds the STT engine WITHOUT a restart; it also
# mtime-watches settings.json, so a plain GUI write auto-applies too).
#
# Prerequisites:
#   - the Rust dontspeakd binary built + installed (see above)
#   - ~/.claude/keybindings.json has "ctrl+g": "voice:pushToTalk"
#   - ~/.claude/settings.json seeded by `dontspeak wire-hooks` (voice + dontspeak blocks)
#   - /dev/uinput udev rule installed (see udev-rule.txt)

set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
# Phase 4: the installed RUST dontspeakd. Prefer $DONTSPEAK_INSTALL_DIR/dontspeakd (where
# scripts/install.sh puts it; default ~/.local/bin), fall back to the freshly-built rust target.
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
RUST_REL="$(cd "$HERE/.." && pwd)/rust/target/release/dontspeakd"
if [ -x "$INSTALL_DIR/dontspeakd" ]; then
    DAEMON_BIN="$INSTALL_DIR/dontspeakd"
elif [ -x "$RUST_REL" ]; then
    DAEMON_BIN="$RUST_REL"
else
    DAEMON_BIN="$INSTALL_DIR/dontspeakd"  # report the expected path in the preflight error
fi
SYSTEMD_DIR="$HOME/.config/systemd/user"
SERVICE_FILE="$SYSTEMD_DIR/ds-daemon.service"
SERVICE_NAME="ds-daemon"

log() { echo "$(date '+%F %T') $*" >&2; }

# ── Sanity checks ────────────────────────────────────────────────────────────

if [ ! -x "$DAEMON_BIN" ]; then
    log "ERROR: Rust dontspeakd not found or not executable: $DAEMON_BIN"
    log "Build + install it first: run scripts/install.sh, or"
    log "  cd ../../rust && cargo build --release -p dontspeakd && install -m0755 target/release/dontspeakd '$INSTALL_DIR/dontspeakd'"
    exit 1
fi

# jq is required for the JSON config reads/writes below.
if ! command -v jq >/dev/null 2>&1; then
    log "ERROR: jq not installed (needed for JSON config)"
    log "Install with: sudo apt-get install jq   (or: sudo dnf install jq)"
    exit 1
fi

# Verify ~/.claude/keybindings.json. The binding lives at
# .bindings[].bindings["ctrl+g"] (a "Chat" context entry), NOT at the root, so we
# search recursively for any "ctrl+g" value of "voice:pushToTalk".
if ! jq '.. | objects | .["ctrl+g"]?' ~/.claude/keybindings.json 2>/dev/null | grep -q "voice:pushToTalk"; then
    log "WARNING: ~/.claude/keybindings.json does not have \"ctrl+g\": \"voice:pushToTalk\""
    log "Add it manually (see keybindings.snippet.json — it goes under bindings[].bindings)"
fi

# Settings seeding is owned by the installer's `dontspeak wire-hooks` step (it seeds
# our `dontspeak` block AND sets Claude Code's `voice` to {enabled:true, mode:tap} for the
# claude_native path). This script only manages the daemon — it never touches settings.json.

# ── Check udev/permissions ───────────────────────────────────────────────────

if [ ! -w /dev/uinput ]; then
    log "WARNING: /dev/uinput is not writable"
    log "Current user: $(id)"
    log "You may need to:"
    log "  1. Add yourself to the 'input' group: sudo usermod -a -G input \$USER"
    log "  2. Install udev rule: sudo cp udev-rule.txt /etc/udev/rules.d/99-input.rules"
    log "  3. Reload udev: sudo udevadm control --reload"
    log "  4. Log out and back in (or: newgrp input)"
fi

# ── Create systemd service ───────────────────────────────────────────────────

mkdir -p "$SYSTEMD_DIR"

if [ -f "$SERVICE_FILE" ]; then
    log "Backing up existing $SERVICE_FILE"
    cp "$SERVICE_FILE" "$SERVICE_FILE.bak.$(date +%s)"
fi

cat > "$SERVICE_FILE" << SERVICEFILE
[Unit]
Description=dontspeak push-to-talk daemon for Claude Code
Documentation=https://github.com/user/dontspeak
After=graphical-session-pre.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=$DAEMON_BIN
# Phase 4 §E.4 hot-reload: \`systemctl --user reload ds-daemon\` sends SIGHUP,
# which the Rust dontspeakd handles by re-reading settings.json + rebuilding the STT
# engine WITHOUT a restart (no spurious Caps edge, in-flight HOLD ended cleanly).
ExecReload=/bin/kill -HUP \$MAINPID
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
Environment="DEBUG=0"

[Install]
WantedBy=graphical-session.target
SERVICEFILE

log "Created $SERVICE_FILE"

# ── Enable and start ────────────────────────────────────────────────────────

systemctl --user daemon-reload
systemctl --user enable "$SERVICE_NAME"
log "Enabled $SERVICE_NAME"

if systemctl --user is-active --quiet "$SERVICE_NAME"; then
    log "Restarting $SERVICE_NAME"
    systemctl --user restart "$SERVICE_NAME"
else
    log "Starting $SERVICE_NAME"
    systemctl --user start "$SERVICE_NAME"
fi

sleep 1
if systemctl --user is-active --quiet "$SERVICE_NAME"; then
    log "SUCCESS: $SERVICE_NAME is running"
    log "Logs: journalctl --user -u $SERVICE_NAME -f"
else
    log "ERROR: $SERVICE_NAME failed to start"
    log "Check logs: journalctl --user -u $SERVICE_NAME"
    exit 1
fi

log "Setup complete. Restart Claude Code if voice settings changed."
