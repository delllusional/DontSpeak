#!/usr/bin/env bash
# package.sh — build DontSpeak Linux distributables (the Linux analogue of the Windows
# installer/portable-zip and the macOS .dmg). Closes the "no Linux package" gap.
#
# Always produces a self-contained PORTABLE TARBALL (works on any distro, no packaging
# toolchain). Additionally produces a .deb, .rpm, and AppImage when the respective tool
# is installed — each is best-effort and skipped (with a hint) if its tool is missing, so
# the tarball is the guaranteed baseline.
#
#   .tar.gz  — always (bin/ + .desktop + udev rule + an install.sh; extract & run install.sh)
#   .deb     — needs `cargo deb`        (cargo install cargo-deb)
#   .rpm     — needs `cargo generate-rpm`(cargo install cargo-generate-rpm)
#   AppImage — needs `linuxdeploy` + linuxdeploy-plugin-gtk on PATH  (EXPERIMENTAL)
#
# Payload (all formats): the GTK host ds-gtk (hosts the engine in-process) + the
# MCP/hook bin dontspeak + the warm-synth helper ds-helper + dontspeak.desktop +
# app-icon.svg + the /dev/uinput udev rule. The .deb/.rpm layout is declared in
# apps/linux/gtk/Cargo.toml ([package.metadata.deb] / [package.metadata.generate-rpm]).
#
#   apps/linux/package.sh                 # all formats, OUTDIR=./dist
#   OUTDIR=~/Desktop apps/linux/package.sh
#   apps/linux/package.sh --skip-appimage # tarball + deb + rpm only
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GTK_DIR="$HERE/gtk"
OUTDIR="${OUTDIR:-$REPO/dist}"
SKIP_APPIMAGE=0
for a in "$@"; do case "$a" in --skip-appimage) SKIP_APPIMAGE=1 ;; -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;; esac; done

# Strip any stray CR (a CRLF Cargo.toml — e.g. a Windows-checkout working tree — would
# otherwise put a carriage return into every artifact filename).
VERSION="$(bash "$REPO/scripts/version.sh" 2>/dev/null | tr -d '\r\n')"
[ -n "$VERSION" ] || VERSION=0.0.0
case "$(uname -m)" in
  x86_64)  ARCH=x86_64;  DEB_ARCH=amd64 ;;
  aarch64) ARCH=aarch64; DEB_ARCH=arm64 ;;
  *) ARCH="$(uname -m)"; DEB_ARCH="$ARCH" ;;
esac
mkdir -p "$OUTDIR"
echo "==> DontSpeak $VERSION ($ARCH) → $OUTDIR"

# ── 1. build the CLI bins (rust/ workspace) + the GTK host (standalone crate) ─────────────
# The GTK host links the engine in-process via ds-core; there is no standalone daemon bin.
echo "==> [1/5] cargo build --release (dontspeak + ds-helper + ds-gtk)"
( cd "$REPO/rust" && cargo build --release -p dontspeak && \
  cargo build --release -p ds-tts --bin ds-helper )
( cd "$GTK_DIR" && cargo build --release )

RREL="$REPO/rust/target/release"
GREL="$GTK_DIR/target/release"
for b in "$GREL/ds-gtk" "$RREL/dontspeak" "$RREL/ds-helper"; do
  [ -x "$b" ] || { echo "MISSING build output: $b" >&2; exit 1; }
done

# ── 2. portable tarball (always) ─────────────────────────────────────────────────────────
echo "==> [2/5] portable tarball"
PKG="dontspeak-$VERSION-$ARCH"
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/$PKG"
install -d "$ROOT/bin" "$ROOT/share/applications" "$ROOT/share/icons/hicolor/scalable/apps" "$ROOT/udev"
install -m0755 "$GREL/ds-gtk" "$RREL/dontspeak" "$RREL/ds-helper" "$ROOT/bin/"
install -m0644 "$HERE/dontspeak.desktop" "$ROOT/share/applications/dontspeak.desktop"
install -m0644 "$REPO/assets/app-icon.svg" "$ROOT/share/icons/hicolor/scalable/apps/dontspeak.svg"
install -m0644 "$HERE/udev-rule.txt" "$ROOT/udev/99-ds-input.rules"

# Self-contained installer inside the tarball (mirrors the Windows portable zip's run path).
cat > "$ROOT/install.sh" <<'INSTALL'
#!/usr/bin/env bash
# Portable DontSpeak installer — copies binaries to ~/.local/bin, installs the launcher,
# wires the Claude Code hooks, and prints the one sudo step (/dev/uinput access).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
install -d "$BIN" "$APPS" "$ICONS"
install -m0755 "$HERE"/bin/* "$BIN/"
sed "s|^Exec=ds-gtk|Exec=$BIN/ds-gtk|" "$HERE/share/applications/dontspeak.desktop" > "$APPS/dontspeak.desktop"
install -m0644 "$HERE/share/icons/hicolor/scalable/apps/dontspeak.svg" "$ICONS/dontspeak.svg"
"$BIN/dontspeak" wire --all 2>/dev/null || echo "(wire skipped)"
echo
echo "Installed to $BIN. To grant /dev/uinput (synthetic keys), once:"
echo "  sudo install -m0644 '$HERE/udev/99-ds-input.rules' /etc/udev/rules.d/99-ds-input.rules"
echo "  sudo udevadm control --reload && sudo udevadm trigger && sudo usermod -aG input \"\$USER\"   # then re-login"
echo "Launch: $BIN/ds-gtk  (or the \"DontSpeak\" app menu entry)"
INSTALL
chmod 0755 "$ROOT/install.sh"
printf 'DontSpeak %s (%s) portable bundle.\nRun ./install.sh to install into ~/.local/bin.\n' "$VERSION" "$ARCH" > "$ROOT/README.txt"

TARBALL="$OUTDIR/$PKG.tar.gz"
tar -C "$STAGE" -czf "$TARBALL" "$PKG"
echo "    → $TARBALL"

# ── 3. .deb (best-effort) ────────────────────────────────────────────────────────────────
echo "==> [3/5] .deb"
if cargo deb --version >/dev/null 2>&1; then
  # --no-build: reuse the release bins we just built (cargo-deb would only rebuild the gtk crate).
  ( cd "$GTK_DIR" && cargo deb --no-build --output "$OUTDIR/dontspeak_${VERSION}_${DEB_ARCH}.deb" ) \
    && echo "    → $OUTDIR/dontspeak_${VERSION}_${DEB_ARCH}.deb" \
    || echo "    !! cargo deb failed (see above)"
else
  echo "    (skip — install with: cargo install cargo-deb)"
fi

# ── 4. .rpm (best-effort) ────────────────────────────────────────────────────────────────
echo "==> [4/5] .rpm"
if cargo generate-rpm --version >/dev/null 2>&1; then
  ( cd "$GTK_DIR" && cargo generate-rpm ) \
    && { mv -f "$GTK_DIR"/target/generate-rpm/*.rpm "$OUTDIR/" 2>/dev/null || true; echo "    → $OUTDIR/*.rpm"; } \
    || echo "    !! cargo generate-rpm failed (see above)"
else
  echo "    (skip — install with: cargo install cargo-generate-rpm)"
fi

# ── 5. AppImage (best-effort, EXPERIMENTAL — needs GTK bundling) ──────────────────────────
echo "==> [5/5] AppImage"
if [ "$SKIP_APPIMAGE" = "1" ]; then
  echo "    (skipped — --skip-appimage)"
elif command -v linuxdeploy >/dev/null 2>&1 && command -v linuxdeploy-plugin-gtk >/dev/null 2>&1; then
  APPDIR="$STAGE/AppDir"
  install -d "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps"
  install -m0755 "$GREL/ds-gtk" "$RREL/dontspeak" "$RREL/ds-helper" "$APPDIR/usr/bin/"
  install -m0644 "$HERE/dontspeak.desktop" "$APPDIR/usr/share/applications/dontspeak.desktop"
  install -m0644 "$REPO/assets/app-icon.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/dontspeak.svg"
  ( cd "$STAGE" && OUTPUT="$OUTDIR/DontSpeak-$VERSION-$ARCH.AppImage" \
      linuxdeploy --appdir "$APPDIR" --plugin gtk \
        --desktop-file "$APPDIR/usr/share/applications/dontspeak.desktop" \
        --icon-file "$APPDIR/usr/share/icons/hicolor/scalable/apps/dontspeak.svg" \
        --output appimage ) \
    && echo "    → $OUTDIR/DontSpeak-$VERSION-$ARCH.AppImage" \
    || echo "    !! AppImage build failed (GTK bundling is finicky — verify on the target)"
else
  echo "    (skip — needs linuxdeploy + linuxdeploy-plugin-gtk on PATH)"
fi

echo
echo "==> Done. Artifacts in $OUTDIR:"
ls -lh "$OUTDIR" 2>/dev/null | grep -E "dontspeak|DontSpeak" || true
