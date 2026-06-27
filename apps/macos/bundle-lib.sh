#!/usr/bin/env bash
# bundle-lib.sh — shared DontSpeak.app assembly, sourced by BOTH:
#   • bundle.sh    (build + INSTALL on this machine), and
#   • dist-dmgs.sh (cross-arch distributable DMGs).
# Keeping the .app layout, icon compile, and signing in ONE place stops the two
# callers from drifting (resource list, Info.plist stamp, codesign identity).
# Source this file; do not execute it.

# macos/ dir (this lib lives there) — AppIcon.icon / Bundle/ resolve relative to it.
BUNDLE_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Shared helpers — compute_build_id + resolve_sign_identity (the lockstep BUILD_ID and
# codesign-identity resolution) live in scripts/lib/common.sh so the app bundler and the
# engine installer (scripts/install-daemon.sh) can't drift. ../../scripts from apps/macos.
. "$BUNDLE_LIB_DIR/../../scripts/lib/common.sh"

# product_version — the marketing version, from the SINGLE source of truth
# (rust/Cargo.toml [workspace.package] version) via the shared scripts/version.sh, so
# the bundle's CFBundleShortVersionString matches ds-core's `ds_version()`
# (CARGO_PKG_VERSION), the Windows installer, and the release tag. Falls back to
# "0.0.0" if it can't be read.
product_version() {
  local v=""
  v="$(bash "$BUNDLE_LIB_DIR/../../scripts/version.sh" 2>/dev/null)"
  printf '%s' "${v:-0.0.0}"
}

# app_display_name — the user-visible app name ("DontSpeak", WITH the space). SINGLE
# source of truth: the i18n catalog's `app.display_name`, the same string the app
# itself renders. The build stamps it into CFBundleName/CFBundleDisplayName so the
# bundle (Finder, the Privacy/Login-Items panes) can't drift from the in-app name —
# those plist keys can't read the catalog at runtime. Falls back to "DontSpeak".
app_display_name() {
  local yml="$BUNDLE_LIB_DIR/../../rust/crates/ds-i18n/locales/en.yml" n=""
  n="$(grep -m1 'display_name:' "$yml" 2>/dev/null | sed -E 's/.*display_name:[[:space:]]*//')"
  printf '%s' "${n:-DontSpeak}"
}

# compile_icon <out_dir> — actool AppIcon.icon → Assets.car + AppIcon.icns in <out_dir>.
# minimum-deployment-target stays 26.0 even though the app targets 13: AppIcon.icon is
# an Icon Composer (Liquid Glass) source that needs SDK 26 to compile; pre-26 systems
# fall back to the emitted AppIcon.icns (CFBundleIconFile).
compile_icon() {
  local out="$1"
  xcrun actool "$BUNDLE_LIB_DIR/AppIcon.icon" --compile "$out" --app-icon AppIcon \
    --enable-on-demand-resources NO --development-region en --target-device mac \
    --platform macosx --minimum-deployment-target 26.0 \
    --output-partial-info-plist "$out/icon.plist" >/dev/null
  [ -f "$out/Assets.car" ] || { echo "actool produced no Assets.car" >&2; return 1; }
}

# build_smkokoro_dylib SWARCH — build the FluidAudio Core ML / ANE Kokoro shim
# (libsmkokoro.dylib) for the apple-native TTS backend. The app is Apple-Silicon ONLY
# (FluidAudio needs macOS 14 + the ANE; there is no Intel build), so `swarch` is always
# arm64 — the non-arm64 guard below is just a defensive no-op. Echoes the dylib path on
# success; echoes nothing (and warns) otherwise so callers treat it as "not bundled".
build_smkokoro_dylib() {
  local swarch="$1"
  if [ "$swarch" != "arm64" ]; then
    echo "   skip libsmkokoro: Apple-Silicon only (arch $swarch)" >&2
    return 0
  fi
  local pkg="$BUNDLE_LIB_DIR/SmKokoro"
  if ! ( cd "$pkg" && swift build -c release --arch arm64 --product smkokoro >&2 ); then
    echo "   WARN: libsmkokoro build failed — apple-native TTS unavailable in this build" >&2
    return 0
  fi
  local bin
  bin="$(cd "$pkg" && swift build -c release --arch arm64 --product smkokoro --show-bin-path 2>/dev/null)"
  [ -f "$bin/libsmkokoro.dylib" ] && echo "$bin/libsmkokoro.dylib"
}

# assemble_app — build a signed DontSpeak.app from prebuilt parts. Args:
#   1 app(out)  2 exe  3 helper  4 assets_car  5 appicon_icns
#   6 info_plist(template)  7 menubar_svg  8 build_id  9 sign_identity(or "-")
# Honors DONTSPEAK_SMKOKORO_DYLIB (the apple-native Kokoro shim) if set → Frameworks.
assemble_app() {
  local app="$1" exe="$2" helper="$3" car="$4" icns="$5" plist="$6" mbsvg="$7" bid="$8" sign="$9"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$exe"    "$app/Contents/MacOS/DontSpeak"
  # The engine spawns its warm Kokoro child as a sibling of the app binary, so the
  # helper must live next to it in Contents/MacOS.
  cp "$helper" "$app/Contents/MacOS/ds-helper"
  # Bundle the FluidAudio Core ML / ANE Kokoro shim (apple-native TTS) when built. The
  # app points SMKOKORO_DYLIB_PATH at it; absent → the helper uses the ONNX path. Signed
  # below: --deep (dev) covers Frameworks; sign_app_dist (dist) signs it explicitly.
  local smk="${DONTSPEAK_SMKOKORO_DYLIB:-}"
  if [ -n "$smk" ] && [ -f "$smk" ]; then
    mkdir -p "$app/Contents/Frameworks"
    cp "$smk" "$app/Contents/Frameworks/libsmkokoro.dylib"
    echo "   bundled libsmkokoro ← $smk"
  fi
  # Bundle the speaker-SEPARATION model (SepFormer int8, ~30 MB) into Resources. The app
  # points DONTSPEAK_SEPARATOR_PATH at it for the dictation speaker-lock; absent → the lock
  # fails open (transcribes unfiltered).
  local sepm="$BUNDLE_LIB_DIR/models/sepformer_int8.onnx"
  if [ -f "$sepm" ]; then
    cp "$sepm" "$app/Contents/Resources/sepformer_int8.onnx"
    echo "   bundled separator ← $sepm"
  fi
  cp "$plist"  "$app/Contents/Info.plist"
  # Stamp the lockstep BUILD_ID (the engine carries the same id; the app's drift check
  # compares real ids, not "dev").
  plutil -replace DSBuildID -string "$bid" "$app/Contents/Info.plist"
  # Stamp the marketing version from the Rust workspace so the OS bundle version
  # (Finder "Get Info") matches the About screen's `ds_version()`. Single source.
  plutil -replace CFBundleShortVersionString -string "$(product_version)" "$app/Contents/Info.plist"
  # Stamp the display name from the i18n catalog so the bundle's name agrees with the
  # in-app name everywhere — one source (app.display_name), no drift.
  local app_name; app_name="$(app_display_name)"
  plutil -replace CFBundleName -string "$app_name" "$app/Contents/Info.plist"
  plutil -replace CFBundleDisplayName -string "$app_name" "$app/Contents/Info.plist"
  cp "$car"  "$app/Contents/Resources/Assets.car"
  cp "$icns" "$app/Contents/Resources/AppIcon.icns"
  # Menu-bar glyph: the VECTOR source (brandGlyph() prefers it, crisp at any size) plus
  # a 2× PNG fallback for renderers that fail the SVG load.
  if [ -f "$mbsvg" ]; then
    cp "$mbsvg" "$app/Contents/Resources/MenuBarIcon.svg"
    command -v rsvg-convert >/dev/null 2>&1 \
      && rsvg-convert -w 72 -h 72 "$mbsvg" -o "$app/Contents/Resources/MenuBarIcon.png" || true
  else
    # Loud, not silent: a missing MenuBarIcon makes the app fall back to the
    # `waveform.circle.fill` SF Symbol (the reorg path regression). Don't hide it.
    echo "   WARN: menu-bar icon '$mbsvg' not found — bundling NONE; the menu bar will fall back to the system waveform glyph" >&2
  fi
  if [ "${DONTSPEAK_DIST:-0}" = "1" ]; then
    sign_app_dist "$app" "$sign"
  else
    # Local/dev: ad-hoc or Apple Development, fast. ONE identity (app.dontspeak.org) so all
    # TCC grants land on this bundle; --deep also signs the bundled helper.
    codesign --force --deep --identifier app.dontspeak.org --sign "$sign" "$app"
  fi
}

# sign_app_dist — notarization-ready signing: bundle libonnxruntime.dylib, then sign
# INSIDE-OUT (nested code first, app last) with hardened runtime + secure timestamp +
# entitlements, no --deep. Args: 1 app  2 sign_identity.
sign_app_dist() {
  local app="$1" sign="$2"
  [ "$sign" != "-" ] || {
    echo "   ERROR: dist build needs a Developer ID Application identity (set DONTSPEAK_CODESIGN_ID)" >&2
    return 1
  }
  local ent="$BUNDLE_LIB_DIR/Bundle/DontSpeak.entitlements"

  # Bundle the onnxruntime dylib so it's signed + notarized with the app (a downloaded copy
  # would be Gatekeeper-quarantined on other Macs). Source: DONTSPEAK_ORT_DYLIB, else the
  # dev's downloaded copy under Application Support. Warn (don't fail) if absent.
  mkdir -p "$app/Contents/Frameworks"
  local ort="${DONTSPEAK_ORT_DYLIB:-}"
  [ -n "$ort" ] || ort="$(find "$HOME/Library/Application Support" -name libonnxruntime.dylib 2>/dev/null | head -1)"
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
  # The apple-native Kokoro shim (Apple-Silicon dist only), signed before the helper.
  [ -f "$app/Contents/Frameworks/libsmkokoro.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libsmkokoro.dylib"
  # The helper loads the third-party dylib too, so it needs the same entitlements.
  codesign "${opts[@]}" --entitlements "$ent" "$app/Contents/MacOS/ds-helper"
  codesign "${opts[@]}" --entitlements "$ent" --identifier app.dontspeak.org "$app"
  codesign --verify --strict --verbose=1 "$app" >&2
}
