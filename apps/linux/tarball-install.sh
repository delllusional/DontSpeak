#!/usr/bin/env bash
# tarball-install.sh — the installer that ships INSIDE the Linux portable tarball as
# ./install.sh (package.sh copies this file in verbatim; single source, no embedded
# heredoc copy to drift). Copies binaries to ~/.local/bin, installs the launcher +
# icon, wires every detected client, and prints the one sudo step (/dev/uinput).
#
# Runs from the extracted tarball root — it resolves payload paths relative to itself,
# so it has no repo dependencies. Env: DONTSPEAK_INSTALL_DIR overrides ~/.local/bin.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
# Refuse an incomplete package before changing the machine. Every platform package carries
# its canonical uninstaller as payload; a missing one is a broken artifact, not optional.
[ -f "$HERE/uninstall.sh" ] || { echo "install: package is missing uninstall.sh" >&2; exit 1; }
# Stop a running host/helper first — `install` over a live binary is unguarded
# (mirrors the macOS clean step, which quits the app before replacing it).
pkill -x ds-gtk 2>/dev/null || true
pkill -f ds-helper 2>/dev/null || true
install -d "$BIN" "$APPS" "$ICONS"
install -m0755 "$HERE"/bin/* "$BIN/"
# Quote the Exec value: the desktop-entry spec word-splits it, so an install dir with a
# space would silently launch "/home/my" instead of the binary. Escape sed's replacement
# metacharacters (\, &, and the | delimiter) so an install dir containing any of them
# doesn't corrupt the generated Exec line.
ESC_BIN="$(printf '%s' "$BIN" | sed -e 's/[\\&|]/\\&/g')"
sed "s|^Exec=ds-gtk|Exec=\"$ESC_BIN/ds-gtk\"|" "$HERE/share/applications/dontspeak.desktop" > "$APPS/dontspeak.desktop"
install -m0644 "$HERE/share/icons/hicolor/scalable/apps/dontspeak.svg" "$ICONS/dontspeak.svg"
# Start-at-login by default (parity with macOS SMAppService and the Windows Run key) —
# dontspeak.desktop's own comment says the installer copies it into ~/.config/autostart/.
# Opt out with DONTSPEAK_NO_AUTOSTART=1 (same env var web/install.sh honors for its own,
# now-redundant copy of this step when it wraps this script).
if [ "${DONTSPEAK_NO_AUTOSTART:-0}" != "1" ]; then
  AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
  install -d "$AUTOSTART"
  cp "$APPS/dontspeak.desktop" "$AUTOSTART/dontspeak.desktop"
fi
# Standalone uninstaller onto PATH (parity with the other platform installers) — once
# the extracted tarball dir is deleted, this is the only removal path left.
install -m0755 "$HERE/uninstall.sh" "$BIN/dontspeak-uninstall"
"$BIN/dontspeak" wire --reconcile 2>/dev/null || echo "(wire skipped)"
echo
echo "Installed to $BIN. To grant /dev/uinput (synthetic keys), once:"
echo "  sudo install -m0644 '$HERE/udev/99-ds-input.rules' /etc/udev/rules.d/99-ds-input.rules"
echo "  sudo udevadm control --reload && sudo udevadm trigger && sudo usermod -aG input \"\$USER\"   # then re-login"
echo "Launch: $BIN/ds-gtk  (or the \"DontSpeak\" app menu entry)"
if [ "${DONTSPEAK_NO_AUTOSTART:-0}" != "1" ]; then
  echo "Start-at-login enabled (~/.config/autostart/dontspeak.desktop; DONTSPEAK_NO_AUTOSTART=1 to skip)"
fi
echo "Uninstall any time: $BIN/dontspeak-uninstall"
