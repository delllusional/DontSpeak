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

# build_smkokoro_dylib SWARCH — both arches (System STT on Intel). Path on stdout or empty.
build_smkokoro_dylib() {
  local swarch="$1"
  local pkg="$BUNDLE_LIB_DIR/SmKokoro"
  if ! swift_build_resilient "$pkg" -c release --arch "$swarch" --product smkokoro >&2; then
    echo "   WARN: libsmkokoro build failed — apple-native backends unavailable in this build" >&2
    return 0
  fi
  local bin
  bin="$(cd "$pkg" && swift build -c release --arch "$swarch" --product smkokoro --show-bin-path 2>/dev/null)"
  # if/else not bare && — missing file must not kill set -e with empty contract.
  if [ -f "$bin/libsmkokoro.dylib" ]; then
    echo "$bin/libsmkokoro.dylib"
  else
    echo "   WARN: libsmkokoro.dylib missing after build — apple-native TTS not bundled" >&2
  fi
}

# assemble_app: 1 app 2 exe 3 helper 4 car 5 icns 6 plist 7 menubar_svg 8 sign
# Optional DONTSPEAK_SMKOKORO_DYLIB, DONTSPEAK_CLI_BIN.
assemble_app() {
  local app="$1" exe="$2" helper="$3" car="$4" icns="$5" plist="$6" mbsvg="$7" sign="$8"
  local repo; repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$exe"    "$app/Contents/MacOS/DontSpeak"
  # Helper must sit next to app binary (engine spawns sibling).
  cp "$helper" "$app/Contents/MacOS/ds-helper"
  local cli="${DONTSPEAK_CLI_BIN:-}"
  if [ -n "$cli" ] && [ -f "$cli" ]; then
    # Contents/Helpers — case-insensitive volume: MacOS/dontspeak would clobber DontSpeak.
    mkdir -p "$app/Contents/Helpers"
    cp "$cli" "$app/Contents/Helpers/dontspeak"
    echo "   bundled dontspeak CLI ← $cli"
  fi
  local smk="${DONTSPEAK_SMKOKORO_DYLIB:-}"
  if [ -n "$smk" ] && [ -f "$smk" ]; then
    mkdir -p "$app/Contents/Frameworks"
    cp "$smk" "$app/Contents/Frameworks/libsmkokoro.dylib"
    echo "   bundled libsmkokoro ← $smk"
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

  # Bundle matching-arch onnxruntime (Gatekeeper). DONTSPEAK_ORT_DYLIB or AS search.
  mkdir -p "$app/Contents/Frameworks"
  local ort="${DONTSPEAK_ORT_DYLIB:-}"
  if [ -z "$ort" ]; then
    # find may exit nonzero on TCC dirs — don't kill set -e.
    ort="$(find "$HOME/Library/Application Support" -name libonnxruntime.dylib 2>/dev/null | head -1 || true)"
  fi
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
    echo "   WARN: libonnxruntime.dylib not found — NOT bundled; the distributed app will download it" >&2
    echo "         at runtime (may be Gatekeeper-blocked). Set DONTSPEAK_ORT_DYLIB to bundle it." >&2
  fi

  local opts=(--force --options runtime --timestamp --sign "$sign")
  [ -f "$app/Contents/Frameworks/libonnxruntime.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libonnxruntime.dylib"
  [ -f "$app/Contents/Frameworks/libsmkokoro.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libsmkokoro.dylib"
  codesign "${opts[@]}" --entitlements "$ent" "$app/Contents/MacOS/ds-helper"
  [ -f "$app/Contents/Helpers/dontspeak" ] &&
    codesign "${opts[@]}" "$app/Contents/Helpers/dontspeak"
  codesign "${opts[@]}" --entitlements "$ent" --identifier app.dontspeak.org "$app"
  codesign --verify --strict --verbose=1 "$app" >&2
}
