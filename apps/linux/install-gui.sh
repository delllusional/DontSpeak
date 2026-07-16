#!/bin/bash
# install-gui.sh — build + install the DontSpeak Linux GUI host (GTK4 + libadwaita).
#
# The Linux analogue of building + installing DontSpeak.app on macOS: it installs the
# `ds-gtk` host, which HOSTS the engine in-process (the same ds-core C ABI the
# macOS/Windows apps use) and provides the tray, health panel, and dictation overlay. This
# GTK host is the ONLY engine host on Linux — the engine has no standalone/headless mode.
#
# Run scripts/install/local/install.sh FIRST: it installs the engine/helper binaries (dontspeak,
# ds-helper) and wires installed clients. This script then adds
# the GUI binary, its .desktop launcher, optional autostart, and the input-device permissions.
#
# Flags:  --autostart   also install ~/.config/autostart/dontspeak.desktop (launch at login)
#         --aec         install the PipeWire/PulseAudio echo-cancel config (full-duplex)
#         --no-udev     skip the /dev/uinput udev rule + input-group step (needs sudo)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
# XDG-aware like ICONS_DIR below, tarball-install.sh, and scripts/install/bundle/uninstall.sh — a
# hardcoded ~/.local/share here would strand the launcher where the uninstaller
# (which honors XDG_DATA_HOME) never looks.
APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
AUTOSTART=0; DO_AEC=0; DO_UDEV=1
for a in "$@"; do case "$a" in
  --autostart) AUTOSTART=1 ;; --aec) DO_AEC=1 ;; --no-udev) DO_UDEV=0 ;;
  *) echo "unknown flag: $a" >&2; exit 2 ;;
esac; done

log() { echo "==> $*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "MISSING: $1" >&2; exit 1; }; }

need cargo
need pkg-config

# ── 1. Build-time deps (GTK4 stack) ──────────────────────────────────────────
miss=""
for pc in gtk4 libadwaita-1 gtk4-layer-shell-0; do
  pkg-config --exists "$pc" || miss="$miss $pc"
done
if [ -n "$miss" ]; then
  echo "MISSING GTK dev libraries:$miss" >&2
  echo "  Debian/Ubuntu: sudo apt install libgtk-4-dev libadwaita-1-dev libgtk4-layer-shell-dev" >&2
  echo "  Fedora:        sudo dnf install gtk4-devel libadwaita-devel gtk4-layer-shell-devel" >&2
  exit 1
fi

# ── 2. Build + install the GUI host ──────────────────────────────────────────
log "building ds-gtk (release)"
( cd "$HERE/gtk" && cargo build --release )
# Stop a running host first — `install` over a live binary is unguarded (mirrors
# the macOS clean step and the tarball installer).
pkill -x ds-gtk 2>/dev/null || true
mkdir -p "$INSTALL_DIR"
install -m0755 "$HERE/gtk/target/release/ds-gtk" "$INSTALL_DIR/ds-gtk"
log "installed $INSTALL_DIR/ds-gtk"

# ── 3. Desktop launcher + icon (Exec → the installed binary so it works off-PATH;
#      the icon is the SAME file the tarball ships, so dev and release menu entries
#      match — dontspeak.desktop points at Icon=dontspeak) ────────────────────
mkdir -p "$APPS_DIR"
# Quote the Exec value: the desktop-entry spec word-splits it, so an INSTALL_DIR with a
# space would silently launch "/home/my" instead of the binary. Escape sed's replacement
# metacharacters (\, &, and the | delimiter) so an INSTALL_DIR containing any of them
# doesn't corrupt the generated Exec line.
ESC_INSTALL_DIR="$(printf '%s' "$INSTALL_DIR" | sed -e 's/[\\&|]/\\&/g')"
sed "s|^Exec=ds-gtk|Exec=\"$ESC_INSTALL_DIR/ds-gtk\"|" \
  "$HERE/dontspeak.desktop" > "$APPS_DIR/dontspeak.desktop"
ICONS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
mkdir -p "$ICONS_DIR"
install -m0644 "$HERE/../../assets/app-icon.svg" "$ICONS_DIR/dontspeak.svg"
log "installed $APPS_DIR/dontspeak.desktop (+ icon)"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" || true

if [ "$AUTOSTART" = 1 ]; then
  mkdir -p "$HOME/.config/autostart"
  cp "$APPS_DIR/dontspeak.desktop" "$HOME/.config/autostart/dontspeak.desktop"
  log "autostart enabled (~/.config/autostart/dontspeak.desktop)"
fi

# ── 4. Input-device permissions (Caps-Lock read + uinput injection) ──────────
if [ "$DO_UDEV" = 1 ]; then
  if [ ! -w /dev/uinput ] || ! id -nG | tr ' ' '\n' | grep -qx input; then
    log "installing udev rule + adding you to the 'input' group (needs sudo)"
    sudo install -m0644 "$HERE/udev-rule.txt" /etc/udev/rules.d/99-ds-input.rules
    sudo udevadm control --reload && sudo udevadm trigger || true
    sudo usermod -aG input "$USER" || true
    echo "   NOTE: log out/in (or run 'newgrp input') for the group change to take effect."
  else
    log "/dev/uinput already writable and you're in 'input' — skipping udev step"
  fi
fi

# ── 5. Full-duplex AEC config (optional) ─────────────────────────────────────
if [ "$DO_AEC" = 1 ]; then
  if pkg-config --exists libpipewire-0.3 && command -v pw-cli >/dev/null 2>&1; then
    mkdir -p "$HOME/.config/pipewire/pipewire.conf.d"
    cp "$HERE/aec/99-ds-aec.conf" "$HOME/.config/pipewire/pipewire.conf.d/"
    log "installed PipeWire echo-cancel drop-in — restart: systemctl --user restart pipewire pipewire-pulse"
  else
    log "PipeWire not detected; for PulseAudio see apps/linux/aec/ds-echo-cancel.pa"
  fi
  echo "   Then enable full-duplex via the MCP: set_config full_duplex=true"
fi

cat <<EOF

Done. The DontSpeak GUI host is installed.
  • Launch it from your app menu ("DontSpeak") or: $INSTALL_DIR/ds-gtk
    It hosts the engine in-process (tray + health panel + dictation overlay) and
    quits the engine when you quit the app — the desktop analogue of DontSpeak.app.
  • Caps-Lock dictation needs the input-device access above (real keyboard required —
    not available under WSL/containers). Models download on demand on first use.
EOF
