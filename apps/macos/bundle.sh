#!/usr/bin/env bash
# bundle.sh -- build+sign DontSpeak.app (engine in-process). Output: $APP
# (default ~/Applications; override DONTSPEAK_APP_DIR).
# 0 install-engine  1 build.sh  2 actool icon  3 assemble  4 codesign
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="${DONTSPEAK_APP_DIR:-$HOME/Applications/DontSpeak.app}"
source "$DIR/bundle-lib.sh"
trap ds_lock_release EXIT
trap 'ds_lock_release; exit 130' INT TERM HUP

# Full Xcode required (actool not in CLT). Fail before slow Rust/swift.
require_xcode() {
  xcrun -f actool >/dev/null 2>&1 && return 0
  local hint="/Applications/Xcode.app/Contents/Developer"
  echo "ERROR: 'actool' not found -- the DontSpeak.app build REQUIRES the full Xcode," >&2
  echo "       not just the Command Line Tools (active dir: $(xcode-select -p 2>/dev/null))." >&2
  if [ -x "$hint/usr/bin/actool" ]; then
    echo "       Xcode IS installed but not selected. Fix it once with:" >&2
    echo "         sudo xcode-select -s $hint" >&2
  else
    echo "       Install Xcode from the App Store, then select it once with:" >&2
    echo "         sudo xcode-select -s /Applications/Xcode.app/Contents/Developer" >&2
  fi
  exit 1
}
require_xcode

echo "==> 0. build + install engine binaries + hooks"
BUILD_ID="$("$DIR/../../scripts/install/local/install-engine.sh" | tail -1)"
echo "   binaries installed; BUILD_ID=$BUILD_ID"

echo "==> 0b. wire configured client integrations"
_bin_dir="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
ds_lock_acquire "$APP"
"$_bin_dir/dontspeak" wire --reconcile \
  || echo "   !! wire --reconcile failed; run '$_bin_dir/dontspeak wire --reconcile' manually" >&2
ds_lock_release

echo "==> 1. build (Rust staticlib + Swift app)"
"$DIR/build.sh" >/dev/null
EXE="$DIR/.build/release/DontSpeak"
[ -x "$EXE" ] || { echo "build did not produce $EXE" >&2; exit 1; }

echo "==> 2. compile AppIcon.icon (actool -> Assets.car + AppIcon.icns)"
ICONOUT="$(mktemp -d)"
cleanup() { ds_lock_release; rm -rf "$ICONOUT"; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM HUP
compile_icon "$ICONOUT"

echo "==> 3. assemble + sign $APP"
SIGN="$(resolve_sign_identity)"
# Shim arch from built app binary (not uname -- Rosetta).
DONTSPEAK_SHIM_DYLIBS="$(build_shims "$(lipo -archs "$EXE" | awk '{print $1}')")"
export DONTSPEAK_SHIM_DYLIBS
# Helper from install-engine dir; menubar svg at repo assets/.
ds_lock_acquire "$APP"
assemble_app "$APP" "$EXE" "$_bin_dir/ds-helper" \
  "$ICONOUT/Assets.car" "$ICONOUT/AppIcon.icns" "$DIR/Bundle/Info.plist" \
  "$(cd "$DIR/../.." && pwd)/assets/menubar-icon.svg" "$SIGN"
require_engine_symbol "$APP/Contents/MacOS/DontSpeak" || {
  echo "FATAL: $APP/Contents/MacOS/DontSpeak has no ds_engine_start after assembly" >&2
  exit 1
}
ds_lock_release
echo "   signed app ($(sign_label "$SIGN"))"

echo
echo "Done -> $APP"
echo "Launch it: open \"$APP\"  (registers itself as the login item + starts the engine)"
