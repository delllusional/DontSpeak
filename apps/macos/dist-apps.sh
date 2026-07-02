#!/usr/bin/env bash
# dist-apps.sh — build the DontSpeak.app bundle(s) and zip each for distribution.
#
# Produces a signed + notarized + stapled DontSpeak.app packaged as
# dontspeak-<version>-macos-<aarch64|x86_64>.app.zip.
# The one-command installer (web/install.sh) unzips it straight into /Applications.
#
# Builds the arch slices named in $DONTSPEAK_ARCHES (default "arm64"; "x86_64" for the
# Intel slice, or "arm64 x86_64" for both). The apple-native Kokoro/Parakeet/diarization
# stack (FluidAudio Core ML / ANE) is Apple-Silicon ONLY, so an x86_64 slice ships WITHOUT
# libsmkokoro and falls back to the portable ONNX TTS/STT path — it still runs on Intel.
#
# The zip ships what the app needs plus the CLI: the app binary (hosts the engine
# in-process via the linked staticlib) + ds-helper (the Kokoro synth child) + the
# multi-call `dontspeak` CLI (MCP server + hooks + `wire`, so an unzipped .app can
# self-wire) + the icon. NO models — they download on first launch.
#
# Output: $OUTDIR/dontspeak-<version>-macos-<arch>.app.zip   (default OUTDIR=~/Desktop)
# Requires: a cargo that can target each arch (rustup for a CROSS slice; any cargo for a
#           NATIVE slice). Override with $DONTSPEAK_CARGO.
set -euo pipefail

# Pin the macOS deployment target for the Rust objects too (matches Package.swift's .v14).
export MACOSX_DEPLOYMENT_TARGET=14.0

# DISTRIBUTION build by default: hardened runtime + secure timestamp + entitlements +
# bundled onnxruntime dylib (via bundle-lib.sh), then notarized + stapled below. Override
# with DONTSPEAK_DIST=0 for an ad-hoc, no-notarization build.
export DONTSPEAK_DIST="${DONTSPEAK_DIST:-1}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # apps/macos/
REPO="$(cd "$HERE/../.." && pwd)"
RUST="$REPO/rust"
CARGO="${DONTSPEAK_CARGO:-}"
if [ -z "$CARGO" ]; then
  if [ -x "$HOME/.cargo/bin/cargo" ]; then CARGO="$HOME/.cargo/bin/cargo"
  else CARGO="$(command -v cargo || true)"; fi
fi
OUTDIR="${OUTDIR:-$HOME/Desktop}"
MENUBAR_SVG="$REPO/assets/menubar-icon.svg"
VERSION="$(bash "$REPO/scripts/version.sh" 2>/dev/null | tr -d '\r\n')"
[ -n "$VERSION" ] || VERSION=0.0.0
# Asset-name arch token: uname-style (aarch64/x86_64), uniform across every platform's
# release asset — the Swift/Apple toolchain keeps using the apple-style $ARCH (arm64).
zip_name() { # $1 = apple arch (arm64|x86_64)
  case "$1" in arm64) echo "dontspeak-$VERSION-macos-aarch64.app.zip" ;;
               *)     echo "dontspeak-$VERSION-macos-$1.app.zip" ;; esac
}
HOST_SLIB="$RUST/target/release-ffi/libds_core.a"

source "$HERE/bundle-lib.sh"   # compile_icon / assemble_app / resolve_sign_identity

[ -n "$CARGO" ] && [ -x "$CARGO" ] || { echo "ERROR: no usable cargo (set DONTSPEAK_CARGO, or install rustup / Homebrew rust)" >&2; exit 1; }

WORKROOT="$(mktemp -d)"
SLIB_BAK=""; [ -f "$HOST_SLIB" ] && { SLIB_BAK="$(mktemp)"; cp "$HOST_SLIB" "$SLIB_BAK"; }
cleanup() {
  rm -rf "$WORKROOT"
  [ -n "$SLIB_BAK" ] && cp "$SLIB_BAK" "$HOST_SLIB" && rm -f "$SLIB_BAK"   # restore host lib
  true
}
trap cleanup EXIT

BUILD_ID="$(compute_build_id "$REPO")"
SIGN="$(resolve_sign_identity)"   # Apple Development / ad-hoc trip Gatekeeper on OTHER Macs;
                                  # clean distribution needs Developer ID + notarization.

echo "==> actool icon (shared by both arches)"
ICONOUT="$WORKROOT/icon"; mkdir -p "$ICONOUT"
compile_icon "$ICONOUT"

mkdir -p "$OUTDIR"

build_arch() {   # $1 display arch, $2 rust triple, $3 swift arch
  local ARCH="$1" TRIPLE="$2" SWARCH="$3"
  echo; echo "################## $ARCH ($TRIPLE) ##################"

  echo "==> [1/6] cargo staticlib ($TRIPLE)"
  ( cd "$RUST" && "$CARGO" build --profile release-ffi --locked --target "$TRIPLE" -p ds-core )
  local SLIB="$RUST/target/$TRIPLE/release-ffi/libds_core.a"
  [ -f "$SLIB" ] || { echo "no staticlib $SLIB" >&2; exit 1; }

  echo "==> [2/6] cargo ds-helper + dontspeak ($TRIPLE)"
  ( cd "$RUST" && "$CARGO" build --release --locked --target "$TRIPLE" -p ds-tts --bin ds-helper )
  local HELPER="$RUST/target/$TRIPLE/release/ds-helper"
  [ -f "$HELPER" ] || { echo "no helper $HELPER" >&2; exit 1; }
  # The multi-call CLI (MCP server + hooks + `wire`) — shipped inside the .app so an
  # unzipped bundle can self-wire (assemble_app reads DONTSPEAK_CLI_BIN).
  ( cd "$RUST" && "$CARGO" build --release --locked --target "$TRIPLE" -p dontspeak --bin dontspeak )
  local CLI="$RUST/target/$TRIPLE/release/dontspeak"
  [ -f "$CLI" ] || { echo "no dontspeak CLI $CLI" >&2; exit 1; }
  export DONTSPEAK_CLI_BIN="$CLI"

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

  # apple-native Kokoro shim (Apple-Silicon only); assemble_app bundles it.
  local DONTSPEAK_SMKOKORO_DYLIB; DONTSPEAK_SMKOKORO_DYLIB="$(build_smkokoro_dylib "$SWARCH")"
  export DONTSPEAK_SMKOKORO_DYLIB

  # Self-contained: bundle the MATCHING-arch onnxruntime dylib per-arch (see the long note
  # in bundle-lib.sh / sign_app_dist). x86_64 macOS ships without one (uses say/claude_code).
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

  # Notarize + staple the .app itself (notarize.sh zips it for submission, staples the app),
  # so the ticket travels inside the distribution zip. Skipped without credentials.
  if [ -n "${DONTSPEAK_NOTARY_PROFILE:-}" ] || [ -n "${DONTSPEAK_APPLE_ID:-}" ]; then
    echo "==> [6/6] notarize + staple, then zip → $OUTDIR/$(zip_name "$ARCH")"
    "$HERE/notarize.sh" "$APP"
  else
    echo "==> [6/6] NOT notarized (no credentials) — zipping the signed app → $OUTDIR/$(zip_name "$ARCH")"
    echo "    (set DONTSPEAK_NOTARY_PROFILE or the APPLE_* trio to notarize; else first launch hits Gatekeeper)"
  fi
  local ZIP="$OUTDIR/$(zip_name "$ARCH")"
  rm -f "$ZIP"
  # ditto --keepParent so the archive contains DontSpeak.app/ at its root (a plain unzip
  # yields /Applications/DontSpeak.app). This is also the notary-friendly zip format.
  ditto -c -k --keepParent "$APP" "$ZIP"
  echo "    → $ZIP ($(du -h "$ZIP" | cut -f1))"
}

ARCHES="${DONTSPEAK_ARCHES:-arm64}"
for A in $ARCHES; do
  case "$A" in
    arm64)  build_arch arm64  aarch64-apple-darwin arm64  ;;
    x86_64) build_arch x86_64 x86_64-apple-darwin  x86_64 ;;
    *) echo "ERROR: unknown arch '$A' (want arm64 and/or x86_64)" >&2; exit 1 ;;
  esac
done

echo; echo "==> Done. App zips on $OUTDIR:"
for ARCH in $ARCHES; do ls -lh "$OUTDIR/$(zip_name "$ARCH")"; done
echo "BUILD_ID=$BUILD_ID  signed-with=$([ "$SIGN" = "-" ] && echo ad-hoc || echo "${SIGN%% (*}…")"
