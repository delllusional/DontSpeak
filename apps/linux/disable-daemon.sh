#!/bin/bash
# disable-daemon.sh — uninstall the dontspeakd systemd service (Linux engine host;
# macOS hosts the engine in DontSpeak.app instead — quit the app there).
#
# Phase-4: enable-daemon.sh now runs the RUST dontspeakd, but this unload path is
# binary-agnostic — it just stops + removes the systemd --user unit, regardless of
# which binary it ran.

set -eu

SERVICE_NAME="ds-daemon"
SERVICE_FILE="$HOME/.config/systemd/user/$SERVICE_NAME.service"

log() { echo "$(date '+%F %T') $*" >&2; }

if systemctl --user is-active --quiet "$SERVICE_NAME"; then
    log "Stopping $SERVICE_NAME"
    systemctl --user stop "$SERVICE_NAME"
fi

if systemctl --user is-enabled "$SERVICE_NAME" 2>/dev/null; then
    log "Disabling $SERVICE_NAME"
    systemctl --user disable "$SERVICE_NAME"
fi

if [ -f "$SERVICE_FILE" ]; then
    log "Backing up $SERVICE_FILE"
    cp "$SERVICE_FILE" "$SERVICE_FILE.bak.$(date +%s)"
    rm "$SERVICE_FILE"
fi

systemctl --user daemon-reload

log "Disabled $SERVICE_NAME"
log "Logs: journalctl --user -u $SERVICE_NAME"
