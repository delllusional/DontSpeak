#!/usr/bin/env bash
# version.sh — print the project's SINGLE-SOURCE marketing version.
#
# The one place of truth is `rust/Cargo.toml` → [workspace.package] version. Every
# builder derives the version from here so the .dmg, the Windows installer, the in-app
# About string (ds-core's `ds_version()` == CARGO_PKG_VERSION), and the
# release tag can never drift:
#   • macOS  — apps/macos/bundle-lib.sh `product_version()` calls this script.
#   • CI     — .github/workflows/release.yml asserts the pushed tag == this version.
#   • Windows— apps/windows/installer/build.ps1 reads the SAME rust/Cargo.toml line
#              natively (PowerShell), so it stays in lockstep without invoking bash.
#
# Prints just the bare version (e.g. "0.1.0") to stdout; "0.0.0" if it can't be read.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cargo="$here/../rust/Cargo.toml"
v="$(grep -m1 '^version = "' "$cargo" 2>/dev/null | sed -E 's/version = "([^"]+)"/\1/')"
printf '%s' "${v:-0.0.0}"
