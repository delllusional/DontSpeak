#!/bin/bash
# install.sh — full CLI install (macOS-first). Delegates bins to install-engine.sh;
# adds wire --reconcile + next-steps. Engine is in-process in the host app (no daemon).
# SAFETY: idempotent; wire merges are additive, backed-up, malformed-safe.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
. "$REPO/scripts/install/lib/common.sh"
H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
UNAME="$(uname -s)"

need() { command -v "$1" >/dev/null 2>&1 || { echo "MISSING: $1 — install it first (see README Prerequisites)"; exit 1; }; }

need cargo

# 1-4. Engine bins + hooks (shared with bundle.sh). Last line = BUILD_ID.
BUILD_ID="$(DONTSPEAK_INSTALL_DIR="$INSTALL_DIR" bash "$REPO/scripts/install/local/install-engine.sh")"
echo "==> binaries + hooks installed (BUILD_ID=$BUILD_ID)"

# 5. wire --reconcile (exclude_clients; skip missing clients)
echo
echo "==> 5. reconcile all detected client integrations"
"$INSTALL_DIR/dontspeak" wire --reconcile \
  || echo "   !! wire --reconcile failed; run '$INSTALL_DIR/dontspeak wire --reconcile' manually" >&2

if [ "$UNAME" = "Darwin" ]; then LOG_HINT="~/Library/Logs/DontSpeak/dontspeak.log"
else LOG_HINT="\${XDG_STATE_HOME:-~/.local/state}/dontspeak/logs/dontspeak.log"; fi

cat <<EOF

Done. Installed:
  • $INSTALL_DIR/{dontspeak,ds-helper}
  • detected client hooks + MCP entries reconciled from the shared registry
    (inspect with 'dontspeak wire --list'; preview with 'dontspeak wire --all --print-only')
  • logs: $LOG_HINT (in-process rotation, no sudo)

Next steps:
EOF

if [ "$UNAME" = "Darwin" ]; then
  cat <<EOF
  • Build + launch the app for the warm engine + Caps-Lock push-to-talk:
        ./apps/macos/bundle.sh && open ~/Applications/DontSpeak.app
    The app HOSTS the engine in-process and registers itself as the login item.
    On first launch grant it Accessibility + Microphone (System Settings >
    Privacy & Security) — ONE grant set, all on DontSpeak.app. (Accessibility
    subsumes Input Monitoring, so there is no separate grant for the Caps read.)
    The hooks already work without it (cold one-shot synth); the app adds the
    warm low-latency engine and Caps-Lock recording.
EOF
else
  cat <<EOF
  • Build + install the GTK GUI host — tray, health panel, dictation overlay; it
    hosts the engine in-process like DontSpeak.app:
        ./apps/linux/install-gui.sh            (add --autostart, --aec as desired)
    Then launch "DontSpeak" from your app menu. Grant input-device access per
    apps/linux/udev-rule.txt if recording does not start.
EOF
fi

cat <<EOF

Hot-reload: the engine reloads config WITHOUT a restart — a config.toml write
auto-applies via its mtime-watch, and the host app can nudge an instant reload
(engine_reload). No relaunch needed after a voice/engine change.
EOF
