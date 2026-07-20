#!/usr/bin/env bash
# dist-apps.sh — signed (+ notarized) DontSpeak.app zips per arch.
# Output: $OUTDIR/dontspeak-<ver>-macos-<aarch64|x86_64>.app.zip (default Desktop).
# DONTSPEAK_ARCHES default arm64. No models in zip. DONTSPEAK_DIST=0 for ad-hoc.
set -euo pipefail

export MACOSX_DEPLOYMENT_TARGET=14.0
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

source "$HERE/bundle-lib.sh"   # compile_icon / assemble_app / resolve_sign_identity / product_version

VERSION="$(product_version)"
# Release asset uses aarch64; Swift uses arm64.
zip_name() { # $1 = apple arch (arm64|x86_64)
  case "$1" in arm64) echo "dontspeak-$VERSION-macos-aarch64.app.zip" ;;
               *)     echo "dontspeak-$VERSION-macos-$1.app.zip" ;; esac
}
HOST_SLIB="$RUST/target/release-ffi/libds_core.a"

[ -n "$CARGO" ] && [ -x "$CARGO" ] || { echo "ERROR: no usable cargo (set DONTSPEAK_CARGO, or install rustup / Homebrew rust)" >&2; exit 1; }

WORKROOT="$(mktemp -d)"
SLIB_BAK=""; [ -f "$HOST_SLIB" ] && { SLIB_BAK="$(mktemp)"; cp "$HOST_SLIB" "$SLIB_BAK"; }
cleanup() {
  rm -rf "$WORKROOT"
  [ -n "$SLIB_BAK" ] && cp "$SLIB_BAK" "$HOST_SLIB" && rm -f "$SLIB_BAK"
  true
}
trap cleanup EXIT

export DONTSPEAK_BUILD_ID="$(compute_build_id "$REPO")"
SIGN="$(resolve_sign_identity)"
# Fail before multi-minute builds if dist lacks Developer ID.
if [ "$DONTSPEAK_DIST" = "1" ] && [ "$SIGN" = "-" ]; then
  echo "ERROR: dist build needs a Developer ID Application identity — set DONTSPEAK_CODESIGN_ID," >&2
  echo "       or DONTSPEAK_DIST=0 for an ad-hoc, no-notarization build." >&2
  exit 1
fi

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
  ( cd "$RUST" && "$CARGO" build --release --locked --target "$TRIPLE" -p ds-helper )
  local HELPER="$RUST/target/$TRIPLE/release/ds-helper"
  [ -f "$HELPER" ] || { echo "no helper $HELPER" >&2; exit 1; }
  # CLI for self-wire (assemble_app reads DONTSPEAK_CLI_BIN).
  ( cd "$RUST" && "$CARGO" build --release --locked --target "$TRIPLE" -p dontspeak --bin dontspeak )
  local CLI="$RUST/target/$TRIPLE/release/dontspeak"
  [ -f "$CLI" ] || { echo "no dontspeak CLI $CLI" >&2; exit 1; }
  export DONTSPEAK_CLI_BIN="$CLI"

  echo "==> [3/6] stage staticlib for the linker"
  mkdir -p "$(dirname "$HOST_SLIB")"
  cp "$SLIB" "$HOST_SLIB"

  echo "==> [4/6] swift build --arch $SWARCH"
  local BIN; BIN="$(cd "$HERE" && swift build -c release --arch "$SWARCH" --show-bin-path)"
  # Nuke product dir: SwiftPM can relink stale and drop force_load of libds_core.a.
  rm -rf "$BIN"
  ( cd "$HERE" && swift build -c release --arch "$SWARCH" )
  local EXE="$BIN/DontSpeak"
  [ -x "$EXE" ] || { echo "no app binary $EXE" >&2; exit 1; }
  if ! require_engine_symbol "$EXE"; then
    echo "FATAL: $EXE is missing ds_engine_start — the Rust staticlib was not linked" >&2
    echo "       (force_load of libds_core.a dropped; size=$(stat -f%z "$EXE" 2>/dev/null) bytes)." >&2
    exit 1
  fi
  echo "    app:    $(file "$EXE" | sed 's/.*: //')"
  echo "    helper: $(file "$HELPER" | sed 's/.*: //')"

  local DONTSPEAK_MLX_DYLIB; DONTSPEAK_MLX_DYLIB="$(build_dontspeak_mlx_dylib "$SWARCH")"
  export DONTSPEAK_MLX_DYLIB

  # Per-arch ORT: DONTSPEAK_ORT_DYLIB_<arch> else ORT_GLOBAL (not live env — clobbered).
  local ORT_VAR="DONTSPEAK_ORT_DYLIB_${ARCH}"
  local ORT="${!ORT_VAR:-$ORT_GLOBAL}"
  export DONTSPEAK_ORT_DYLIB="$ORT"

  echo "==> [5/6] assemble + sign DontSpeak.app"
  local APP="$WORKROOT/$ARCH/DontSpeak.app"; mkdir -p "$WORKROOT/$ARCH"
  assemble_app "$APP" "$EXE" "$HELPER" "$ICONOUT/Assets.car" "$ICONOUT/AppIcon.icns" \
    "$HERE/Bundle/Info.plist" "$MENUBAR_SVG" "$SIGN"

  # Final backstop: CLI must not have clobbered DontSpeak on case-insensitive volumes.
  local shipped="$APP/Contents/MacOS/DontSpeak"
  if ! require_engine_symbol "$shipped"; then
    echo "FATAL: assembled $shipped has no ds_engine_start — the app executable is not the" >&2
    echo "       engine-linked SwiftUI app (likely the bundled CLI clobbered it). Aborting." >&2
    exit 1
  fi

  if [ -n "${DONTSPEAK_NOTARY_PROFILE:-}" ] || [ -n "${DONTSPEAK_APPLE_ID:-}" ]; then
    echo "==> [6/6] notarize + staple, then zip → $OUTDIR/$(zip_name "$ARCH")"
    "$HERE/notarize.sh" "$APP"
  else
    echo "==> [6/6] NOT notarized (no credentials) — zipping the signed app → $OUTDIR/$(zip_name "$ARCH")"
    echo "    (set DONTSPEAK_NOTARY_PROFILE or the APPLE_* trio to notarize; else first launch hits Gatekeeper)"
  fi
  local ZIP="$OUTDIR/$(zip_name "$ARCH")"
  rm -f "$ZIP"
  ditto -c -k --keepParent "$APP" "$ZIP"
  echo "    → $ZIP ($(du -h "$ZIP" | cut -f1))"
}

# Capture global ORT once before per-arch exports clobber it.
ORT_GLOBAL="${DONTSPEAK_ORT_DYLIB:-}"

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
echo "BUILD_ID=$DONTSPEAK_BUILD_ID  signed-with=$(sign_label "$SIGN")"
