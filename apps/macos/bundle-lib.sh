#!/usr/bin/env bash
# bundle-lib.sh — shared .app assembly (bundle.sh + dist-apps.sh). Source only.

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

# sign_label IDENTITY — short human label for a codesign identity ("ad-hoc" for "-").
sign_label() { [ "$1" = "-" ] && echo ad-hoc || echo "${1%% (*}…"; }

# legacy_icns — full 10-size AppIcon.icns from assets/app-icon.svg (actool stub is 16+128 only;
# macOS <26 needs CFBundleIconFile; Assets.car is Liquid Glass 26+ only).
legacy_icns() {
  local out="$1"
  local svg="$BUNDLE_LIB_DIR/../../assets/app-icon.svg"
  if ! command -v rsvg-convert >/dev/null 2>&1; then
    echo "   WARN: rsvg-convert not found (brew install librsvg) — keeping actool's stub" >&2
    echo "         AppIcon.icns (16px+128px only); the app icon degrades on macOS < 26." >&2
    return 0
  fi
  [ -f "$svg" ] || { echo "   WARN: $svg missing — keeping actool's stub .icns" >&2; return 0; }
  local set; set="$(mktemp -d)/AppIcon.iconset"; mkdir -p "$set"
  # <pixels>:<iconutil basename> — the two @1x/@2x rows per logical size iconutil expects.
  local s; for s in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x \
                    256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
    rsvg-convert -w "${s%%:*}" -h "${s%%:*}" "$svg" -o "$set/icon_${s#*:}.png"
  done
  # Apple's official packer. Overwrite the stub so assemble_app ships the full icns.
  iconutil -c icns "$set" -o "$out/AppIcon.icns" \
    && echo "   full AppIcon.icns ← assets/app-icon.svg (10 sizes)" \
    || echo "   WARN: iconutil failed — keeping actool's stub .icns" >&2
  rm -rf "$(dirname "$set")"
}

# compile_icon — actool Assets.car + full legacy icns. SDK 26 for AppIcon.icon.
compile_icon() {
  local out="$1"
  xcrun actool "$BUNDLE_LIB_DIR/AppIcon.icon" --compile "$out" --app-icon AppIcon \
    --enable-on-demand-resources NO --development-region en --target-device mac \
    --platform macosx --minimum-deployment-target 26.0 \
    --output-partial-info-plist "$out/icon.plist" >/dev/null
  [ -f "$out/Assets.car" ] || { echo "actool produced no Assets.car" >&2; return 1; }
  legacy_icns "$out"
}

# mlx_shim_missing WHY — dist builds (DONTSPEAK_DIST=1) fail loud (rc 1) unless waived via
# DONTSPEAK_ALLOW_MISSING_MLX=1; dev builds warn and continue (rc 0).
mlx_shim_missing() {
  if [ "${DONTSPEAK_DIST:-0}" = "1" ] && [ -z "${DONTSPEAK_ALLOW_MISSING_MLX:-}" ]; then
    echo "   ERROR: libdontspeak_mlx $1 — dist build would ship without MLX backends" >&2
    echo "          (BuiltIn TTS/STT silently degrade to ONNX-CPU on Apple Silicon)." >&2
    echo "          Set DONTSPEAK_ALLOW_MISSING_MLX=1 to waive." >&2
    return 1
  fi
  echo "   WARN: libdontspeak_mlx $1 — MLX backends unavailable in this build" >&2
  echo "         (BuiltIn TTS/STT degrade to ONNX-CPU)" >&2
}

# mlx_prebuilt_usable DERIVED SWARCH — is a restored (CI-cached) Xcode product tree shippable?
# Requires all three things assemble_app harvests: a dylib of the REQUESTED arch, MLX's Metal
# library, and the dependency checkouts the license bundler reads — a partial cache must
# rebuild rather than ship a wrong-arch or license-less app. Opt-in via
# DONTSPEAK_MLX_REUSE_PREBUILT: staleness is the cache key's problem (CI keys on
# Package.resolved + the shim sources + the Xcode build), so local builds keep rebuilding.
mlx_prebuilt_usable() {
  local derived="$1" swarch="$2"
  [ "${DONTSPEAK_MLX_REUSE_PREBUILT:-0}" = "1" ] || return 1
  local products="$derived/Build/Products/Release"
  local bin="$products/PackageFrameworks/dontspeak_mlx.framework/Versions/A/dontspeak_mlx"
  [ -f "$bin" ] || return 1
  [ -f "$products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" ] || return 1
  [ -d "$derived/SourcePackages/checkouts" ] || return 1
  lipo -archs "$bin" 2>/dev/null | tr ' ' '\n' | grep -qx "$swarch"
}

# build_dontspeak_mlx_dylib SWARCH — MLX on arm64; system-speech-only compatibility shim on Intel.
# Built-in Intel models stay on ORT CPU. The small Intel dylib retains System STT without linking
# MLX. MLX's Metal shaders are Xcode resources, so build the arm64 product through Xcode.
build_dontspeak_mlx_dylib() {
  local swarch="$1"
  case "$swarch" in
    arm64) ;;
    x86_64)
      local pkg="$BUNDLE_LIB_DIR/DontSpeakMLX"
      local source="$pkg/Sources/DontSpeakMLX/shim.swift"
      local out="$pkg/.build/system-$swarch/libdontspeak_mlx.dylib"
      mkdir -p "${out%/*}"
      if ! xcrun swiftc -parse-as-library -O -whole-module-optimization -D SYSTEM_ONLY \
          -target x86_64-apple-macosx14.0 -emit-library "$source" -o "$out" >&2; then
        echo "   ERROR: Intel system-speech shim build failed" >&2
        return 1
      fi
      [ -f "$out" ] || {
        echo "   ERROR: Intel system-speech shim produced no dylib" >&2
        return 1
      }
      echo "   Intel built-in models use ONNX CPU; bundled shim provides System STT" >&2
      echo "$out"
      return 0
      ;;
    *)
      echo "   ERROR: unsupported Swift architecture '$swarch' for libdontspeak_mlx" >&2
      return 2
      ;;
  esac
  local pkg="$BUNDLE_LIB_DIR/DontSpeakMLX"
  local derived="$pkg/.build/xcode-$swarch"
  local products="$derived/Build/Products/Release"
  local bin="$products/PackageFrameworks/dontspeak_mlx.framework/Versions/A/dontspeak_mlx"
  local metallib="$products/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib"
  # mlx-swift + mlx-audio-swift compile from source (~12 min) and change only when their
  # exact pins do — reuse a verified prebuilt tree instead of rebuilding it every release.
  if mlx_prebuilt_usable "$derived" "$swarch"; then
    echo "   reusing prebuilt libdontspeak_mlx ($swarch) ← $products" >&2
    echo "$bin"
    return 0
  fi
  if ! (cd "$pkg" && xcodebuild -scheme dontspeak_mlx -destination 'generic/platform=macOS' \
      -configuration Release -derivedDataPath "$derived" ARCHS="$swarch" ONLY_ACTIVE_ARCH=YES \
      build -quiet) >&2; then
    mlx_shim_missing "$swarch build failed" || return 1
    return 0
  fi
  # if/else not bare && — missing file must not kill set -e with empty contract.
  if [ -f "$bin" ] && [ -f "$metallib" ]; then
    echo "$bin"
  else
    mlx_shim_missing "dylib or Metal library missing after $swarch build" || return 1
  fi
}

# Copy SwiftPM resource bundles generated beside the Xcode-built MLX shim. In addition to the
# mandatory default.metallib this includes tokenizer/configuration and dependency resources.
bundle_swift_package_resources() {
  local out="$1" mlx="$2"
  local swarch products resource copied=0
  swarch="$(lipo -archs "$mlx" 2>/dev/null | awk '{print $1}')"
  if [ "$swarch" = "x86_64" ]; then
    echo "   Intel system-speech shim has no MLX resource bundles"
    return 0
  fi
  products="$BUNDLE_LIB_DIR/DontSpeakMLX/.build/xcode-$swarch/Build/Products/Release"
  mkdir -p "$out"
  for resource in "$products"/*.bundle; do
    [ -d "$resource" ] || continue
    cp -R "$resource" "$out/"
    copied=$((copied + 1))
  done
  [ -f "$out/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib" ] || {
    echo "   ERROR: MLX default.metallib was not bundled" >&2
    return 1
  }
  echo "   bundled $copied Swift package resource bundles"
}

# Copy the legal files for Swift packages linked into libdontspeak_mlx. SwiftSyntax is a
# build-only macro dependency of mlx-swift-lm and is not linked into the shipped dylib.
bundle_swift_package_licenses() {
  local out="$1" mlx="${2:-}"
  if [ -n "$mlx" ] && [ "$(lipo -archs "$mlx" 2>/dev/null | awk '{print $1}')" = "x86_64" ]; then
    return 0
  fi
  local pkg="$BUNDLE_LIB_DIR/DontSpeakMLX"
  local checkouts=""
  local candidate
  for candidate in "$pkg"/.build/xcode-*/SourcePackages/checkouts "$pkg/.build/checkouts"; do
    if [ -d "$candidate" ]; then
      checkouts="$candidate"
      break
    fi
  done
  [ -d "$checkouts" ] || return 0
  mkdir -p "$out"
  local checkout_dir package_name legal_file legal_name copied=0
  for checkout_dir in "$checkouts"/*; do
    [ -d "$checkout_dir" ] || continue
    package_name="${checkout_dir##*/}"
    [ "$package_name" = "swift-syntax" ] && continue
    for legal_file in "$checkout_dir"/LICENSE* "$checkout_dir"/NOTICE*; do
      [ -f "$legal_file" ] || continue
      legal_name="${legal_file##*/}"
      cp "$legal_file" "$out/$package_name-$legal_name"
      copied=$((copied + 1))
    done
  done
  echo "   bundled $copied Swift package license/notice files"
}

# strip_locals FILE — drop local symbols from a bundled Mach-O (`-x` keeps every external
# one, so `nm`'s `ds_engine_start` guard and the Rust side's `dlsym("ds_mlx_*")` still
# resolve). Called on the copy inside the .app and always before codesign, so signatures
# cover the stripped bytes. Best-effort: a strip failure costs size, never correctness.
# The Xcode-built MLX shim carries ~677k local symbols (~30 MB) — by far the biggest win.
strip_locals() {
  local bin="$1" before after
  before="$(stat -f%z "$bin" 2>/dev/null || echo 0)"
  if ! strip -x "$bin" 2>/dev/null; then
    echo "   WARN: strip -x $bin failed — shipping unstripped" >&2
    return 0
  fi
  after="$(stat -f%z "$bin" 2>/dev/null || echo 0)"
  echo "   stripped $(basename "$bin"): $before → $after bytes"
}

# assemble_app: 1 app 2 exe 3 helper 4 car 5 icns 6 plist 7 menubar_svg 8 sign
# Optional DONTSPEAK_MLX_DYLIB, DONTSPEAK_CLI_BIN.
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
    # Contents/Helpers — case-insensitive volume: MacOS/dontspeak would clobber DontSpeak.
    mkdir -p "$app/Contents/Helpers"
    cp "$cli" "$app/Contents/Helpers/dontspeak"
    echo "   bundled dontspeak CLI ← $cli"
  fi
  local mlx="${DONTSPEAK_MLX_DYLIB:-}"
  if [ -n "$mlx" ] && [ -f "$mlx" ]; then
    mkdir -p "$app/Contents/Frameworks"
    cp "$mlx" "$app/Contents/Frameworks/libdontspeak_mlx.dylib"
    strip_locals "$app/Contents/Frameworks/libdontspeak_mlx.dylib"
    echo "   bundled libdontspeak_mlx ← $mlx"
  fi
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
  if [ -n "$mlx" ] && [ -f "$mlx" ]; then
    bundle_swift_package_resources "$app/Contents/Resources" "$mlx"
    bundle_swift_package_licenses "$app/Contents/Resources/licenses/swift" "$mlx"
  fi
  if [ -f "$mbsvg" ]; then
    cp "$mbsvg" "$app/Contents/Resources/MenuBarIcon.svg"
    command -v rsvg-convert >/dev/null 2>&1 \
      && rsvg-convert -w 72 -h 72 "$mbsvg" -o "$app/Contents/Resources/MenuBarIcon.png" || true
  else
    echo "   WARN: menu-bar icon '$mbsvg' not found — bundling NONE; the menu bar will fall back to the system waveform glyph" >&2
  fi
  if [ "${DONTSPEAK_DIST:-0}" = "1" ]; then
    sign_app_dist "$app" "$sign"
  else
    # One identity (app.dontspeak.org) for shared TCC; --deep signs helper.
    codesign --force --deep --identifier app.dontspeak.org --sign "$sign" "$app"
  fi
}

# sign_app_dist — inside-out, hardened runtime + timestamp + entitlements (no --deep).
sign_app_dist() {
  local app="$1" sign="$2"
  [ "$sign" != "-" ] || {
    echo "   ERROR: dist build needs a Developer ID Application identity (set DONTSPEAK_CODESIGN_ID)" >&2
    return 1
  }
  local ent="$BUNDLE_LIB_DIR/Bundle/DontSpeak.entitlements"

  # onnxruntime ships OUT of the bundle: ds-model fetches the pinned dist into the model
  # cache (SHA-256 verified, Microsoft Developer-ID signed, no quarantine xattr — and the
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
    echo "   bundled onnxruntime ← $ort"
  else
    echo "   onnxruntime: fetched at first run (set DONTSPEAK_ORT_DYLIB to embed one instead)"
  fi

  local opts=(--force --options runtime --timestamp --sign "$sign")
  [ -f "$app/Contents/Frameworks/libonnxruntime.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libonnxruntime.dylib"
  [ -f "$app/Contents/Frameworks/libdontspeak_mlx.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libdontspeak_mlx.dylib"
  codesign "${opts[@]}" --entitlements "$ent" "$app/Contents/MacOS/ds-helper"
  [ -f "$app/Contents/Helpers/dontspeak" ] &&
    codesign "${opts[@]}" "$app/Contents/Helpers/dontspeak"
  codesign "${opts[@]}" --entitlements "$ent" --identifier app.dontspeak.org "$app"
  codesign --verify --strict --verbose=1 "$app" >&2
}
