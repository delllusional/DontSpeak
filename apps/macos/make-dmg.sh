#!/usr/bin/env bash
# make-dmg.sh — build a styled drag-to-install DMG for DontSpeak.app.
#
# Produces the classic macOS installer window: the app icon on the left, an alias
# to /Applications on the right, an arrow + background image between them, so the
# user just drags the app onto Applications. The DMG carries NO models (they
# download on first launch) so it stays ~9 MB.
#
# Pipeline:
#   1. magick   — render .background/background.png (gradient + arrow + caption).
#   2. hdiutil  — create a read-WRITE UDRW image, copy in the .app + an
#                 /Applications symlink + the background.
#   3. osascript— drive Finder to set window bounds, icon size/positions, and the
#                 background picture (writes the volume's .DS_Store).
#   4. hdiutil  — convert to a compressed read-only UDZO image.
#
# Output: $1 (default macos/dist/DontSpeak.dmg). App: DONTSPEAK_APP_DIR or ~/Applications.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/../../scripts/lib/common.sh"   # find_codesign_id (+ PATH setup)
APP="${DONTSPEAK_APP_DIR:-$HOME/Applications/DontSpeak.app}"
# Volume name == the Finder installer window title. Stamp the single-source marketing
# version (rust/Cargo.toml via version.sh) into it, e.g. "DontSpeak 0.1.0".
VER="$(bash "$HERE/../../scripts/version.sh" 2>/dev/null || echo 0.0.0)"
VOL="DontSpeak $VER"
OUT="${1:-$HERE/dist/DontSpeak.dmg}"

[ -d "$APP" ] || { echo "ERROR: app not found: $APP (run macos/build.sh + bundle.sh first)" >&2; exit 1; }
command -v magick >/dev/null || { echo "ERROR: ImageMagick (magick) required for the background" >&2; exit 1; }

STAGE="$(mktemp -d)"
WORK="$(mktemp -u).dmg"
MNT="/Volumes/$VOL"
mkdir -p "$(dirname "$OUT")"
cleanup() { hdiutil detach "$MNT" >/dev/null 2>&1 || true; rm -rf "$STAGE" "$WORK"; }
trap cleanup EXIT

# Volume + DMG icon. actool's fallback AppIcon.icns is 256px-ONLY, which the Finder
# title-bar proxy (16px) can't render → it shows a GENERIC icon. So build a proper
# multi-size .icns from the vector app-icon source (crisp from 16px up). Fall back to
# the app's 256-only icns if the source / tools are missing.
# Repo-root assets/ is ../../ from apps/macos (same apps/ reorg gotcha as bundle.sh).
ICON_SVG="$(cd "$HERE/../.." && pwd)/assets/icon.svg"
ICNS="$APP/Contents/Resources/AppIcon.icns"
if [ -f "$ICON_SVG" ] && command -v rsvg-convert >/dev/null && command -v iconutil >/dev/null; then
  SET="$STAGE/icon.iconset"; mkdir -p "$SET"; ok=1
  for s in 16 32 128 256 512; do
    rsvg-convert -w "$s" -h "$s" "$ICON_SVG" -o "$SET/icon_${s}x${s}.png" 2>/dev/null || ok=0
    rsvg-convert -w "$((s*2))" -h "$((s*2))" "$ICON_SVG" -o "$SET/icon_${s}x${s}@2x.png" 2>/dev/null || ok=0
  done
  if [ "$ok" = 1 ] && iconutil -c icns "$SET" -o "$STAGE/VolumeIcon.icns" 2>/dev/null; then
    ICNS="$STAGE/VolumeIcon.icns"
    echo "   volume/DMG icon ← assets/icon.svg (multi-size icns, 16–1024px)"
  fi
fi

echo "==> [1/4] background image"
BG="$STAGE/background.png"
# Designed SVG asset (vector → retina-crisp, editable): an abstract soft-pastel
# aurora with faint "sound" ripples radiating from the app + a minimal arrow.
# Falls back to a plain pastel gradient if the SVG or rsvg-convert is unavailable.
SVG_BG="$HERE/dmg-background.svg"
if [ -f "$SVG_BG" ] && command -v rsvg-convert >/dev/null 2>&1; then
  rsvg-convert -w 700 -h 420 "$SVG_BG" -o "$BG"
else
  magick -size 700x420 gradient:'#f1ecfb'-'#e6f3ec' \
    -stroke '#8d7fb0' -strokewidth 4 -fill '#8d7fb0' \
    -draw "line 282,178 408,178" -stroke none -draw "polygon 400,166 400,190 430,178" "$BG"
fi

echo "==> [2/4] read-write image + payload"
hdiutil detach "$MNT" >/dev/null 2>&1 || true
# Size the read-write image to FIT the payload. The old fixed 80m overflowed once the
# self-contained dist bundle started carrying libonnxruntime (~37M) + sepformer (~30M) +
# libsmkokoro on top of the app — "No space left on device" mid-ditto. Measure the app and
# add headroom for the background image, the /Applications symlink, and HFS+ overhead.
APP_MB="$(du -sm "$APP" | cut -f1)"
SIZE_MB=$(( APP_MB + 60 ))
hdiutil create -size "${SIZE_MB}m" -volname "$VOL" -fs HFS+ -ov "$WORK" >/dev/null
hdiutil attach "$WORK" -mountpoint "$MNT" -nobrowse -noautoopen >/dev/null
mkdir -p "$MNT/.background"
cp "$BG" "$MNT/.background/background.png"
ditto "$APP" "$MNT/$(basename "$APP")"
ln -s /Applications "$MNT/Applications"
# NOTE: the volume icon is set AFTER the Finder styling step below — Finder rewrites
# the volume root's FinderInfo while arranging the window, wiping an earlier bit.

echo "==> [3/4] style the Finder window"
osascript <<EOF
tell application "Finder"
  tell disk "$VOL"
    open
    set current view of container window to icon view
    set toolbar visible of container window to false
    set statusbar visible of container window to false
    set the bounds of container window to {200, 150, 900, 598}
    set opts to the icon view options of container window
    set arrangement of opts to not arranged
    set icon size of opts to 128
    set text size of opts to 13
    set background picture of opts to file ".background:background.png"
    set position of item "$(basename "$APP")" of container window to {185, 172}
    set position of item "Applications" of container window to {515, 172}
    update without registering applications
    delay 1
    close
  end tell
end tell
EOF

# Volume icon AFTER Finder styling (Finder clears it if set earlier): the app's icon as
# .VolumeIcon.icns + the icnC creator on it + the custom-icon FinderInfo bit on the root,
# so the mounted installer's window/title-bar proxy shows the app icon, not generic.
if [ -f "$ICNS" ]; then
  cp "$ICNS" "$MNT/.VolumeIcon.icns"
  SetFile -c icnC "$MNT/.VolumeIcon.icns"
  SetFile -a C "$MNT" || echo "   WARN: SetFile custom-icon bit failed (volume icon may be generic)" >&2
fi
sync

echo "==> [4/4] compress to read-only UDZO"
hdiutil detach "$MNT" >/dev/null
rm -f "$OUT"
hdiutil convert "$WORK" -format UDZO -imagekey zlib-level=9 -o "$OUT" >/dev/null

# Give the .dmg FILE itself the app icon too (so it shows in Finder before mount):
# embed the icns as an 'icns' resource on the file + set the custom-icon bit.
if [ -f "$ICNS" ]; then
  cp "$ICNS" "$STAGE/icon.icns"
  sips -i "$STAGE/icon.icns" >/dev/null 2>&1 || true
  DeRez -only icns "$STAGE/icon.icns" > "$STAGE/icon.rsrc" 2>/dev/null || true
  if [ -s "$STAGE/icon.rsrc" ]; then
    Rez -append "$STAGE/icon.rsrc" -o "$OUT" 2>/dev/null || true
    SetFile -a C "$OUT" 2>/dev/null || true
  fi
fi

# Dist: sign the .dmg with Developer ID (AFTER the icon-resource embed above, which would
# otherwise invalidate the signature). notarize.sh then notarizes + staples it.
if [ "${DONTSPEAK_DIST:-0}" = "1" ]; then
  SIGN_ID="$(find_codesign_id)"   # DONTSPEAK_DIST=1 here → Developer ID Application only
  if [ -n "$SIGN_ID" ]; then
    codesign --force --timestamp --sign "$SIGN_ID" "$OUT"
    echo "   signed DMG ← ${SIGN_ID%% (*}…"
  else
    echo "   WARN: no Developer ID Application identity — DMG left unsigned (set DONTSPEAK_CODESIGN_ID)" >&2
  fi
fi

echo "DMG → $OUT ($(du -h "$OUT" | cut -f1))"
