#!/usr/bin/env bash
# uninstall.sh — completely remove the Linux DontSpeak install (the mirror of
# scripts/install.sh + apps/linux/install-gui.sh, and the Linux analogue of the
# macOS scripts/uninstall.sh). Stops the running GUI host, un-wires the Claude Code
# hooks, deletes the installed binaries, the .desktop launchers, and ALL app data /
# caches / state, so the next install starts truly clean.
#
# With --udev it ALSO removes the /dev/uinput udev rule (needs sudo). Your `input`
# group membership is left intact (harmless, and removing it would need a re-login).
#
# Idempotent: every piece is removed best-effort; missing ones are skipped.
#
#   apps/linux/uninstall.sh           # remove binaries + data + launchers
#   apps/linux/uninstall.sh --udev    # ALSO remove the udev rule (sudo)
set -uo pipefail   # deliberately NOT -e: one missing piece must not abort the teardown

RM_UDEV=0
for a in "$@"; do
  case "$a" in
    --udev) RM_UDEV=1 ;;
    -h | --help)
      # Header comment only, minus the shebang: from line 2, stop at the first non-# line.
      awk 'NR > 1 && !/^#/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"
      exit 0
      ;;
    *) echo "uninstall: ignoring unknown arg '$a'" >&2 ;;
  esac
done

H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
# XDG roots (see ds-config paths.rs: config/state/cache under the lowercase app id).
CONFIG_DIR="${XDG_CONFIG_HOME:-$H/.config}/dontspeak"
STATE_DIR="${XDG_STATE_HOME:-$H/.local/state}/dontspeak"
CACHE_DIR="${XDG_CACHE_HOME:-$H/.cache}/dontspeak"
APPS_DIR="${XDG_DATA_HOME:-$H/.local/share}/applications"

echo "==> 1. stop the running GUI host + warm helper"
pkill -x ds-gtk 2>/dev/null || true
pkill -f "ds-helper" 2>/dev/null || true

echo "==> 2. un-wire all client integrations (before deleting the binary)"
if [ -x "$INSTALL_DIR/dontspeak" ]; then
  "$INSTALL_DIR/dontspeak" wire --all --remove 2>/dev/null \
    || echo "   (wire --all --remove failed or nothing to remove)"
else
  echo "   (no $INSTALL_DIR/dontspeak — skipping hook removal)"
fi

echo "==> 3. remove the installed binaries"
for b in ds-gtk dontspeak ds-helper; do rm -f "$INSTALL_DIR/$b"; done

echo "==> 4. remove the .desktop launchers (app menu + autostart)"
rm -f "$APPS_DIR/dontspeak.desktop" \
      "${XDG_CONFIG_HOME:-$H/.config}/autostart/dontspeak.desktop"
# Refresh the menu cache so the entry disappears without a re-login (best-effort).
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true

echo "==> 5. remove app data, downloaded models, caches, state"
# config_dir (settings/speakers/narration spec) + state + the model/onnxruntime cache.
rm -rf "$CONFIG_DIR" "$STATE_DIR" "$CACHE_DIR"

if [ "$RM_UDEV" = "1" ]; then
  echo "==> 6. remove the /dev/uinput udev rule (sudo)"
  sudo rm -f /etc/udev/rules.d/99-ds-input.rules 2>/dev/null || true
  sudo udevadm control --reload 2>/dev/null || true
  sudo udevadm trigger 2>/dev/null || true
else
  echo "==> 6. (udev rule left intact — pass --udev to also remove it; your 'input' group membership is kept)"
fi

echo
echo "Done. DontSpeak removed. Reinstall with: scripts/install.sh && apps/linux/install-gui.sh"
