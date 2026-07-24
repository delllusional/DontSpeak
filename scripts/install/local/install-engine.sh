#!/usr/bin/env bash
# install-engine.sh -- build+install CLI bins (dontspeak, ds-helper) with BUILD_ID.
# No standalone daemon (app hosts engine). Called by install.sh + bundle.sh.
#
# Installs to ~/.local/bin only -- app uses bundled ds-helper; helper/engine changes
# need full bundle.sh. Hooks/MCP from this script go live via ~/.local/bin/dontspeak.
#
# Env: DONTSPEAK_INSTALL_DIR, DONTSPEAK_BUILD_ID, DONTSPEAK_CODESIGN_ID.
# Last stdout line = BUILD_ID.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
. "$REPO/scripts/install/lib/common.sh"
RUST_DIR="$REPO/rust"
H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
UNAME="$(uname -s)"

# 1. BUILD_ID (debug log only, not status wire)
DONTSPEAK_BUILD_ID="$(compute_build_id "$REPO")"; export DONTSPEAK_BUILD_ID
echo "==> engine BUILD_ID = $DONTSPEAK_BUILD_ID" >&2

mkdir -p "$INSTALL_DIR"

# 2. CLI bins (BUILD_ID via dontspeakd build.rs)
echo "==> build hooks/mcp + ds-helper (release)" >&2
( cd "$RUST_DIR" && cargo build --release \
    -p dontspeak -p ds-helper \
    --bin dontspeak --bin ds-helper ) >&2

echo "==> install binaries -> $INSTALL_DIR" >&2
REL="$RUST_DIR/target/release"
trap ds_lock_release EXIT
trap 'ds_lock_release; exit 130' INT TERM HUP
ds_lock_acquire "$(local_install_lock_destination "$INSTALL_DIR")"
for b in dontspeak ds-helper; do
  install -m 0755 "$REL/$b" "$INSTALL_DIR/$b"
done

# 3. Stable sign with app.dontspeak.org (shared TCC); ad-hoc fallback
if [ "$UNAME" = "Darwin" ]; then
  ensure_local_sign_identity
  STABLE_ID="$(find_codesign_id)"
  sign_stable() {
    codesign --force --identifier "app.dontspeak.org" --sign "$STABLE_ID" "$INSTALL_DIR/$1" 2>/dev/null \
      && echo "   signed $1 (stable: ${STABLE_ID%% (*}..., app.dontspeak.org)" >&2 \
      || { echo "   !! stable-sign $1 failed; ad-hoc fallback" >&2; codesign --force --sign - "$INSTALL_DIR/$1" 2>/dev/null; }
  }
  for b in dontspeak ds-helper; do
    if [ -n "$STABLE_ID" ]; then sign_stable "$b"
    else codesign --force --sign - "$INSTALL_DIR/$b" 2>/dev/null \
           && echo "   ad-hoc signed $b (no stable identity -- grant RE-PROMPTS on rebuild; set DONTSPEAK_CODESIGN_ID)" >&2; fi
  done
fi

# 4. Standalone uninstaller (parity with release installer)
install -m0755 "$REPO/scripts/install/bundle/uninstall.sh" "$INSTALL_DIR/dontspeak-uninstall"
echo "==> placed $INSTALL_DIR/dontspeak-uninstall (full removal any time)" >&2

ds_lock_release
echo "$DONTSPEAK_BUILD_ID"
