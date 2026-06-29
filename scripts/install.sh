#!/bin/bash
# install.sh — first-time / full CLI install of the DontSpeak RUST stack (macOS-first).
#
# The binary build+install is delegated to scripts/install-daemon.sh (the SINGLE
# source of truth, ALSO called by apps/macos/bundle.sh). It builds + installs the engine
# + helper binaries (dontspeakd, dontspeak, ds-helper) into $INSTALL_DIR
# (default ~/.local/bin), stable-signs dontspeakd (so
# the TCC grants survive rebuilds), and installs the thin hook wrappers.
# Logging is ~/Library/Logs/dontspeak.log with in-process rotation (no conf needed).
#
# This wrapper adds the things specific to a fresh CLI install: merging the voice
# hooks into ~/.claude/settings.json via `dontspeak wire-hooks` (additive, backed-up;
# preview with --print-only, undo with --remove) and the next-steps notes.
#
# ENGINE HOST: on macOS the engine runs IN-PROCESS inside DontSpeak.app (built via
# apps/macos/bundle.sh) — it owns the RPC socket, hosts TTS/STT, and catches Caps-Lock,
# so there is NO standalone daemon and a single TCC grant lands on the app. On Linux
# the headless dontspeakd is the engine host (systemd: apps/linux/enable-daemon.sh). The
# hooks work without either (warm socket if up, else a cold one-shot synth).
#
# SAFETY: idempotent. Touches ~/.claude/hooks (backed up), $INSTALL_DIR, and merges
# the voice hooks into ~/.claude/settings.json via `dontspeak wire-hooks` — an
# additive, backed-up, malformed-safe merge that never clobbers your other keys.
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
# dontspeak binary owns the ONE cross-platform hook definition + a SAFE merge
# (additive, idempotent, timestamped backup first, malformed file left untouched), so
# every platform installs the identical set. Preview without writing:
#   $INSTALL_DIR/dontspeak wire-hooks --print-only      (undo: wire-hooks --remove)
echo
echo "==> 5. wire Claude Code voice hooks → ~/.claude/settings.json"
echo "       (also wires OpenAI Codex's narration hooks → ~/.codex/config.toml if ~/.codex exists)"
"$INSTALL_DIR/dontspeak" wire-hooks

# ==> 6. Register the MCP server with Claude DESKTOP, if it's installed. Desktop has no
# hook system, so this is registration ONLY — it adds mcpServers.dontspeak to
# ~/Library/Application Support/Claude/claude_desktop_config.json (additive, backed-up,
# re-points on reinstall). wire-desktop self-detects and SKIPS quietly if Desktop is
# absent, so this is safe to always run. Preview: wire-desktop --print-only; undo: --remove.
echo
echo "==> 6. register MCP server with Claude Desktop (only if installed)"
"$INSTALL_DIR/dontspeak" wire-desktop || true

cat <<EOF

Done. Installed:
  • $INSTALL_DIR/{dontspeakd,dontspeak,ds-helper}
  • ~/.claude/settings.json voice hooks (merged via 'dontspeak wire-hooks';
    undo any time with: $INSTALL_DIR/dontspeak wire-hooks --remove)
  • ~/.codex/config.toml narration hooks (only if ~/.codex exists; skip with
    'wire-hooks --no-codex', or wire later with 'wire-hooks --codex-only')
  • Claude Desktop MCP server (only if Desktop is installed; registered via
    'dontspeak wire-desktop' → claude_desktop_config.json — restart Desktop to
    load it; undo with 'dontspeak wire-desktop --remove')
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
  • DESKTOP (recommended): build + install the GTK GUI host — tray, health panel,
    dictation overlay; hosts the engine in-process like DontSpeak.app:
        ./apps/linux/install-gui.sh            (add --autostart, --aec as desired)
    Then launch "DontSpeak" from your app menu.
  • HEADLESS (server): run the engine as a systemd user service instead:
        ./apps/linux/enable-daemon.sh
    Pick ONE host (the engine pidfile is single-speaker). Either way, grant
    input-device access per apps/linux/udev-rule.txt if recording does not start.
EOF
fi

cat <<EOF

Hot-reload: the engine reloads config WITHOUT a restart — a settings.json write
auto-applies via its mtime-watch, and the GUI also sends SIGHUP for an instant
nudge. No relaunch needed after a voice/engine change.
EOF
