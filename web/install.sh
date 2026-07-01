#!/bin/sh
# DontSpeak one-command installer — macOS + Linux.
#
#   curl -fsSL https://dontspeak.org/install.sh | sh
#
# Downloads the prebuilt app for this OS/arch from the latest GitHub Release,
# verifies its SHA-256, installs it, wires the MCP server + voice hooks into every
# detected client (`dontspeak wire --all`), and launches the app once so the voice
# models download themselves on first boot. No compiler required.
#
# Programmers who want a from-source build should instead clone the repo and run
# scripts/install.sh (this script never builds).
#
# Env overrides:
#   DONTSPEAK_REPO         owner/repo to fetch releases from (default delllusional/DontSpeak)
#   DONTSPEAK_DOWNLOAD_BASE  override the whole asset base URL (e.g. a dontspeak.org mirror)
#   DONTSPEAK_DRY_RUN=1    resolve + print the plan, download nothing
set -eu

REPO="${DONTSPEAK_REPO:-delllusional/DontSpeak}"
API="https://api.github.com/repos/$REPO/releases/latest"
DRY="${DONTSPEAK_DRY_RUN:-0}"

say()  { printf '==> %s\n' "$*"; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

# ── HTTP: prefer curl, fall back to wget ─────────────────────────────────────
if command -v curl >/dev/null 2>&1; then
  http_get()  { curl -fsSL "$1"; }                 # to stdout
  http_dl()   { curl -fsSL -o "$2" "$1"; }         # url file
elif command -v wget >/dev/null 2>&1; then
  http_get()  { wget -qO- "$1"; }
  http_dl()   { wget -qO "$2" "$1"; }
else
  die "need curl or wget on PATH"
fi

# Resolve a release asset's download URL by a basename regex, off the latest release.
# Uses the GitHub API so a versioned name (the Linux tarball) resolves without knowing
# the version. DONTSPEAK_DOWNLOAD_BASE, if set, short-circuits to <base>/<name> — best-effort
# for the fixed-name assets (dmg/exe/checksums); it can't reconstruct the versioned tarball.
asset_url() {  # $1 = extended-regex matching the asset filename
  pat="$1"
  if [ -n "${DONTSPEAK_DOWNLOAD_BASE:-}" ]; then
    lit=$(printf '%s' "$pat" | sed 's/\\//g')   # unescape the ERE to a literal name
    printf '%s/%s\n' "${DONTSPEAK_DOWNLOAD_BASE%/}" "$lit"; return 0
  fi
  http_get "$API" \
    | grep -o '"browser_download_url": *"[^"]*"' \
    | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/' \
    | grep -E "/$pat($|\?)" \
    | head -n1
}

# Verify $1 against the sha256 line for its basename in the checksums.txt at $2 (a URL).
verify_sha() {  # $1 = file, $2 = checksums url  (skips cleanly if unavailable)
  file="$1"; sums_url="$2"; base=$(basename "$file")
  sums=$(http_get "$sums_url" 2>/dev/null || true)
  [ -n "$sums" ] || { warn "no checksums.txt on the release — skipping integrity check"; return 0; }
  # Match either sha256sum format: text "<hash>  name" or binary "<hash> *name" — i.e. the
  # separator right before the basename is a space or a '*'.
  want=$(printf '%s\n' "$sums" | grep -E "[ *]$base\$" | awk '{print $1}' | head -n1)
  [ -n "$want" ] || { warn "$base not listed in checksums.txt — skipping integrity check"; return 0; }
  if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$file" | awk '{print $1}')
  elif command -v shasum   >/dev/null 2>&1; then got=$(shasum -a 256 "$file" | awk '{print $1}')
  else warn "no sha256sum/shasum — skipping integrity check"; return 0; fi
  [ "$want" = "$got" ] || die "checksum mismatch for $base (want $want, got $got)"
  say "verified $base (sha256 ok)"
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in arm64|x86_64) : ;; *) die "unsupported macOS arch: $ARCH" ;; esac
    DMG_NAME="DontSpeak-$ARCH.dmg"
    url=$(asset_url "DontSpeak-$ARCH\\.dmg") || true
    [ -n "$url" ] || die "no macOS asset ($DMG_NAME) on the latest release of $REPO"
    sums=$(asset_url "checksums\\.txt")
    say "macOS $ARCH → $url"
    [ "$DRY" = "1" ] && { echo "(dry run) would install DontSpeak.app to /Applications and wire --all"; exit 0; }

    dmg="$TMP/$DMG_NAME"; http_dl "$url" "$dmg"; verify_sha "$dmg" "$sums"
    mnt="$TMP/mnt"; mkdir -p "$mnt"
    hdiutil attach "$dmg" -mountpoint "$mnt" -nobrowse -noverify -noautoopen >/dev/null
    trap 'hdiutil detach "$mnt" >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT
    say "installing DontSpeak.app → /Applications"
    rm -rf "/Applications/DontSpeak.app"
    cp -R "$mnt/DontSpeak.app" /Applications/
    hdiutil detach "$mnt" >/dev/null 2>&1 || true
    trap 'rm -rf "$TMP"' EXIT

    cli="/Applications/DontSpeak.app/Contents/MacOS/dontspeak"
    if [ -x "$cli" ]; then say "wiring clients (MCP + hooks)"; "$cli" wire --all || warn "wire --all reported an issue"
    else warn "no bundled dontspeak CLI in the app — start it and use the Setup Integration action to wire"; fi
    say "launching DontSpeak (first boot downloads the voice models)"
    open -a /Applications/DontSpeak.app || warn "could not auto-launch — open DontSpeak from Applications"
    cat <<'EOF'

Done. Next:
  • On first launch, grant DontSpeak Accessibility + Microphone
    (System Settings › Privacy & Security) — one grant set, all on DontSpeak.app.
  • Start a NEW Claude Code session to load the DontSpeak MCP server.
  • Models download automatically in the background; watch progress in the app.
  • Undo any time:  /Applications/DontSpeak.app/Contents/MacOS/dontspeak wire --all --remove
EOF
    ;;

  Linux)
    case "$ARCH" in x86_64|aarch64) : ;; *) die "unsupported Linux arch: $ARCH" ;; esac
    url=$(asset_url "dontspeak-[0-9][^/]*-$ARCH\\.tar\\.gz") || true
    [ -n "$url" ] || die "no Linux tarball (dontspeak-<ver>-$ARCH.tar.gz) on the latest release of $REPO"
    sums=$(asset_url "checksums\\.txt")
    say "Linux $ARCH → $url"
    [ "$DRY" = "1" ] && { echo "(dry run) would extract the tarball and run its install.sh (wires --all)"; exit 0; }

    tgz="$TMP/$(basename "$url")"; http_dl "$url" "$tgz"; verify_sha "$tgz" "$sums"
    say "extracting"
    tar -xzf "$tgz" -C "$TMP"
    inner=$(find "$TMP" -maxdepth 2 -name install.sh -path '*dontspeak-*' | head -n1)
    [ -n "$inner" ] || die "tarball has no install.sh"
    say "running the bundled installer (copies to ~/.local/bin, wires --all)"
    sh "$inner"
    # Launch the GTK host if a display is available, so the engine boots + models download.
    if [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] && command -v "$HOME/.local/bin/ds-gtk" >/dev/null 2>&1; then
      say "launching DontSpeak"
      ("$HOME/.local/bin/ds-gtk" >/dev/null 2>&1 &) || true
    else
      say "no display detected — launch DontSpeak (ds-gtk) from your desktop to start model download"
    fi
    cat <<'EOF'

Done. Next:
  • Start a NEW Claude Code session to load the DontSpeak MCP server.
  • Grant /dev/uinput access with the sudo step printed above (synthetic keys / Caps-Lock).
  • Undo any time:  ~/.local/bin/dontspeak wire --all --remove
EOF
    ;;

  *)
    die "unsupported OS: $OS (Windows: run install.ps1 instead)"
    ;;
esac
