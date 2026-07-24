#!/usr/bin/env bash
# bundle-lib.sh -- shared .app assembly (bundle.sh + dist-apps.sh). Source only.

BUNDLE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$BUNDLE_LIB_DIR/../../scripts/install/lib/common.sh"

# Marketing version from workspace Cargo.toml (matches ds_version / release tag).
product_version() {
  local v=""
  v="$(python3 "$BUNDLE_LIB_DIR/../../scripts/release/sync-workspace-version.py" --print 2>/dev/null | tr -d '\r\n')"
  printf '%s' "${v:-0.0.0}"
}

# CFBundleVersion: strip prerelease from same source (no independent literal).
product_build_version() {
  product_version | cut -d- -f1
}

# CFBundleName from i18n common.app_name (plist can't read catalog at runtime).
app_display_name() {
  local yml="$BUNDLE_LIB_DIR/../../rust/crates/ds-i18n/locales/en.yml" n=""
  n="$(grep -m1 '^  app_name:' "$yml" 2>/dev/null \
    | sed -E 's/.*app_name:[[:space:]]*//; s/[[:space:]]+#.*$//')"
  printf '%s' "${n:-DontSpeak}"
}

# sign_label IDENTITY -- short human label for a codesign identity ("ad-hoc" for "-").
sign_label() { [ "$1" = "-" ] && echo ad-hoc || echo "${1%% (*}..."; }

# legacy_icns -- full 10-size AppIcon.icns from assets/app-icon.svg (actool stub is 16+128 only;
# macOS <26 needs CFBundleIconFile; Assets.car is Liquid Glass 26+ only).
legacy_icns() {
  local out="$1"
  local svg="$BUNDLE_LIB_DIR/../../assets/app-icon.svg"
  if ! command -v rsvg-convert >/dev/null 2>&1; then
    echo "   WARN: rsvg-convert not found (brew install librsvg) -- keeping actool's stub" >&2
    echo "         AppIcon.icns (16px+128px only); the app icon degrades on macOS < 26." >&2
    return 0
  fi
  [ -f "$svg" ] || { echo "   WARN: $svg missing -- keeping actool's stub .icns" >&2; return 0; }
  local set; set="$(mktemp -d)/AppIcon.iconset"; mkdir -p "$set"
  # <pixels>:<iconutil basename> -- the two @1x/@2x rows per logical size iconutil expects.
  local s; for s in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x \
                    256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
    rsvg-convert -w "${s%%:*}" -h "${s%%:*}" "$svg" -o "$set/icon_${s#*:}.png"
  done
  # Apple's official packer. Overwrite the stub so assemble_app ships the full icns.
  iconutil -c icns "$set" -o "$out/AppIcon.icns" \
    && echo "   full AppIcon.icns <- assets/app-icon.svg (10 sizes)" \
    || echo "   WARN: iconutil failed -- keeping actool's stub .icns" >&2
  rm -rf "$(dirname "$set")"
}

# compile_icon -- actool Assets.car + full legacy icns. SDK 26 for AppIcon.icon.
compile_icon() {
  local out="$1"
  xcrun actool "$BUNDLE_LIB_DIR/AppIcon.icon" --compile "$out" --app-icon AppIcon \
    --enable-on-demand-resources NO --development-region en --target-device mac \
    --platform macosx --minimum-deployment-target 26.0 \
    --output-partial-info-plist "$out/icon.plist" >/dev/null
  [ -f "$out/Assets.car" ] || { echo "actool produced no Assets.car" >&2; return 1; }
  legacy_icns "$out"
}

# ds_shim_families SWARCH -- families this build ships, in bundle order. arm64 gets all
# three; Intel gets the dependency-free system shim only (no MLX/Core ML runtime there).
# DONTSPEAK_SHIMS overrides for experiments; a dist build that drops one still needs the
# DONTSPEAK_ALLOW_MISSING_SHIM waiver, so a release cannot quietly ship a subset.
ds_shim_families() {
  local swarch="$1" default="" family
  case "$swarch" in
    arm64)  default="sys mlx fluid" ;;
    x86_64) default="sys" ;;
    *)
      echo "   ERROR: unsupported Swift architecture '$swarch' for the speech shims" >&2
      return 2
      ;;
  esac
  if [ -z "${DONTSPEAK_SHIMS:-}" ]; then
    echo "$default"
    return 0
  fi
  for family in $DONTSPEAK_SHIMS; do
    case " $default " in
      *" $family "*) ;;
      *)
        echo "   ERROR: DONTSPEAK_SHIMS names '$family', not a $swarch family ($default)" >&2
        return 2
        ;;
    esac
  done
  echo "$DONTSPEAK_SHIMS"
}

shim_dylib_name() { echo "libdontspeak_$1.dylib"; }

# Xcode emits dynamic Swift package products as framework executables. Stage each one under
# the filename the Rust loader and app bundle contract use.
stage_shim_dylib() {
  local family="$1" source="$2" swarch="$3"
  local out="$BUNDLE_LIB_DIR/DontSpeakShims/.build/shims-$swarch/$(shim_dylib_name "$family")"
  mkdir -p "${out%/*}"
  cp "$source" "$out"
  echo "$out"
}

# shim_derived_dir FAMILY SWARCH -- the ONE naming rule for an xcodebuild product tree. Every
# consumer (build, prebuilt check, resource harvest, licence harvest, CI cache path) derives
# the same name from the pair, so no caller ever globs xcode-* and picks up another arch's or
# another family's tree. Separate roots per family also keep a Fluid edit from invalidating
# the ~12-minute mlx-swift tree.
shim_derived_dir() { echo "$BUNDLE_LIB_DIR/DontSpeakShims/.build/xcode-$1-$2"; }

# shim_missing FAMILY WHY -- dist builds (DONTSPEAK_DIST=1) fail loud (rc 1) unless waived via
# DONTSPEAK_ALLOW_MISSING_SHIM=1; dev builds warn and continue (rc 0).
shim_missing() {
  local family="$1" why="$2" degradation
  case "$family" in
    sys)   degradation="System STT is unavailable in this build" ;;
    mlx)   degradation="MLX BuiltIn TTS/STT degrade to ONNX-CPU" ;;
    fluid) degradation="Fluid BuiltIn TTS/STT degrade to ONNX-CPU" ;;
    *)     degradation="that backend is unavailable" ;;
  esac
  if [ "${DONTSPEAK_DIST:-0}" = "1" ] && [ -z "${DONTSPEAK_ALLOW_MISSING_SHIM:-}" ]; then
    echo "   ERROR: $(shim_dylib_name "$family") $why -- dist build would ship without the $family shim" >&2
    echo "          ($degradation)." >&2
    echo "          Set DONTSPEAK_ALLOW_MISSING_SHIM=1 to waive." >&2
    return 1
  fi
  echo "   WARN: $(shim_dylib_name "$family") $why -- $family shim unavailable in this build" >&2
  echo "         ($degradation)" >&2
}

# shim_prebuilt_usable FAMILY DERIVED SWARCH -- is a restored (CI-cached) Xcode product tree
# shippable? Requires everything assemble_app harvests: a dylib of the REQUESTED arch, MLX's
# Metal library (mlx only -- FluidAudio ships no metallib), and the dependency checkouts the
# license bundler reads. A partial cache must rebuild rather than ship a wrong-arch or
# license-less app. Opt-in via DONTSPEAK_SHIM_REUSE_PREBUILT, one flag for every
# xcodebuild-backed family: it means "trust the restored derived-data trees", which is a
# property of the JOB (a CI release build with restore-only caches), not of one family --
# staleness is the cache key's problem. Local builds keep rebuilding.
shim_prebuilt_usable() {
  local family="$1" derived="$2" swarch="$3"
  [ "${DONTSPEAK_SHIM_REUSE_PREBUILT:-0}" = "1" ] || return 1
  local products="$derived/Build/Products/Release"
  local bin="$products/PackageFrameworks/dontspeak_$family.framework/Versions/A/dontspeak_$family"
  [ -f "$bin" ] || return 1
  if [ "$family" = "mlx" ]; then
    [ -f "$products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" ] || return 1
  fi
  [ -d "$derived/SourcePackages/checkouts" ] || return 1
  lipo -archs "$bin" 2>/dev/null | tr ' ' '\n' | grep -qx "$swarch"
}

# build_shim FAMILY SWARCH -- echo the built dylib path on stdout (empty when waived).
# `sys` compiles with a bare swiftc call on ANY arch: it has no package dependencies, so no
# SwiftPM resolution and no network (~40 s). `mlx` and `fluid` go through Xcode because
# mlx-swift's default.metallib is an Xcode-only resource; `fluid` follows for uniformity with
# the prebuilt-reuse / cache / license-harvest machinery, which is written against
# derived-data trees.
build_shim() {
  local family="$1" swarch="$2"
  local pkg="$BUNDLE_LIB_DIR/DontSpeakShims"
  case "$swarch" in
    arm64|x86_64) ;;
    *)
      echo "   ERROR: unsupported Swift architecture '$swarch' for $(shim_dylib_name "$family")" >&2
      return 2
      ;;
  esac
  case "$family" in
    sys)
      local srcdir="$pkg/Sources/DontSpeakSys"
      # Globbed, not enumerated, so adding a source to the target cannot silently drop it from
      # the shipped dylib. A glob that matched nothing expands to the literal pattern and
      # swiftc would fail with a confusing "no such file" -- name the directory instead.
      local sources=("$srcdir"/*.swift)
      [ -e "${sources[0]}" ] || {
        echo "   ERROR: no Swift sources in $srcdir" >&2
        return 1
      }
      local out="$pkg/.build/sys-$swarch/libdontspeak_sys.dylib"
      mkdir -p "${out%/*}"
      if ! xcrun swiftc -parse-as-library -O -whole-module-optimization \
          -target "$swarch-apple-macosx14.0" -emit-library "${sources[@]}" -o "$out" >&2; then
        shim_missing sys "$swarch build failed" || return 1
        return 0
      fi
      if [ -f "$out" ]; then
        echo "$out"
      else
        shim_missing sys "produced no dylib after the $swarch build" || return 1
      fi
      return 0
      ;;
    mlx|fluid) ;;
    *)
      echo "   ERROR: unknown shim family '$family'" >&2
      return 2
      ;;
  esac
  if [ "$swarch" != "arm64" ]; then
    shim_missing "$family" "not built on Intel" || return 1
    return 0
  fi
  local derived; derived="$(shim_derived_dir "$family" "$swarch")"
  local products="$derived/Build/Products/Release"
  local bin="$products/PackageFrameworks/dontspeak_$family.framework/Versions/A/dontspeak_$family"
  local metallib="$products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib"
  # mlx-swift + mlx-audio-swift compile from source (~12 min) and change only when their
  # exact pins do -- reuse a verified prebuilt tree instead of rebuilding it every release.
  if shim_prebuilt_usable "$family" "$derived" "$swarch"; then
    echo "   reusing prebuilt $(shim_dylib_name "$family") ($swarch) <- $products" >&2
    stage_shim_dylib "$family" "$bin" "$swarch"
    return 0
  fi
  if ! (cd "$pkg" && xcodebuild -scheme "dontspeak_$family" -destination 'generic/platform=macOS' \
      -configuration Release -derivedDataPath "$derived" ARCHS="$swarch" ONLY_ACTIVE_ARCH=YES \
      build -quiet) >&2; then
    shim_missing "$family" "$swarch build failed" || return 1
    return 0
  fi
  # if/else not bare && -- missing file must not kill set -e with empty contract.
  if [ -f "$bin" ] && { [ "$family" != "mlx" ] || [ -f "$metallib" ]; }; then
    stage_shim_dylib "$family" "$bin" "$swarch"
  else
    shim_missing "$family" "dylib or Metal library missing after $swarch build" || return 1
  fi
}

# build_shims SWARCH -- newline-separated dylib paths for ds_shim_families, in bundle order.
build_shims() {
  local swarch="$1" families defaults family path
  families="$(ds_shim_families "$swarch")" || return $?
  defaults="$(DONTSPEAK_SHIMS= ds_shim_families "$swarch")" || return $?
  # A dist build that quietly drops a default family is exactly what the waiver is for.
  for family in $defaults; do
    case " $families " in
      *" $family "*) ;;
      *) shim_missing "$family" "excluded by DONTSPEAK_SHIMS" || return 1 ;;
    esac
  done
  case " $families " in
    *" mlx "*|*" fluid "*) ;;
    *) echo "   built-in models use ONNX CPU; bundled shim provides System STT" >&2 ;;
  esac
  for family in $families; do
    path="$(build_shim "$family" "$swarch")" || return $?
    [ -n "$path" ] && echo "$path"
  done
  return 0
}

# shim_required_exports FAMILY -- the C ABI each dylib must expose (dontspeak_<family>.h).
# Hand-maintained beside those headers: a @_cdecl string typo compiles clean in Swift, so
# this list plus verify_shim_exports is the only thing that catches one.
shim_required_exports() {
  case "$1" in
    sys)
      echo "ds_sys_available ds_sys_authorize ds_sys_transcribe ds_sys_stream_start" \
           "ds_sys_stream_push ds_sys_stream_finish ds_sys_set_log_cb"
      ;;
    mlx)
      echo "ds_mlx_tts_init ds_mlx_tts_synthesize2 ds_mlx_tts_shutdown ds_mlx_asr_init" \
           "ds_mlx_transcribe ds_mlx_asr_shutdown ds_mlx_asr_stream_start" \
           "ds_mlx_asr_stream_push ds_mlx_asr_stream_finish ds_mlx_asr_stream_shutdown" \
           "ds_mlx_diar_init ds_mlx_diarize ds_mlx_diar_embed ds_mlx_diar_shutdown" \
           "ds_mlx_set_log_cb"
      ;;
    fluid)
      echo "ds_fluid_tts_init ds_fluid_tts_synthesize_phonemes ds_fluid_tts_shutdown" \
           "ds_fluid_asr_init ds_fluid_transcribe ds_fluid_asr_shutdown" \
           "ds_fluid_asr_stream_start ds_fluid_asr_stream_push ds_fluid_asr_stream_finish" \
           "ds_fluid_asr_stream_shutdown ds_fluid_diar_init ds_fluid_diarize" \
           "ds_fluid_diar_embed ds_fluid_diar_shutdown ds_fluid_set_log_cb"
      ;;
    *) return 1 ;;
  esac
}

# verify_shim_exports FAMILY DYLIB -- every required symbol resolves as an exported text
# symbol. grep -c not -q (pipefail + SIGPIPE -> false failure), same as require_engine_symbol.
verify_shim_exports() {
  local family="$1" dylib="$2" table symbol n
  table="$(nm -gU "$dylib" 2>/dev/null || true)"
  for symbol in $(shim_required_exports "$family"); do
    n="$(printf '%s\n' "$table" | grep -cE " [Tt] _?$symbol\$" || true)"
    [ "${n:-0}" -gt 0 ] || {
      echo "   ERROR: $(basename "$dylib") does not export $symbol" >&2
      return 1
    }
  done
  echo "   verified $(basename "$dylib") exports $(shim_required_exports "$family" | wc -w | tr -d ' ') symbols"
}

# verify_shim_isolation FAMILY DYLIB -- the runtimes really are in separate images: `sys` links
# neither heavy runtime, and neither native family carries the other's entry points.
verify_shim_isolation() {
  local family="$1" dylib="$2" n
  case "$family" in
    sys)
      n="$(otool -L "$dylib" 2>/dev/null | grep -Ec 'mlx|Cmlx|FluidAudio' || true)"
      [ "${n:-0}" -eq 0 ] || {
        echo "   ERROR: $(basename "$dylib") links an MLX/FluidAudio runtime" >&2
        return 1
      }
      ;;
    mlx)
      n="$(nm -gU "$dylib" 2>/dev/null | grep -c ' _ds_fluid_' || true)"
      [ "${n:-0}" -eq 0 ] || {
        echo "   ERROR: $(basename "$dylib") exports ds_fluid_* -- FluidAudio is still linked" >&2
        return 1
      }
      ;;
    fluid)
      n="$(nm -gU "$dylib" 2>/dev/null | grep -c ' _ds_mlx_' || true)"
      [ "${n:-0}" -eq 0 ] || {
        echo "   ERROR: $(basename "$dylib") exports ds_mlx_* -- MLX is still linked" >&2
        return 1
      }
      ;;
    *)
      echo "   ERROR: unknown shim family '$family'" >&2
      return 1
      ;;
  esac
}

# bundle_swift_package_resources OUT SWARCH FAMILY... -- copy the SwiftPM resource bundles
# generated beside each xcodebuild-backed family's product (mandatory default.metallib for
# mlx, plus tokenizer/configuration and dependency resources). Each tree is COMPUTED from
# (family, swarch) -- never globbed -- because dist-apps.sh loops arches in ONE process with a
# constant BUNDLE_LIB_DIR, so a leftover arm64 tree would otherwise land in the Intel .app.
bundle_swift_package_resources() {
  local out="$1" swarch="$2"; shift 2
  local family families="" products resource copied=0 wants_mlx=0
  for family in "$@"; do
    case "$family" in
      mlx)   wants_mlx=1; families="$families $family" ;;
      fluid) families="$families $family" ;;
      *) ;;  # sys: bare swiftc, no derived data and no package resources
    esac
  done
  if [ -z "$families" ]; then
    echo "   system-speech-only build has no MLX resource bundles"
    return 0
  fi
  mkdir -p "$out"
  for family in $families; do
    products="$(shim_derived_dir "$family" "$swarch")/Build/Products/Release"
    [ -d "$products" ] || continue
    for resource in "$products"/*.bundle; do
      [ -d "$resource" ] || continue
      # Shared transitive bundles appear in both trees with identical content (same pins),
      # so an overwrite is a no-op rather than a conflict.
      cp -R "$resource" "$out/"
      copied=$((copied + 1))
    done
  done
  if [ "$wants_mlx" = 1 ] \
     && [ ! -f "$out/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" ]; then
    echo "   ERROR: MLX default.metallib was not bundled" >&2
    return 1
  fi
  echo "   bundled $copied Swift package resource bundles"
}

# bundle_swift_package_licenses OUT SWARCH FAMILY... -- the legal files for every Swift package
# linked into the shipped dylibs. UNION of the per-family checkouts trees, deduped by package
# name: stopping at the first match would silently drop one family's notices. SwiftSyntax is a
# build-only macro dependency of mlx-swift-lm and is not linked into any shipped dylib.
bundle_swift_package_licenses() {
  local out="$1" swarch="$2"; shift 2
  local family roots="" candidate
  for family in "$@"; do
    case "$family" in
      mlx|fluid)
        candidate="$(shim_derived_dir "$family" "$swarch")/SourcePackages/checkouts"
        [ -d "$candidate" ] && roots="$roots $candidate"
        ;;
      *) ;;
    esac
  done
  [ -n "$roots" ] || return 0
  mkdir -p "$out"
  local checkouts checkout_dir package_name legal_file legal_name copied=0 seen=""
  for checkouts in $roots; do
    for checkout_dir in "$checkouts"/*; do
      [ -d "$checkout_dir" ] || continue
      package_name="${checkout_dir##*/}"
      [ "$package_name" = "swift-syntax" ] && continue
      case " $seen " in *" $package_name "*) continue ;; esac
      seen="$seen $package_name"
      for legal_file in "$checkout_dir"/LICENSE* "$checkout_dir"/NOTICE*; do
        [ -f "$legal_file" ] || continue
        legal_name="${legal_file##*/}"
        cp "$legal_file" "$out/$package_name-$legal_name"
        copied=$((copied + 1))
      done
    done
  done
  echo "   bundled $copied Swift package license/notice files"
}

# strip_locals FILE -- drop local symbols from a bundled Mach-O (`-x` keeps every external
# one, so `nm`'s `ds_engine_start` guard and the Rust side's `dlsym("ds_{sys,mlx,fluid}_*")`
# still resolve). Called on the copy inside the .app and always before codesign, so signatures
# cover the stripped bytes. Best-effort: a strip failure costs size, never correctness.
# The Xcode-built MLX shim carries ~677k local symbols (~30 MB) -- by far the biggest win.
strip_locals() {
  local bin="$1" before after
  before="$(stat -f%z "$bin" 2>/dev/null || echo 0)"
  if ! strip -x "$bin" 2>/dev/null; then
    echo "   WARN: strip -x $bin failed -- shipping unstripped" >&2
    return 0
  fi
  after="$(stat -f%z "$bin" 2>/dev/null || echo 0)"
  echo "   stripped $(basename "$bin"): $before -> $after bytes"
}

# assemble_app: 1 app 2 exe 3 helper 4 car 5 icns 6 plist 7 menubar_svg 8 sign
# Optional DONTSPEAK_SHIM_DYLIBS (newline-separated absolute paths), DONTSPEAK_CLI_BIN.
assemble_app() {
  local app="$1" exe="$2" helper="$3" car="$4" icns="$5" plist="$6" mbsvg="$7" sign="$8"
  local repo; repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$exe"    "$app/Contents/MacOS/DontSpeak"
  strip_locals "$app/Contents/MacOS/DontSpeak"
  # Helper must sit next to app binary (engine spawns sibling).
  cp "$helper" "$app/Contents/MacOS/ds-helper"
  local cli="${DONTSPEAK_CLI_BIN:-}"
  if [ -n "$cli" ] && [ -f "$cli" ]; then
    # Contents/Helpers -- case-insensitive volume: MacOS/dontspeak would clobber DontSpeak.
    mkdir -p "$app/Contents/Helpers"
    cp "$cli" "$app/Contents/Helpers/dontspeak"
    echo "   bundled dontspeak CLI <- $cli"
  fi
  # One dylib per shim family. Verify the copy that will actually ship: after strip (so the
  # check sees the shipped bytes), before codesign.
  local shim_path shim_base shim_family bundled_families=""
  local swarch; swarch="$(lipo -archs "$app/Contents/MacOS/DontSpeak" 2>/dev/null | awk '{print $1}')"
  while IFS= read -r shim_path; do
    [ -n "$shim_path" ] && [ -f "$shim_path" ] || continue
    shim_base="$(basename "$shim_path")"
    shim_family="${shim_base#libdontspeak_}"
    shim_family="${shim_family%.dylib}"
    mkdir -p "$app/Contents/Frameworks"
    cp "$shim_path" "$app/Contents/Frameworks/$shim_base"
    strip_locals "$app/Contents/Frameworks/$shim_base"
    verify_shim_exports "$shim_family" "$app/Contents/Frameworks/$shim_base" || return 1
    verify_shim_isolation "$shim_family" "$app/Contents/Frameworks/$shim_base" || return 1
    bundled_families="$bundled_families $shim_family"
    echo "   bundled $shim_base <- $shim_path"
  done <<<"${DONTSPEAK_SHIM_DYLIBS:-}"
  cp "$plist"  "$app/Contents/Info.plist"
  plutil -replace CFBundleShortVersionString -string "$(product_version)" "$app/Contents/Info.plist"
  plutil -replace CFBundleVersion -string "$(product_build_version)" "$app/Contents/Info.plist"
  local app_name; app_name="$(app_display_name)"
  plutil -replace CFBundleName -string "$app_name" "$app/Contents/Info.plist"
  plutil -replace CFBundleDisplayName -string "$app_name" "$app/Contents/Info.plist"
  cp "$car"  "$app/Contents/Resources/Assets.car"
  cp "$icns" "$app/Contents/Resources/AppIcon.icns"
  install -m0755 "$repo/scripts/install/bundle/uninstall.sh" "$app/Contents/Resources/uninstall.sh"
  mkdir -p "$app/Contents/Resources/licenses"
  cp "$repo/LICENSE" "$app/Contents/Resources/LICENSE"
  cp "$repo/NOTICE.md" "$app/Contents/Resources/NOTICE.md"
  cp "$repo/licenses/"* "$app/Contents/Resources/licenses/"
  # Harvest from what SHIPPED, not from ds_shim_families: a family that failed to build and
  # was waived (or dropped via DONTSPEAK_SHIMS) must not leave its resources or licences behind.
  bundle_swift_package_resources "$app/Contents/Resources" "$swarch" $bundled_families
  bundle_swift_package_licenses "$app/Contents/Resources/licenses/swift" "$swarch" $bundled_families
  if [ -f "$mbsvg" ]; then
    cp "$mbsvg" "$app/Contents/Resources/MenuBarIcon.svg"
    command -v rsvg-convert >/dev/null 2>&1 \
      && rsvg-convert -w 72 -h 72 "$mbsvg" -o "$app/Contents/Resources/MenuBarIcon.png" || true
  else
    echo "   WARN: menu-bar icon '$mbsvg' not found -- bundling NONE; the menu bar will fall back to the system waveform glyph" >&2
  fi
  if [ "${DONTSPEAK_DIST:-0}" = "1" ]; then
    sign_app_dist "$app" "$sign"
  else
    # One identity (app.dontspeak.org) for shared TCC; --deep signs helper.
    codesign --force --deep --identifier app.dontspeak.org --sign "$sign" "$app"
  fi
}

# sign_app_dist -- inside-out, hardened runtime + timestamp + entitlements (no --deep).
sign_app_dist() {
  local app="$1" sign="$2"
  [ "$sign" != "-" ] || {
    echo "   ERROR: dist build needs a Developer ID Application identity (set DONTSPEAK_CODESIGN_ID)" >&2
    return 1
  }
  local ent="$BUNDLE_LIB_DIR/Bundle/DontSpeak.entitlements"

  # onnxruntime ships OUT of the bundle: ds-model fetches the pinned dist into the model
  # cache (SHA-256 verified, Microsoft Developer-ID signed, no quarantine xattr -- and the
  # hardened runtime already entitles disable-library-validation for exactly this dylib).
  # DONTSPEAK_ORT_DYLIB opts a build into embedding one instead; releases don't, so every
  # published app is the same artifact.
  mkdir -p "$app/Contents/Frameworks"
  local ort="${DONTSPEAK_ORT_DYLIB:-}"
  local app_arch
  app_arch="$(lipo -archs "$app/Contents/MacOS/DontSpeak" 2>/dev/null | awk '{print $1}' || true)"
  if [ -n "$ort" ] && [ -f "$ort" ] && [ -n "$app_arch" ] \
     && ! lipo -archs "$ort" 2>/dev/null | tr ' ' '\n' | grep -qx "$app_arch"; then
    echo "   skip onnxruntime bundle: $ort is not a $app_arch slice" >&2
    ort=""
  fi
  if [ -n "$ort" ] && [ -f "$ort" ]; then
    cp "$ort" "$app/Contents/Frameworks/libonnxruntime.dylib"
    echo "   bundled onnxruntime <- $ort"
  else
    echo "   onnxruntime: fetched at first run (set DONTSPEAK_ORT_DYLIB to embed one instead)"
  fi

  local opts=(--force --options runtime --timestamp --sign "$sign")
  # Every Frameworks member: onnxruntime plus one dylib per shim family.
  local member
  for member in "$app/Contents/Frameworks/"*.dylib; do
    [ -f "$member" ] && codesign "${opts[@]}" "$member"
  done
  codesign "${opts[@]}" --entitlements "$ent" "$app/Contents/MacOS/ds-helper"
  [ -f "$app/Contents/Helpers/dontspeak" ] &&
    codesign "${opts[@]}" "$app/Contents/Helpers/dontspeak"
  codesign "${opts[@]}" --entitlements "$ent" --identifier app.dontspeak.org "$app"
  codesign --verify --strict --verbose=1 "$app" >&2
}
