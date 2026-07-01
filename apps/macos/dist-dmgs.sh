#!/usr/bin/env bash
# dist-dmgs.sh — build the DontSpeak.app bundle + DMG for distribution.
#
# Builds the arch slices named in $DONTSPEAK_ARCHES (default "arm64"; "x86_64" for the
# Intel slice, or "arm64 x86_64" for both). The apple-native Kokoro/Parakeet/diarization
# stack (FluidAudio Core ML / ANE) is Apple-Silicon ONLY, so an x86_64 slice ships WITHOUT
# libsmkokoro and falls back to the portable ONNX TTS/STT path — it still runs on Intel.
#
# UNLIKE bundle.sh this has NO machine-install side effects: it does not install the
# engine bins to ~/.local/bin, register a login item, touch the running app, or remove
# launchd agents. It builds the Rust FFI staticlib + the bundled ds-helper per
# arch, links the Swift app, and reuses the shared assemble_app
# (bundle-lib.sh) to produce a signed .app, then a styled DMG.
#
# The DMG ships only what the app itself needs: the app binary (which hosts the engine
# in-process via the linked staticlib) + ds-helper (the Kokoro synth child) + the
# icon. The CLI/MCP bins (dontspeak, ds-helper) are installed separately by the
# CLI installer, not shipped here.
#
# Output: $OUTDIR/DontSpeak-<arch>.dmg   (default OUTDIR=~/Desktop)
# Requires: a cargo that can target each arch — a NATIVE slice (host == target, e.g.
#           x86_64 on an Intel Mac) builds with any cargo on PATH (Homebrew is fine);
#           a CROSS slice needs rustup with that target installed. Override the binary
#           with $DONTSPEAK_CARGO. Also: ImageMagick (make-dmg.sh); rsvg-convert is
#           optional (nicer DMG background + glyph PNG).
set -euo pipefail

# Pin the macOS deployment target for the Rust objects too (matches Package.swift's
# .v14) so the whole binary — not just the Swift slice — is built for macOS 14, and the
# linker stops warning that cargo's objects were built for a newer SDK.
export MACOSX_DEPLOYMENT_TARGET=14.0

# This is the DISTRIBUTION build by default: hardened runtime + secure timestamp +
# entitlements + bundled onnxruntime dylib + signed DMG (via bundle-lib.sh / make-dmg.sh
# dist branches), requiring a Developer ID Application identity, then notarized + stapled
# below. Override with DONTSPEAK_DIST=0 for the old ad-hoc, no-notarization build.
export DONTSPEAK_DIST="${DONTSPEAK_DIST:-1}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # apps/macos/
# Repo root is TWO levels up (apps/macos/ → repo root) since the apps/ reorg (8d326a5);
# was $HERE/.. (=apps/), which broke RUST, MENUBAR_SVG, and HOST_SLIB below.
REPO="$(cd "$HERE/../.." && pwd)"
RUST="$REPO/rust"
# Cargo binary. Prefer an explicit override, then rustup's multi-target cargo (needed for
# CROSS-arch slices), then whatever cargo is on PATH (Homebrew etc.) — enough for a NATIVE
# slice where the requested triple is the host triple (e.g. x86_64 on an Intel Mac).
CARGO="${DONTSPEAK_CARGO:-}"
if [ -z "$CARGO" ]; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then CARGO="$HOME/.cargo/bin/cargo"
  else CARGO="$(command -v cargo || true)"; fi
fi
OUTDIR="${OUTDIR:-$HOME/Desktop}"
MENUBAR_SVG="$REPO/assets/menubar-icon.svg"
HOST_SLIB="$RUST/target/release-ffi/libds_core.a"

source "$HERE/bundle-lib.sh"   # compile_icon / assemble_app / resolve_sign_identity

[ -n "$CARGO" ] && [ -x "$CARGO" ] || { echo "ERROR: no usable cargo (set DONTSPEAK_CARGO, or install rustup / Homebrew rust)" >&2; exit 1; }

# Build per-arch staging + the icon live under ONE temp root, and we back up the host
# staticlib that step [3] overwrites — both restored on exit (success OR failure), so a
# later plain `swift build`/build.sh isn't left linking a cross-compiled lib.
WORKROOT="$(mktemp -d)"
SLIB_BAK=""; [ -f "$HOST_SLIB" ] && { SLIB_BAK="$(mktemp)"; cp "$HOST_SLIB" "$SLIB_BAK"; }
cleanup() {
  rm -rf "$WORKROOT"
  [ -n "$SLIB_BAK" ] && cp "$SLIB_BAK" "$HOST_SLIB" && rm -f "$SLIB_BAK"   # restore host lib
  true
}
trap cleanup EXIT

BUILD_ID="$(compute_build_id "$REPO")"   # shared with bundle.sh; honors $DONTSPEAK_BUILD_ID

SIGN="$(resolve_sign_identity)"   # NOTE: Apple Development / ad-hoc trip Gatekeeper on
                                  # OTHER Macs; clean distribution needs Developer ID + notarization.

echo "==> actool icon (shared by both arches)"
ICONOUT="$WORKROOT/icon"; mkdir -p "$ICONOUT"
compile_icon "$ICONOUT"

build_arch() {   # $1 display arch, $2 rust triple, $3 swift arch
  local ARCH="$1" TRIPLE="$2" SWARCH="$3"
  echo; echo "################## $ARCH ($TRIPLE) ##################"

  echo "==> [1/6] cargo staticlib ($TRIPLE)"
  ( cd "$RUST" && "$CARGO" build --profile release-ffi --target "$TRIPLE" -p ds-core )
  local SLIB="$RUST/target/$TRIPLE/release-ffi/libds_core.a"
  [ -f "$SLIB" ] || { echo "no staticlib $SLIB" >&2; exit 1; }

  echo "==> [2/6] cargo ds-helper ($TRIPLE)"
  ( cd "$RUST" && "$CARGO" build --release --target "$TRIPLE" -p ds-tts --bin ds-helper )
  local HELPER="$RUST/target/$TRIPLE/release/ds-helper"
  [ -f "$HELPER" ] || { echo "no helper $HELPER" >&2; exit 1; }

  # Package.swift force_loads ../../rust/target/release-ffi/libds_core.a — stage the
  # arch-specific lib there so `swift build --arch` links the matching slice. (Restored
  # to the original host lib on exit by the cleanup trap.)
  echo "==> [3/6] stage staticlib for the linker"
  mkdir -p "$(dirname "$HOST_SLIB")"
  cp "$SLIB" "$HOST_SLIB"

  echo "==> [4/6] swift build --arch $SWARCH"
  local BIN; BIN="$(cd "$HERE" && swift build -c release --arch "$SWARCH" --show-bin-path)"
  rm -f "$BIN/DontSpeak"   # force relink against the just-staged staticlib
  ( cd "$HERE" && swift build -c release --arch "$SWARCH" )
  local EXE="$BIN/DontSpeak"
  [ -x "$EXE" ] || { echo "no app binary $EXE" >&2; exit 1; }
  echo "    app:    $(file "$EXE" | sed 's/.*: //')"
  echo "    helper: $(file "$HELPER" | sed 's/.*: //')"

  # Build the apple-native Kokoro shim (Apple-Silicon only); assemble_app bundles it.
  local DONTSPEAK_SMKOKORO_DYLIB; DONTSPEAK_SMKOKORO_DYLIB="$(build_smkokoro_dylib "$SWARCH")"
  export DONTSPEAK_SMKOKORO_DYLIB

  # Self-contained: bundle the MATCHING-arch onnxruntime dylib (sign_app_dist reads
  # $DONTSPEAK_ORT_DYLIB). A combined "arm64 x86_64" run can't share one global dylib —
  # it would stuff an arm64 lib into the x86_64 app. So resolve per-arch here:
  #   • DONTSPEAK_ORT_DYLIB_<arch> (e.g. DONTSPEAK_ORT_DYLIB_arm64) is the explicit override;
  #   • else fall back to a global DONTSPEAK_ORT_DYLIB.
  # Either way the lib is REJECTED unless `lipo` confirms it carries this slice's arch, so a
  # wrong-arch dylib is skipped (→ sign_app_dist warns / searches Application Support) instead
  # of silently mis-bundled. NOTE: x86_64 macOS has NO pinned onnxruntime dist (built-in ONNX
  # is unusable there; the app uses say/claude_code), so its slice normally ships without one.
  local ORT_VAR="DONTSPEAK_ORT_DYLIB_${ARCH}"
  local ORT="${!ORT_VAR:-${DONTSPEAK_ORT_DYLIB:-}}"
  if [ -n "$ORT" ] && [ -f "$ORT" ] && ! lipo -archs "$ORT" 2>/dev/null | tr ' ' '\n' | grep -qx "$SWARCH"; then
    echo "   skip onnxruntime bundle: $ORT is not a $SWARCH slice" >&2
    ORT=""
  fi
  export DONTSPEAK_ORT_DYLIB="$ORT"

  echo "==> [5/6] assemble + sign DontSpeak.app"
  local APP="$WORKROOT/$ARCH/DontSpeak.app"; mkdir -p "$WORKROOT/$ARCH"
  assemble_app "$APP" "$EXE" "$HELPER" "$ICONOUT/Assets.car" "$ICONOUT/AppIcon.icns" \
    "$HERE/Bundle/Info.plist" "$MENUBAR_SVG" "$BUILD_ID" "$SIGN"

  echo "==> [6/6] DMG → $OUTDIR/DontSpeak-$ARCH.dmg"
  DONTSPEAK_APP_DIR="$APP" "$HERE/make-dmg.sh" "$OUTDIR/DontSpeak-$ARCH.dmg"
}

# Which arch slices to build. Default "arm64" (the historically shipped slice). Set
# DONTSPEAK_ARCHES to a space-separated list to add/replace — "x86_64" for the Intel slice
# (builds natively on an Intel Mac), or "arm64 x86_64" for both (the cross slice needs
# rustup with that target). Recognized: arm64, x86_64.
ARCHES="${DONTSPEAK_ARCHES:-arm64}"
for A in $ARCHES; do
  case "$A" in
    arm64)  build_arch arm64  aarch64-apple-darwin arm64  ;;
    x86_64) build_arch x86_64 x86_64-apple-darwin  x86_64 ;;
    *) echo "ERROR: unknown arch '$A' (want arm64 and/or x86_64)" >&2; exit 1 ;;
  esac
done

# Notarize + staple each DMG when notary credentials are configured (a stored notarytool
# keychain profile via DONTSPEAK_NOTARY_PROFILE, or Apple-ID creds — see notarize.sh).
# Without credentials we skip and print how to do it, so the build still succeeds.
if [ -n "${DONTSPEAK_NOTARY_PROFILE:-}" ] || [ -n "${DONTSPEAK_APPLE_ID:-}" ]; then
  for ARCH in $ARCHES; do
    echo; echo "==> notarize DontSpeak-$ARCH.dmg"
    "$HERE/notarize.sh" "$OUTDIR/DontSpeak-$ARCH.dmg"
  done
else
  echo
  echo "==> NOT notarized (no credentials). To notarize + staple each DMG, run:"
  for ARCH in $ARCHES; do
    echo "      DONTSPEAK_NOTARY_PROFILE=<profile> macos/notarize.sh \"$OUTDIR/DontSpeak-$ARCH.dmg\""
  done
  echo "    (set up the profile once: xcrun notarytool store-credentials — see notarize.sh header)"
fi

echo; echo "==> Done. DMGs on $OUTDIR:"
for ARCH in $ARCHES; do ls -lh "$OUTDIR/DontSpeak-$ARCH.dmg"; done
echo "BUILD_ID=$BUILD_ID  signed-with=$([ "$SIGN" = "-" ] && echo ad-hoc || echo "${SIGN%% (*}…")"
