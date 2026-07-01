#!/bin/bash
# install.sh — first-time / full CLI install of the DontSpeak RUST stack (macOS-first).
#
# The binary build+install is delegated to scripts/install-daemon.sh (the SINGLE
# source of truth, ALSO called by apps/macos/bundle.sh). It builds + installs the CLI
# binaries (dontspeak MCP/hooks + ds-helper) into $INSTALL_DIR (default ~/.local/bin),
# stable-signs them (so the TCC grants survive rebuilds), and installs the thin hook
# wrappers. Logging is ~/Library/Logs/dontspeak.log with in-process rotation (no conf needed).
#
# This wrapper adds the things specific to a fresh CLI install: wiring each client's whole
# integration via `dontspeak wire <client>` (Claude Code = hooks + MCP, Desktop = MCP, Codex =
# hooks; additive, backed-up; preview with --print-only, undo with --remove) and the next-steps notes.
#
# ENGINE HOST: the engine runs IN-PROCESS inside the platform's resident host app on EVERY
# platform — macOS DontSpeak.app (apps/macos/bundle.sh), Linux the GTK host ds-gtk
# (apps/linux/install-gui.sh), Windows the WinUI app. The host owns the RPC socket, hosts
# TTS/STT, and catches Caps-Lock, so a single TCC/permission grant lands on the app and there
# is NO standalone daemon. The hooks work without the app up (warm socket if it is, else a
# cold one-shot synth).
#
# SAFETY: idempotent. Touches ~/.claude/hooks (backed up), $INSTALL_DIR, and wires each client's
# integration via `dontspeak wire <client>` — additive, backed-up, malformed-safe merges that
# never clobber your other keys.
set -euo pipefail

# This installer lives in scripts/; the repo root is one level up. The shared helpers
# (PATH setup, etc.) live in scripts/lib/common.sh.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
. "$REPO/scripts/lib/common.sh"
H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
UNAME="$(uname -s)"

need() { command -v "$1" >/dev/null 2>&1 || { echo "MISSING: $1 — install it first (see README Prerequisites)"; exit 1; }; }

need cargo

# ==> 1-4. Build + install the engine binaries + hooks (shared with apps/macos/bundle.sh).
# Echoes the BUILD_ID it baked in as its last line.
BUILD_ID="$(DONTSPEAK_INSTALL_DIR="$INSTALL_DIR" bash "$REPO/scripts/install-daemon.sh")"
echo "==> binaries + hooks installed (BUILD_ID=$BUILD_ID)"

# ==> 5. Wire the Claude Code voice hooks into ~/.claude/settings.json. The
# The dontspeak binary owns the ONE cross-platform per-client integration definition + a SAFE
# merge (additive, idempotent, timestamped backup first, malformed file left untouched), so every
# platform installs the identical set. `wire <client>` does that client's WHOLE integration in one
# step — Claude Code = voice hooks + MCP server; Desktop = MCP server; Codex = narration hooks.
# Preview with --print-only; undo with --remove; a client that isn't installed is a clean skip.
echo
echo "==> 5. wire Claude Code (voice hooks → ~/.claude/settings.json + MCP server → ~/.claude.json)"
"$INSTALL_DIR/dontspeak" wire claude_code

echo
echo "==> 6. wire Claude Desktop MCP server (only if Desktop is installed)"
"$INSTALL_DIR/dontspeak" wire claude_desktop || true

echo
echo "==> 7. wire OpenAI Codex narration hooks (only if ~/.codex exists)"
"$INSTALL_DIR/dontspeak" wire codex || true

cat <<EOF

Done. Installed:
  • $INSTALL_DIR/{dontspeak,ds-helper}
  • Claude Code: ~/.claude/settings.json voice hooks + ~/.claude.json MCP server (wired via
    'dontspeak wire claude_code' — start a NEW Claude Code session to load the MCP server;
    undo any time with 'dontspeak wire claude_code --remove')
  • Claude Desktop MCP server (only if Desktop is installed; 'dontspeak wire claude_desktop'
    → claude_desktop_config.json — restart Desktop to load it; undo with '… --remove')
  • ~/.codex/config.toml narration hooks (only if ~/.codex exists; 'dontspeak wire codex';
    undo with 'dontspeak wire codex --remove')
  • logs: ~/Library/Logs/dontspeak.log (in-process rotation, no sudo)

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

Hot-reload: the engine reloads config WITHOUT a restart — a settings.json write
auto-applies via its mtime-watch, and the host app can nudge an instant reload
(engine_reload). No relaunch needed after a voice/engine change.
EOF
