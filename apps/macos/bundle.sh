#!/usr/bin/env bash
# bundle.sh — build the SwiftUI app, compile the Liquid Glass icon, and assemble a
# runnable, signed DontSpeak.app. The app HOSTS the engine in-process (no daemon).
#
# Steps:
#   0. install-engine.sh — build+install+stable-sign the engine binaries + hooks with
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
# used by dist-apps.sh so the bundle layout + signing live in one place.
source "$DIR/bundle-lib.sh"

# ==> Preflight: REQUIRE the full Xcode toolchain BEFORE the (slow) Rust + swift
# build, so a fresh machine fails fast with an actionable message instead of dying
# at step 2. `actool` (the icon compiler) ships ONLY with Xcode.app — NOT the
# Command Line Tools — so a box where `xcode-select -p` still points at
# /Library/Developer/CommandLineTools can't compile AppIcon.icon. We hard-require
# it here (no silent fallback); the fix is a one-time `xcode-select -s`.
require_xcode() {
  xcrun -f actool >/dev/null 2>&1 && return 0
  local hint="/Applications/Xcode.app/Contents/Developer"
  echo "ERROR: 'actool' not found — the DontSpeak.app build REQUIRES the full Xcode," >&2
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

# Release (scripts/install/web/install.sh) and dev (this script) installs share the ONE per-user
# layout at $APP — there is no other location to clean up; assemble_app replaces
# $APP itself.

# ==> 0. Build + install the engine BINARIES + hooks FIRST. install-engine.sh
# computes the BUILD_ID (git), builds+installs+stable-signs the CLI bins (incl. the
# ds-helper that step 3 bundles), installs the hooks, and echoes the id as its
# LAST stdout line (all progress → stderr). We bake that SAME id into the app's
# Info.plist (step 3). There is no standalone daemon — the app hosts the engine.
echo "==> 0. build + install engine binaries + hooks"
BUILD_ID="$("$DIR/../../scripts/install/local/install-engine.sh" | tail -1)"
[ -n "$BUILD_ID" ] || { echo "install-engine.sh produced no BUILD_ID" >&2; exit 1; }
echo "   binaries installed; BUILD_ID=$BUILD_ID"

# install-engine.sh installs binaries only; reconcile applies the configured client
# integrations during app deployment too (mirrors scripts/install/local/install.sh).
echo "==> 0b. wire configured client integrations"
_bin_dir="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
# `wire --reconcile` converges each client to config.toml's `exclude_clients` (absent ⇒ all) —
# identical to `--all` on a fresh machine, but a reinstall over a user-narrowed set won't thrash.
"$_bin_dir/dontspeak" wire --reconcile \
  || echo "   !! wire --reconcile failed; run '$_bin_dir/dontspeak wire --reconcile' manually" >&2

echo "==> 1. build (Rust staticlib + swift build)"
"$DIR/build.sh" >/dev/null
EXE="$DIR/.build/release/DontSpeak"
[ -x "$EXE" ] || { echo "build did not produce $EXE" >&2; exit 1; }

echo "==> 2. compile AppIcon.icon (actool → Assets.car + AppIcon.icns)"
ICONOUT="$(mktemp -d)"; trap 'rm -rf "$ICONOUT"' EXIT
compile_icon "$ICONOUT"

echo "==> 3. assemble + sign $APP"
# The host-arch helper installed by install-engine.sh (step 0) goes into the bundle.
SIGN="$(resolve_sign_identity)"
# Build the apple-native Kokoro shim (Apple-Silicon only); bundled by assemble_app and
# pointed at via SMKOKORO_DYLIB_PATH at app launch. Keyed off the BUILT APP's arch, not
# `uname -m`: a Rosetta shell reports x86_64 while ALSO producing an x86_64 app, so the
# app binary is the one truth the shim must match (dist-apps.sh gates on SWARCH the same way).
DONTSPEAK_SMKOKORO_DYLIB="$(build_smkokoro_dylib "$(lipo -archs "$EXE" | awk '{print $1}')")"
export DONTSPEAK_SMKOKORO_DYLIB
# menubar-icon.svg lives at the REPO ROOT under assets/ (../../assets from apps/macos).
# The apps/ reorg (8d326a5) moved macos/ → apps/macos/ but left this as $DIR/.. (=apps/),
# so the file silently went missing and the menu bar fell back to the waveform SF Symbol.
# The helper comes from the SAME dir install-engine.sh (step 0) installed into — a
# custom DONTSPEAK_INSTALL_DIR must not fall back to a stale ~/.local/bin copy (that
# drifts the app/helper BUILD_ID pair).
assemble_app "$APP" "$EXE" "$_bin_dir/ds-helper" \
  "$ICONOUT/Assets.car" "$ICONOUT/AppIcon.icns" "$DIR/Bundle/Info.plist" \
  "$(cd "$DIR/../.." && pwd)/assets/menubar-icon.svg" "$BUILD_ID" "$SIGN"
# Post-assemble backstop (mirror of dist-apps.sh): the INSTALLED executable must still be
# the engine-linked SwiftUI app after every cp/sign assemble_app performed.
require_engine_symbol "$APP/Contents/MacOS/DontSpeak" || {
  echo "FATAL: $APP/Contents/MacOS/DontSpeak has no ds_engine_start after assembly" >&2
  exit 1
}
echo "   signed app ($(sign_label "$SIGN"))"

echo
echo "Done → $APP"
echo "Launch it: open \"$APP\"  (registers itself as the login item + starts the engine)"
