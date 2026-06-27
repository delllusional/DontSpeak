#!/usr/bin/env bash
#
# build.sh — build the Rust FFI staticlib, then the SwiftUI macOS app.
#
# Steps:
#   1. cargo build the ds-core staticlib in the `release-ffi` profile
#      (inherits release, but does NOT strip and disables LTO so the
#      `#[no_mangle] extern "C"` symbols survive in libds_core.a — a
#      stripped/fat-LTO'd archive loses them and `-lds_core` fails to link).
#      This also regenerates the committed C header IF `cbindgen` is available
#      (the `--features cbindgen` regen is opt-in; the header is committed so the
#      default path needs no cbindgen).
#   2. swift build the app, linking that staticlib + the system frameworks the
#      staticlib transitively needs.
#
# Per the project constraints this BUILDS ONLY — it never runs the app/engine and
# never bundles/codesigns a runnable .app. It echoes the product paths.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd "$HERE/../../rust" && pwd)"

echo "==> [1/2] Building Rust FFI staticlib (release-ffi profile)…"
( cd "$RUST_DIR" && cargo build --profile release-ffi -p ds-core )

STATICLIB="$RUST_DIR/target/release-ffi/libds_core.a"
if [[ ! -f "$STATICLIB" ]]; then
    echo "ERROR: expected staticlib not found: $STATICLIB" >&2
    exit 1
fi
echo "    staticlib: $STATICLIB"

echo "==> [2/2] Building the SwiftUI app (swift build -c release)…"
# SwiftPM doesn't track the external C staticlib as a dependency, so a Rust-only
# change leaves the previously-linked executable in place (no Swift sources changed
# ⇒ no relink) and the app silently keeps the OLD engine. Drop the linked binary so
# swift build always relinks against the freshly-built libds_core.a.
rm -f "$HERE/.build/release/DontSpeak"
( cd "$HERE" && swift build -c release )

APP_BIN="$HERE/.build/release/DontSpeak"
echo
echo "==> Build complete."
echo "    Rust staticlib : $STATICLIB"
echo "    App executable : $APP_BIN"
echo "    (Bundling into a runnable .app — Info.plist at macos/Bundle/Info.plist —"
echo "     is intentionally NOT done here; the constraint is build-only.)"
