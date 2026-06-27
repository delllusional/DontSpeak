#!/usr/bin/env bash
# bundle.sh — build the SwiftUI app, compile the Liquid Glass icon, and assemble a
# runnable, signed DontSpeak.app. The app HOSTS the engine in-process (no daemon).
#
# Steps:
#   0. install-daemon.sh — build+install+stable-sign the engine binaries + hooks with
#                 a BUILD_ID (name kept for compat; installs no daemon).
#   1. build.sh — Rust FFI staticlib (release-ffi) + `swift build` the app.
#   2. actool   — compile AppIcon.icon (Icon Composer) into Assets.car (macOS 26
#                 Liquid Glass) + a fallback AppIcon.icns. NOTE: there is no
#                 SVG→.icon CLI; AppIcon.icon is the authored source (edit it in
#                 Icon Composer, or by hand — icon.json + Assets/Foreground.svg).
#   3. assemble DontSpeak.app (executable + Bundle/Info.plist + the compiled icon).
#   4. codesign with a stable Apple identity if available (else ad-hoc).
#
# Output: $APP (default ~/Applications/DontSpeak.app). Override with DONTSPEAK_APP_DIR.
# The app is the login item + engine host, so installing it here is enough —
# launch it once and it registers itself at login and starts the engine.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="${DONTSPEAK_APP_DIR:-$HOME/Applications/DontSpeak.app}"
# Shared .app assembly (compile_icon / assemble_app / resolve_sign_identity), also
# used by dist-dmgs.sh so the bundle layout + signing live in one place.
source "$DIR/bundle-lib.sh"

# ==> 0. Build + install the engine BINARIES + hooks FIRST. install-daemon.sh
# computes the BUILD_ID (git), builds+installs+stable-signs the 5 bins (incl. the
# ds-helper that step 3c bundles), installs the hooks, and echoes the id as its
# LAST stdout line (all progress → stderr). We bake that SAME id into the app's
# Info.plist (step 3). There is no standalone daemon — the app hosts the engine.
echo "==> 0. build + install engine binaries + hooks"
BUILD_ID="$("$DIR/../../scripts/install-daemon.sh" | tail -1)"
[ -n "$BUILD_ID" ] || { echo "install-daemon.sh produced no BUILD_ID" >&2; exit 1; }
echo "   binaries installed; BUILD_ID=$BUILD_ID"

echo "==> 1. build (Rust staticlib + swift build)"
"$DIR/build.sh" >/dev/null
EXE="$DIR/.build/release/DontSpeak"
[ -x "$EXE" ] || { echo "build did not produce $EXE" >&2; exit 1; }

echo "==> 2. compile AppIcon.icon (actool → Assets.car + AppIcon.icns)"
ICONOUT="$(mktemp -d)"; trap 'rm -rf "$ICONOUT"' EXIT
compile_icon "$ICONOUT"

echo "==> 3. assemble + sign $APP"
# The host-arch helper installed by install-daemon.sh (step 0) goes into the bundle.
SIGN="$(resolve_sign_identity)"
# Build the apple-native Kokoro shim for the host arch (Apple-Silicon only); bundled by
# assemble_app and pointed at via SMKOKORO_DYLIB_PATH at app launch.
DONTSPEAK_SMKOKORO_DYLIB="$(build_smkokoro_dylib "$(uname -m)")"; export DONTSPEAK_SMKOKORO_DYLIB
# menubar-icon.svg lives at the REPO ROOT under assets/ (../../assets from apps/macos).
# The apps/ reorg (8d326a5) moved macos/ → apps/macos/ but left this as $DIR/.. (=apps/),
# so the file silently went missing and the menu bar fell back to the waveform SF Symbol.
assemble_app "$APP" "$EXE" "$HOME/.local/bin/ds-helper" \
  "$ICONOUT/Assets.car" "$ICONOUT/AppIcon.icns" "$DIR/Bundle/Info.plist" \
  "$(cd "$DIR/../.." && pwd)/assets/menubar-icon.svg" "$BUILD_ID" "$SIGN"
echo "   signed app ($([ "$SIGN" = "-" ] && echo ad-hoc || echo "${SIGN%% (*}…"))"

# ==> 5. Cutover: the app hosts the engine in-process, so the standalone launchd
# daemon must NOT run (it would fight for the same RPC socket). Remove it.
echo "==> 5. remove standalone launchd daemon (app hosts the engine now)"
PLISTDST="$HOME/Library/LaunchAgents/org.dontspeak.daemon.plist"
launchctl unload "$PLISTDST" 2>/dev/null || true
rm -f "$PLISTDST"
pkill -f '/dontspeakd' 2>/dev/null || true
echo "   launchd daemon removed — DontSpeak.app runs caps + RPC + TTS in-process"

echo
echo "Done → $APP"
echo "Launch it: open \"$APP\"  (registers itself as the login item + starts the engine)"
