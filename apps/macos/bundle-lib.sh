#!/usr/bin/env bash
# bundle-lib.sh — shared DontSpeak.app assembly, sourced by BOTH:
#   • bundle.sh    (build + INSTALL on this machine), and
#   • dist-apps.sh (cross-arch distributable DontSpeak.app zips).
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
  # tr strips a stray CR a CRLF working tree would otherwise put into the version.
  v="$(bash "$BUNDLE_LIB_DIR/../../scripts/version.sh" 2>/dev/null | tr -d '\r\n')"
  printf '%s' "${v:-0.0.0}"
}

# product_build_version — CFBundleVersion (the bundle "build number" key, distinct from
# CFBundleShortVersionString/the marketing version above). Apple expects this to be plain
# period-separated integers, so this strips any semver prerelease suffix (e.g.
# "0.1.1-dev" → "0.1.1") from the SAME single source rather than being a second,
# independently-set literal that could drift from it.
product_build_version() {
  product_version | cut -d- -f1
}

# app_display_name — the user-visible app name. SINGLE source of truth: the i18n
# catalog's `common.app_name`, the same string the app itself renders. The build stamps
# it into CFBundleName/CFBundleDisplayName so the bundle (Finder, the
# Privacy/Login-Items panes) can't drift from the in-app name — those plist keys can't
# read the catalog at runtime. Falls back to "DontSpeak". The sed strips the key, then
# any trailing inline YAML comment.
app_display_name() {
  local yml="$BUNDLE_LIB_DIR/../../rust/crates/ds-i18n/locales/en.yml" n=""
  n="$(grep -m1 '^  app_name:' "$yml" 2>/dev/null \
    | sed -E 's/.*app_name:[[:space:]]*//; s/[[:space:]]+#.*$//')"
  printf '%s' "${n:-DontSpeak}"
}

# sign_label IDENTITY — short human label for a codesign identity ("ad-hoc" for "-").
sign_label() { [ "$1" = "-" ] && echo ad-hoc || echo "${1%% (*}…"; }

# legacy_icns <out_dir> — write a COMPLETE traditional AppIcon.icns (all 10 iconset
# renditions, 16²…512²@2x) into <out_dir>, OVERWRITING actool's stub. Rendered from the
# repo's full-composition master assets/app-icon.svg (gradient squircle + bubble + </>)
# with rsvg-convert (the project's rasterizer, same as the menu-bar glyph) → iconutil.
#
# WHY THIS EXISTS: actool compiles AppIcon.icon into an Assets.car whose AppIcon is the
# macOS-26 `AssetType: "Icon Image"` (Liquid Glass) — a format ONLY macOS 26+ can read —
# and it emits a fallback AppIcon.icns carrying JUST the 16px + 128px renditions. So on
# macOS 14/15 the app icon can't come from Assets.car AND the .icns has no 32/256/512
# rendition → Finder/Dock show a blurry-or-generic icon (v0.1.0's "no app icon" bug on
# Intel/Sequoia). Shipping a full .icns as CFBundleIconFile is the supported dual path:
# Assets.car serves macOS 26, this .icns serves everything older.
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

# compile_icon <out_dir> — AppIcon.icon → Assets.car + AppIcon.icns in <out_dir>.
# actool compiles the Liquid Glass Assets.car (macOS 26, CFBundleIconName); legacy_icns then
# replaces actool's stub .icns with a COMPLETE one (CFBundleIconFile) for macOS < 26. The app
# ships BOTH — see legacy_icns for why the actool-emitted .icns alone is insufficient.
# minimum-deployment-target stays 26.0: AppIcon.icon needs SDK 26 to compile.
compile_icon() {
  local out="$1"
  xcrun actool "$BUNDLE_LIB_DIR/AppIcon.icon" --compile "$out" --app-icon AppIcon \
    --enable-on-demand-resources NO --development-region en --target-device mac \
    --platform macosx --minimum-deployment-target 26.0 \
    --output-partial-info-plist "$out/icon.plist" >/dev/null
  [ -f "$out/Assets.car" ] || { echo "actool produced no Assets.car" >&2; return 1; }
  legacy_icns "$out"
}

# build_smkokoro_dylib SWARCH — build the FluidAudio shim (libsmkokoro.dylib) for the
# apple-native backends. On arm64 that's Core ML / ANE Kokoro TTS + Parakeet STT + the
# System STT tier; on x86_64 the ANE backends degrade at runtime but the dylib still
# carries System STT (legacy SFSpeechRecognizer on macOS 14–25), so BOTH arches build it.
# Echoes the dylib path on success; echoes nothing (and warns) otherwise so callers treat
# it as "not bundled".
build_smkokoro_dylib() {
  local swarch="$1"
  local pkg="$BUNDLE_LIB_DIR/SmKokoro"
  # swift_build_resilient (common.sh) self-heals a stale .build module cache; >&2 keeps the
  # build chatter off this function's stdout (which returns the dylib path).
  if ! swift_build_resilient "$pkg" -c release --arch "$swarch" --product smkokoro >&2; then
    echo "   WARN: libsmkokoro build failed — apple-native backends unavailable in this build" >&2
    return 0
  fi
  local bin
  bin="$(cd "$pkg" && swift build -c release --arch "$swarch" --product smkokoro --show-bin-path 2>/dev/null)"
  # if/else, not a bare `X && Y` — as the function's LAST command, a missing dylib would
  # return 1 and kill `set -e` callers with no message, breaking the "echoes nothing
  # (and warns)" contract above.
  if [ -f "$bin/libsmkokoro.dylib" ]; then
    echo "$bin/libsmkokoro.dylib"
  else
    echo "   WARN: libsmkokoro.dylib missing after build — apple-native TTS not bundled" >&2
  fi
}

# assemble_app — build a signed DontSpeak.app from prebuilt parts. Args:
#   1 app(out)  2 exe  3 helper  4 assets_car  5 appicon_icns
#   6 info_plist(template)  7 menubar_svg  8 build_id  9 sign_identity(or "-")
# Honors DONTSPEAK_SMKOKORO_DYLIB (the apple-native Kokoro shim) if set → Frameworks.
assemble_app() {
  local app="$1" exe="$2" helper="$3" car="$4" icns="$5" plist="$6" mbsvg="$7" bid="$8" sign="$9"
  local repo; repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$exe"    "$app/Contents/MacOS/DontSpeak"
  # The engine spawns its warm Kokoro child as a sibling of the app binary, so the
  # helper must live next to it in Contents/MacOS.
  cp "$helper" "$app/Contents/MacOS/ds-helper"
  # Ship the `dontspeak` CLI (MCP server + hooks + `wire`) inside the bundle when built
  # (DONTSPEAK_CLI_BIN, set by dist-apps.sh) — an unzipped .app has no ~/.local/bin copy, so
  # it's what the installer wires with and what the MCP registration points at. Signed below;
  # absent → unchanged (dev bundle.sh installs it to ~/.local/bin separately).
  local cli="${DONTSPEAK_CLI_BIN:-}"
  if [ -n "$cli" ] && [ -f "$cli" ]; then
    # Contents/Helpers, NOT Contents/MacOS: the app executable is `DontSpeak` and the CLI is
    # `dontspeak`. macOS volumes are case-INSENSITIVE by default, so those two names are the
    # SAME path — bundling the CLI into Contents/MacOS silently OVERWRITES the app binary with
    # the CLI. The app then "launches" (it's really the CLI), does nothing, and exits 0: no
    # menu bar, no engine, no models. (This is exactly how v0.1.0 shipped a broken macOS app.)
    # A separate dir sidesteps the collision; `wire` self-registers via current_exe(), so the
    # CLI works from anywhere and the installer just runs this path.
    mkdir -p "$app/Contents/Helpers"
    cp "$cli" "$app/Contents/Helpers/dontspeak"
    echo "   bundled dontspeak CLI ← $cli"
  fi
  # Bundle the FluidAudio Core ML / ANE Kokoro shim (apple-native TTS) when built. The
  # app points SMKOKORO_DYLIB_PATH at it; absent → the helper uses the ONNX path. Signed
  # below: --deep (dev) covers Frameworks; sign_app_dist (dist) signs it explicitly.
  local smk="${DONTSPEAK_SMKOKORO_DYLIB:-}"
  if [ -n "$smk" ] && [ -f "$smk" ]; then
    mkdir -p "$app/Contents/Frameworks"
    cp "$smk" "$app/Contents/Frameworks/libsmkokoro.dylib"
    echo "   bundled libsmkokoro ← $smk"
  fi
  # The SepFormer speaker-lock separator is NO LONGER bundled: it moved out of the repo
  # into the standard download registry (ds-model urls.rs SEPFORMER — the engine
  # auto-fetches it when the lock is on). The app still sets DONTSPEAK_SEPARATOR_PATH
  # when a bundled copy exists, so pre-move bundles keep working.
  cp "$plist"  "$app/Contents/Info.plist"
  # Stamp the lockstep BUILD_ID (the engine carries the same id; the app's drift check
  # compares real ids, not "dev").
  plutil -replace DSBuildID -string "$bid" "$app/Contents/Info.plist"
  # Stamp the marketing version from the Rust workspace so the OS bundle version
  # (Finder "Get Info") matches the About screen's `ds_version()`. Single source.
  plutil -replace CFBundleShortVersionString -string "$(product_version)" "$app/Contents/Info.plist"
  # Stamp CFBundleVersion (the build-number key) from the SAME source — otherwise it
  # ships as the checked-in Info.plist template's literal "1" forever, unrelated to the
  # actual product version, on every build.
  plutil -replace CFBundleVersion -string "$(product_build_version)" "$app/Contents/Info.plist"
  # Stamp the display name from the i18n catalog so the bundle's name agrees with the
  # in-app name everywhere — one source (app.display_name), no drift.
  local app_name; app_name="$(app_display_name)"
  plutil -replace CFBundleName -string "$app_name" "$app/Contents/Info.plist"
  plutil -replace CFBundleDisplayName -string "$app_name" "$app/Contents/Info.plist"
  cp "$car"  "$app/Contents/Resources/Assets.car"
  cp "$icns" "$app/Contents/Resources/AppIcon.icns"
  # The helper embeds Apache-licensed Misaki dictionary data through voice-g2p. Bundle the
  # product license, attribution notice, and referenced license copies as readable resources.
  mkdir -p "$app/Contents/Resources/licenses"
  cp "$repo/LICENSE" "$app/Contents/Resources/LICENSE"
  cp "$repo/NOTICE.md" "$app/Contents/Resources/NOTICE.md"
  cp "$repo/licenses/"* "$app/Contents/Resources/licenses/"
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
  if [ -z "$ort" ]; then
    # `|| true` inside the substitution: find exits nonzero on any TCC-protected subdir
    # (com.apple.TCC, MobileSync, …) without Full Disk Access — even when the dylib WAS
    # found — and under set -e/pipefail that status would kill the whole dist build.
    ort="$(find "$HOME/Library/Application Support" -name libonnxruntime.dylib 2>/dev/null | head -1 || true)"
  fi
  # Never bundle a wrong-arch dylib: the fallback above finds the DEV machine's copy,
  # which on an Apple-Silicon box is arm64 — bundling it into an x86_64 slice would ship
  # a dylib the app can't load. Gate on the arch of the app binary being assembled.
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
  # The apple-native shim (both arches — carries System STT on Intel), signed before the helper.
  [ -f "$app/Contents/Frameworks/libsmkokoro.dylib" ] &&
    codesign "${opts[@]}" "$app/Contents/Frameworks/libsmkokoro.dylib"
  # The helper loads the third-party dylib too, so it needs the same entitlements.
  codesign "${opts[@]}" --entitlements "$ent" "$app/Contents/MacOS/ds-helper"
  # The bundled CLI (when shipped) — a nested Mach-O must be signed before the outer app,
  # else notarization rejects the bundle. No entitlements needed (MCP/hook client).
  [ -f "$app/Contents/Helpers/dontspeak" ] &&
    codesign "${opts[@]}" "$app/Contents/Helpers/dontspeak"
  codesign "${opts[@]}" --entitlements "$ent" --identifier app.dontspeak.org "$app"
  codesign --verify --strict --verbose=1 "$app" >&2
}
