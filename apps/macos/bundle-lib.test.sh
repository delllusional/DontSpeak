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

# Source fixture: build_shim globs $BUNDLE_LIB_DIR-rooted directories, so the stub compile has
# real paths to record. Without it the glob would expand to the literal pattern.
mkdir -p "$test_dir/DontSpeakMLX/Sources/DontSpeakSys" \
         "$test_dir/DontSpeakMLX/Sources/DontSpeakFluid" \
         "$test_dir/DontSpeakMLX/Sources/DontSpeakMLX"
: >"$test_dir/DontSpeakMLX/Sources/DontSpeakSys/Sys.swift"
: >"$test_dir/DontSpeakMLX/Sources/DontSpeakSys/SysLog.swift"
: >"$test_dir/DontSpeakMLX/Sources/DontSpeakFluid/Fluid.swift"
: >"$test_dir/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift"

xcodebuild() { return 99; }
xcrun() {
  [ "${1:-}" = "swiftc" ] || return 98
  shift
  local out="" system_only=0 intel_target=0
  # Record the FULL path of every Swift source this compile names: a basename could not tell
  # Sources/DontSpeakSys/Sys.swift apart from another target's file, which is exactly the
  # boundary the assertions below pin (the `sys` dylib must link no MLX/FluidAudio source).
  : >"$test_dir/swift-sources"
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
      *.swift)
        echo "$1" >>"$test_dir/swift-sources"
        ;;
    esac
    [ "$#" -gt 0 ] && shift
  done
  echo "$system_only" >"$test_dir/swift-system-only"
  echo "$intel_target" >"$test_dir/swift-intel-target"
  [ -n "$out" ] || return 97
  mkdir -p "${out%/*}"
  : >"$out"
}
lipo() { echo "x86_64"; }
nm() { cat "$test_dir/nm-output" 2>/dev/null || true; }
otool() { cat "$test_dir/otool-output" 2>/dev/null || true; }

# -- family lists ------------------------------------------------------------------------
[ "$(ds_shim_families arm64)" = "sys mlx fluid" ] \
  || fail "arm64 must ship all three shim families"
[ "$(ds_shim_families x86_64)" = "sys" ] \
  || fail "Intel must ship the system shim only"
if ds_shim_families powerpc >/dev/null 2>"$stderr_file"; then
  fail "unknown architecture was accepted by ds_shim_families"
fi
if DONTSPEAK_SHIMS="bogus" ds_shim_families arm64 >/dev/null 2>"$stderr_file"; then
  fail "DONTSPEAK_SHIMS accepted an unknown family"
fi
grep -q "not a arm64 family" "$stderr_file" || fail "unknown family error was not reported"
if DONTSPEAK_SHIMS="mlx" ds_shim_families x86_64 >/dev/null 2>"$stderr_file"; then
  fail "DONTSPEAK_SHIMS accepted a family this arch never builds"
fi

# -- the dependency-free system shim -----------------------------------------------------
sys="$(DONTSPEAK_DIST=1 build_shim sys x86_64 2>"$stderr_file")" \
  || fail "Intel distribution rejected the system-speech shim"
[ -f "$sys" ] || fail "Intel build returned no shim dylib"
# Every named source must come from Sources/DontSpeakSys, and there must be more than one --
# proving the glob, not a hardcoded file name. Any MLX/Fluid source here would link a heavy
# runtime into the dylib that has to build on Intel.
sys_source_count="$(grep -c . "$test_dir/swift-sources" 2>/dev/null || echo 0)"
[ "$sys_source_count" -ge 2 ] \
  || fail "sys shim build names $sys_source_count source files, want at least 2"
while IFS= read -r source; do
  [ -n "$source" ] || continue
  case "$source" in
    "$test_dir/DontSpeakMLX/Sources/DontSpeakSys/"*) ;;
    *) fail "sys shim build names a source outside DontSpeakSys: $source" ;;
  esac
done <"$test_dir/swift-sources"
if grep -q '/DontSpeakFluid/\|/Sources/DontSpeakMLX/' "$test_dir/swift-sources"; then
  fail "sys shim build linked another family's source"
fi
# SYSTEM_ONLY is deleted from the repo; a resurrected fence would silently re-couple families.
[ "$(cat "$test_dir/swift-system-only")" = 0 ] \
  || fail "sys shim build still passes -D SYSTEM_ONLY"
[ "$(cat "$test_dir/swift-intel-target")" = 1 ] \
  || fail "sys shim build did not target Intel"

if build_shim sys powerpc >/dev/null 2>"$stderr_file"; then
  fail "unknown architecture was accepted"
fi
grep -q "unsupported Swift architecture" "$stderr_file" \
  || fail "unknown architecture error was not reported"

# Empty source directory: name the directory rather than letting swiftc report a literal glob.
mv "$test_dir/DontSpeakMLX/Sources/DontSpeakSys" "$test_dir/sys-sources-away"
mkdir -p "$test_dir/DontSpeakMLX/Sources/DontSpeakSys"
if build_shim sys arm64 >/dev/null 2>"$stderr_file"; then
  fail "an empty Sources/DontSpeakSys was accepted"
fi
grep -q "no Swift sources in .*DontSpeakSys" "$stderr_file" \
  || fail "empty-source-dir error did not name the directory"
rmdir "$test_dir/DontSpeakMLX/Sources/DontSpeakSys"
mv "$test_dir/sys-sources-away" "$test_dir/DontSpeakMLX/Sources/DontSpeakSys"

# -- dist-vs-dev waiver, per family ------------------------------------------------------
if DONTSPEAK_DIST=1 build_shim fluid arm64 >/dev/null 2>"$stderr_file"; then
  fail "arm64 distribution accepted a failed fluid build"
fi
grep -q "dist build would ship without the fluid shim" "$stderr_file" \
  || fail "arm64 distribution failure did not name the family"

out="$(DONTSPEAK_DIST=1 DONTSPEAK_ALLOW_MISSING_SHIM=1 build_shim fluid arm64 2>"$stderr_file")" \
  || fail "DONTSPEAK_ALLOW_MISSING_SHIM did not waive a failed fluid build"
[ -z "$out" ] || fail "waived fluid build returned a dylib path"

out="$(DONTSPEAK_DIST=0 build_shim mlx arm64 2>"$stderr_file")" \
  || fail "arm64 development build did not allow MLX degradation"
[ -z "$out" ] || fail "failed arm64 development build returned an MLX dylib path"
grep -q "mlx shim unavailable" "$stderr_file" || fail "arm64 development failure did not warn"

# Dropping a default family in a dist build is the same class of silent subset.
if DONTSPEAK_DIST=1 DONTSPEAK_SHIMS="sys" build_shims arm64 >/dev/null 2>"$stderr_file"; then
  fail "a dist build dropped mlx+fluid without the waiver"
fi
grep -q "excluded by DONTSPEAK_SHIMS" "$stderr_file" \
  || fail "dropped-family error did not name the cause"
DONTSPEAK_DIST=1 DONTSPEAK_ALLOW_MISSING_SHIM=1 DONTSPEAK_SHIMS="sys" build_shims arm64 \
  >/dev/null 2>"$stderr_file" || fail "the waiver did not allow a deliberate subset"

# -- prebuilt (CI-cached) tree reuse, per family -----------------------------------------
# Every rejection must fall through to a real xcodebuild -- the stub fails, so "reused" and
# "rebuilt" are distinguishable by exit status alone.
make_prebuilt() {
  local family="$1" derived products bin
  derived="$(shim_derived_dir "$family" arm64)"
  products="$derived/Build/Products/Release"
  bin="$products/PackageFrameworks/dontspeak_$family.framework/Versions/A/dontspeak_$family"
  mkdir -p "${bin%/*}" "$derived/SourcePackages/checkouts"
  : >"$bin"
  echo "$bin"
}
mlx_prebuilt="$(make_prebuilt mlx)"
fluid_prebuilt="$(make_prebuilt fluid)"
mlx_products="$(shim_derived_dir mlx arm64)/Build/Products/Release"
mkdir -p "$mlx_products/mlx-swift_Cmlx.bundle/Contents/Resources"
: >"$mlx_products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib"

lipo() { echo "arm64"; }
if DONTSPEAK_DIST=1 build_shim mlx arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt tree was reused without DONTSPEAK_SHIM_REUSE_PREBUILT"
fi

out="$(DONTSPEAK_DIST=1 DONTSPEAK_SHIM_REUSE_PREBUILT=1 build_shim mlx arm64 2>"$stderr_file")" \
  || fail "matching prebuilt arm64 mlx tree was not reused"
[ "$out" = "$mlx_prebuilt" ] || fail "mlx reuse returned '$out', want '$mlx_prebuilt'"

# A fluid tree carries no metallib and must still be reusable; an mlx tree without one must not.
out="$(DONTSPEAK_DIST=1 DONTSPEAK_SHIM_REUSE_PREBUILT=1 build_shim fluid arm64 2>"$stderr_file")" \
  || fail "metallib-less prebuilt fluid tree was rejected"
[ "$out" = "$fluid_prebuilt" ] || fail "fluid reuse returned '$out', want '$fluid_prebuilt'"

mv "$mlx_products/mlx-swift_Cmlx.bundle" "$test_dir/metallib-away"
if DONTSPEAK_DIST=1 DONTSPEAK_SHIM_REUSE_PREBUILT=1 build_shim mlx arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt mlx tree without the Metal library was reused"
fi
mv "$test_dir/metallib-away" "$mlx_products/mlx-swift_Cmlx.bundle"

lipo() { echo "x86_64"; }
if DONTSPEAK_DIST=1 DONTSPEAK_SHIM_REUSE_PREBUILT=1 build_shim mlx arm64 >/dev/null 2>"$stderr_file"; then
  fail "prebuilt tree of the wrong arch was reused"
fi
lipo() { echo "arm64"; }

for family in mlx fluid; do
  mv "$(shim_derived_dir "$family" arm64)/SourcePackages" "$test_dir/checkouts-away"
  if DONTSPEAK_DIST=1 DONTSPEAK_SHIM_REUSE_PREBUILT=1 build_shim "$family" arm64 \
      >/dev/null 2>"$stderr_file"; then
    fail "prebuilt $family tree without the dependency checkouts was reused"
  fi
  mv "$test_dir/checkouts-away" "$(shim_derived_dir "$family" arm64)/SourcePackages"
done

# -- export + isolation gates ------------------------------------------------------------
: >"$test_dir/nm-output"
if verify_shim_exports sys "$sys" >/dev/null 2>"$stderr_file"; then
  fail "verify_shim_exports passed a dylib exporting nothing"
fi
grep -q "does not export ds_sys_available" "$stderr_file" \
  || fail "missing-export error did not name the symbol"
for symbol in $(shim_required_exports sys); do
  echo "0000000000001000 T _$symbol" >>"$test_dir/nm-output"
done
verify_shim_exports sys "$sys" >/dev/null 2>"$stderr_file" \
  || fail "verify_shim_exports rejected a complete export table"

: >"$test_dir/otool-output"
verify_shim_isolation sys "$sys" 2>"$stderr_file" \
  || fail "verify_shim_isolation rejected a dependency-free sys dylib"
echo "	@rpath/FluidAudio.framework/FluidAudio" >"$test_dir/otool-output"
if verify_shim_isolation sys "$sys" 2>"$stderr_file"; then
  fail "verify_shim_isolation accepted a sys dylib linking FluidAudio"
fi
: >"$test_dir/otool-output"

echo "0000000000002000 T _ds_fluid_tts_init" >>"$test_dir/nm-output"
if verify_shim_isolation mlx "$sys" 2>"$stderr_file"; then
  fail "verify_shim_isolation accepted an mlx dylib exporting ds_fluid_*"
fi
grep -q "FluidAudio is still linked" "$stderr_file" \
  || fail "mlx isolation failure did not explain itself"

# -- harvester arch + family scoping (the two-arch contamination regression) -------------
for family in mlx fluid; do
  products="$(shim_derived_dir "$family" arm64)/Build/Products/Release"
  mkdir -p "$products/$family-only.bundle"
  checkouts="$(shim_derived_dir "$family" arm64)/SourcePackages/checkouts"
  mkdir -p "$checkouts/$family-package" "$checkouts/swift-syntax"
  echo "legal" >"$checkouts/$family-package/LICENSE"
  echo "legal" >"$checkouts/swift-syntax/LICENSE"
done

bundle_swift_package_licenses "$test_dir/lic-arm64" arm64 sys mlx fluid >/dev/null \
  || fail "licence harvest failed for a full arm64 build"
[ -f "$test_dir/lic-arm64/mlx-package-LICENSE" ] \
  || fail "MLX package licences were dropped"
[ -f "$test_dir/lic-arm64/fluid-package-LICENSE" ] \
  || fail "FluidAudio package licences were dropped"
[ ! -f "$test_dir/lic-arm64/swift-syntax-LICENSE" ] \
  || fail "build-only swift-syntax licence was bundled"

bundle_swift_package_resources "$test_dir/res-arm64" arm64 sys mlx fluid >/dev/null \
  || fail "resource harvest failed for a full arm64 build"
[ -d "$test_dir/res-arm64/mlx-only.bundle" ] && [ -d "$test_dir/res-arm64/fluid-only.bundle" ] \
  || fail "resource harvest missed a family's bundles"

# The dist-apps.sh two-arch scenario: arm64 trees are still on disk while the Intel leg runs
# in the SAME process. Nothing arm64 may reach the Intel .app.
bundle_swift_package_resources "$test_dir/res-intel" x86_64 sys >/dev/null \
  || fail "Intel resource harvest failed"
[ -z "$(ls -A "$test_dir/res-intel" 2>/dev/null || true)" ] \
  || fail "Intel .app picked up arm64 resource bundles"
bundle_swift_package_licenses "$test_dir/lic-intel" x86_64 sys >/dev/null \
  || fail "Intel licence harvest failed"
[ -z "$(ls -A "$test_dir/lic-intel" 2>/dev/null || true)" ] \
  || fail "Intel .app picked up arm64 package licences"

# The metallib requirement is keyed on the family argument, not on arch or glob order.
bundle_swift_package_resources "$test_dir/res-fluid" arm64 sys fluid >/dev/null \
  || fail "a fluid-only harvest wrongly demanded the MLX Metal library"
mv "$mlx_products/mlx-swift_Cmlx.bundle" "$test_dir/metallib-away"
if bundle_swift_package_resources "$test_dir/res-nometal" arm64 sys mlx >/dev/null 2>"$stderr_file"; then
  fail "an mlx harvest without the Metal library was accepted"
fi
mv "$test_dir/metallib-away" "$mlx_products/mlx-swift_Cmlx.bundle"

echo "bundle-lib tests passed"
