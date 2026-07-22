#!/usr/bin/env bash
# package.sh — build DontSpeak Linux distributables (the Linux analogue of the Windows
# portable-zip and the macOS .app.zip). Closes the "no Linux package" gap.
#
# Produces a portable tarball with no packaging toolchain. The target still needs the
# compatible GTK/libadwaita and audio runtime libraries documented by the installer.
#
#   .tar.gz — bin/ + .desktop + udev rule + an install.sh; extract & run install.sh
#
# Payload: the GTK host ds-gtk (hosts the engine in-process) + the MCP/hook bin
# dontspeak + the warm-synth helper ds-helper + dontspeak.desktop + app-icon.svg +
# the /dev/uinput udev rule + the canonical standalone uninstaller.
#
#   apps/linux/package.sh
#   OUTDIR=~/Desktop apps/linux/package.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
GTK_DIR="$HERE/gtk"
OUTDIR="${OUTDIR:-$REPO/dist}"
for a in "$@"; do case "$a" in
  # Header comment only, minus the shebang: from line 2, stop at the first non-# line.
  -h|--help) awk 'NR > 1 && !/^#/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"; exit 0 ;;
  *) echo "package.sh: unknown option '$a' (try --help)" >&2; exit 2 ;;
esac; done

# Strip any stray CR (a CRLF Cargo.toml — e.g. a Windows-checkout working tree — would
# otherwise put a carriage return into every artifact filename).
VERSION="$(python3 "$REPO/scripts/release/sync-workspace-version.py" --print 2>/dev/null | tr -d '\r\n')"
[ -n "$VERSION" ] || VERSION=0.0.0
ARCH="$(uname -m)"
mkdir -p "$OUTDIR"
echo "==> DontSpeak $VERSION ($ARCH) → $OUTDIR"

# ── 1. build the CLI bins (rust/ workspace) + the GTK host (standalone crate) ─────────────
# The GTK host links the engine in-process via ds-core; there is no standalone daemon bin.
echo "==> [1/2] cargo build --release (dontspeak + ds-helper + ds-gtk)"
( cd "$REPO/rust" && cargo build --release --locked -p dontspeak && \
  cargo build --release --locked -p ds-helper )
( cd "$GTK_DIR" && cargo build --release --locked )

RREL="$REPO/rust/target/release"
GREL="$GTK_DIR/target/release"
for b in "$GREL/ds-gtk" "$RREL/dontspeak" "$RREL/ds-helper"; do
  [ -x "$b" ] || { echo "MISSING build output: $b" >&2; exit 1; }
done

# ── 2. portable tarball ──────────────────────────────────────────────────────────────────
echo "==> [2/2] portable tarball"
PKG="dontspeak-$VERSION-linux-$ARCH"
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT INT TERM HUP
ROOT="$STAGE/$PKG"
install -d "$ROOT/bin" "$ROOT/share/applications" "$ROOT/share/icons/hicolor/scalable/apps" "$ROOT/udev" "$ROOT/licenses"
install -m0755 "$GREL/ds-gtk" "$RREL/dontspeak" "$RREL/ds-helper" "$ROOT/bin/"
install -m0644 "$HERE/dontspeak.desktop" "$ROOT/share/applications/dontspeak.desktop"
install -m0644 "$REPO/assets/app-icon.svg" "$ROOT/share/icons/hicolor/scalable/apps/dontspeak.svg"
install -m0644 "$HERE/udev-rule.txt" "$ROOT/udev/99-ds-input.rules"
install -m0644 "$REPO/LICENSE" "$ROOT/LICENSE"
install -m0644 "$REPO/NOTICE.md" "$ROOT/NOTICE.md"
install -m0644 "$REPO/licenses/Apache-2.0.txt" "$ROOT/licenses/Apache-2.0.txt"
install -m0644 "$REPO/licenses/voice-g2p-MIT.txt" "$ROOT/licenses/voice-g2p-MIT.txt"
install -m0644 "$REPO/licenses/Boson-Higgs-Audio-2-Community-License.txt" "$ROOT/licenses/Boson-Higgs-Audio-2-Community-License.txt"
install -m0644 "$REPO/licenses/Meta-Llama-3-Community-License.txt" "$ROOT/licenses/Meta-Llama-3-Community-License.txt"

# Self-contained installer inside the tarball (mirrors the Windows portable zip's run
# path). Shipped verbatim from tarball-install.sh — the single source; don't inline a
# copy here (packaging_sync.rs pins this stays a file copy). The uninstaller rides
# along too: install.sh places it on PATH as dontspeak-uninstall, so removal still
# works after the extracted dir is deleted.
install -m0755 "$HERE/tarball-install.sh" "$ROOT/install.sh"
install -m0755 "$REPO/scripts/install/bundle/uninstall.sh" "$ROOT/uninstall.sh"
printf 'DontSpeak %s (%s) portable bundle.\nRun ./install.sh to install into ~/.local/bin.\nUninstall later: dontspeak-uninstall\n' "$VERSION" "$ARCH" > "$ROOT/README.txt"

TARBALL="$OUTDIR/$PKG.tar.gz"
tar -C "$STAGE" -czf "$TARBALL" "$PKG"
echo "    → $TARBALL"

echo
echo "==> Done. Artifacts in $OUTDIR:"
ls -lh "$OUTDIR"/*ontspeak* 2>/dev/null || true
