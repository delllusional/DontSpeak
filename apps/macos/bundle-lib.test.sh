#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/bundle-lib.sh"

fail() {
  echo "bundle-lib test failed: $*" >&2
  exit 1
}

test_dir="$(mktemp -d)"
stderr_file="$test_dir/stderr"
trap 'rm -rf "$test_dir"' EXIT
BUNDLE_LIB_DIR="$test_dir"

if grep -Eq 'DONTSPEAK_SEPARATOR_PATH|sepformer_int8\.onnx' \
    "$HERE/Sources/DontSpeak/DontSpeakApp.swift"; then
  fail "macOS host still probes for the retired bundled separator"
fi

xcodebuild() { return 99; }
xcrun() {
  [ "${1:-}" = "swiftc" ] || return 98
  shift
  local out="" system_only=0 intel_target=0
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -D)
        shift
        [ "${1:-}" = "SYSTEM_ONLY" ] && system_only=1
        ;;
      -target)
        shift
        [ "${1:-}" = "x86_64-apple-macosx14.0" ] && intel_target=1
        ;;
      -o)
        shift
        out="${1:-}"
        ;;
    esac
    [ "$#" -gt 0 ] && shift
  done
  [ "$system_only" = 1 ] && [ "$intel_target" = 1 ] && [ -n "$out" ] || return 97
  mkdir -p "${out%/*}"
  : >"$out"
}
lipo() { echo "x86_64"; }

mlx="$(DONTSPEAK_DIST=1 build_dontspeak_mlx_dylib x86_64 2>"$stderr_file")" \
  || fail "Intel distribution rejected the system-speech shim"
[ -f "$mlx" ] || fail "Intel build returned no shim dylib"
grep -q "built-in models use ONNX CPU" "$stderr_file" \
  || fail "Intel build did not report the ONNX backend"
bundle_swift_package_resources "$test_dir/resources" "$mlx" >/dev/null \
  || fail "Intel shim incorrectly required MLX resources"

if build_dontspeak_mlx_dylib powerpc >/dev/null 2>"$stderr_file"; then
  fail "unknown architecture was accepted"
fi
grep -q "unsupported Swift architecture" "$stderr_file" \
  || fail "unknown architecture error was not reported"

if DONTSPEAK_DIST=1 build_dontspeak_mlx_dylib arm64 >/dev/null 2>"$stderr_file"; then
  fail "arm64 distribution accepted a failed MLX build"
fi
grep -q "dist build would ship without MLX backends" "$stderr_file" \
  || fail "arm64 distribution failure did not enforce MLX"

mlx="$(DONTSPEAK_DIST=0 build_dontspeak_mlx_dylib arm64 2>"$stderr_file")" \
  || fail "arm64 development build did not allow MLX degradation"
[ -z "$mlx" ] || fail "failed arm64 development build returned an MLX dylib path"
grep -q "MLX backends unavailable" "$stderr_file" \
  || fail "arm64 development failure did not warn"

# Cached-tree reuse (CI restores one instead of recompiling mlx-swift). Every rejection below
# must fall through to a real xcodebuild — the stub fails, so "reused" and "rebuilt" are
# distinguishable by exit status alone.
derived="$test_dir/DontSpeakMLX/.build/xcode-arm64"
products="$derived/Build/Products/Release"
prebuilt="$products/PackageFrameworks/dontspeak_mlx.framework/Versions/A/dontspeak_mlx"
mkdir -p "${prebuilt%/*}" "$products/mlx-swift_Cmlx.bundle/Contents/Resources" \
  "$derived/SourcePackages/checkouts"
: >"$prebuilt"
: >"$products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib"

lipo() { echo "arm64"; }
if DONTSPEAK_DIST=1 build_dontspeak_mlx_dylib arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt tree was reused without DONTSPEAK_MLX_REUSE_PREBUILT"
fi

mlx="$(DONTSPEAK_DIST=1 DONTSPEAK_MLX_REUSE_PREBUILT=1 build_dontspeak_mlx_dylib arm64 2>"$stderr_file")" \
  || fail "matching prebuilt arm64 tree was not reused"
[ "$mlx" = "$prebuilt" ] || fail "reuse returned '$mlx', want '$prebuilt'"

lipo() { echo "x86_64"; }
if DONTSPEAK_DIST=1 DONTSPEAK_MLX_REUSE_PREBUILT=1 build_dontspeak_mlx_dylib arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt tree of the wrong arch was reused"
fi

lipo() { echo "arm64"; }
rm -rf "$derived/SourcePackages"
if DONTSPEAK_DIST=1 DONTSPEAK_MLX_REUSE_PREBUILT=1 build_dontspeak_mlx_dylib arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt tree without the dependency checkouts was reused"
fi

echo "bundle-lib tests passed"
