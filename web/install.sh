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
# Uses the GitHub API so the versioned asset names resolve without knowing the version.
# DONTSPEAK_DOWNLOAD_BASE, if set, short-circuits to <base>/<name> — now only useful for
# the fixed-name checksums.txt (every binary asset embeds the version).
asset_url() {  # $1 = extended-regex matching the asset filename
  pat="$1"
  if [ -n "${DONTSPEAK_DOWNLOAD_BASE:-}" ]; then
    case "$pat" in *\[*)
      # A character class means a VERSIONED name (the Linux tarball) — un-escaping it
      # would build a garbage URL. Fail loudly instead of 404ing on nonsense.
      die "DONTSPEAK_DOWNLOAD_BASE can't resolve the versioned asset '$pat' — unset it" ;;
    esac
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

# Drop a STANDALONE uninstaller next to the CLI. macOS/Linux have no OS-level "installed apps"
# registry for drag/tarball installs (unlike the Windows Settings>Apps entry install.ps1
# registers), so a one-command script placed on PATH is the idiomatic equivalent. It reuses the
# app's own `dontspeak wire --all --remove`, then removes the app, launchers, autostart, and all
# data — mirroring scripts/uninstall.sh (macOS) / apps/linux/uninstall.sh (Linux). Self-deletes.
UNINSTALLER="$HOME/.local/bin/dontspeak-uninstall"
place_uninstaller() {
  mkdir -p "$(dirname "$UNINSTALLER")"
  cat > "$UNINSTALLER" <<'UNINSTALL'
#!/usr/bin/env bash
# DontSpeak uninstaller — unwires every client, removes the app + launchers + autostart + all
# data. Placed by the one-command installer. Idempotent; missing pieces are skipped. Self-deletes.
set -uo pipefail
H="$HOME"
case "$(uname -s)" in
  Darwin)
    APP="/Applications/DontSpeak.app"; CLI="$APP/Contents/MacOS/dontspeak"
    osascript -e 'quit app "DontSpeak"' 2>/dev/null || true; sleep 1
    pkill -f "DontSpeak.app/Contents/MacOS/DontSpeak" 2>/dev/null || true
    pkill -f ds-helper 2>/dev/null || true
    [ -x "$CLI" ] && "$CLI" wire --all --remove 2>/dev/null || true
    rm -rf "$APP" \
      "$H/Library/Application Support/DontSpeak" \
      "$H/Library/Application Support/org.dontspeak.DontSpeak" \
      "$H/Library/Application Support/FluidAudio" "$H/.cache/fluidaudio" \
      "$H/Library/Caches/DontSpeak" "$H/Library/Caches/app.dontspeak.org" \
      "$H/Library/Caches/org.dontspeak.DontSpeak" \
      "$H/Library/HTTPStorages/app.dontspeak.org" \
      "$H/Library/Preferences/app.dontspeak.org.plist" "$H/Library/Logs/DontSpeak"
    rm -f "$H"/Library/Logs/dontspeak*.log* "$H"/Library/Logs/ds-helper.log
    osascript -e 'tell application "System Events" to delete login item "DontSpeak"' 2>/dev/null || true
    ;;
  Linux)
    BIN="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
    APPS="${XDG_DATA_HOME:-$H/.local/share}/applications"
    pkill -x ds-gtk 2>/dev/null || true; pkill -f ds-helper 2>/dev/null || true
    [ -x "$BIN/dontspeak" ] && "$BIN/dontspeak" wire --all --remove 2>/dev/null || true
    for b in ds-gtk dontspeak ds-helper; do rm -f "$BIN/$b"; done
    rm -f "$APPS/dontspeak.desktop" "${XDG_CONFIG_HOME:-$H/.config}/autostart/dontspeak.desktop"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" 2>/dev/null || true
    rm -rf "${XDG_CONFIG_HOME:-$H/.config}/dontspeak" \
           "${XDG_STATE_HOME:-$H/.local/state}/dontspeak" \
           "${XDG_CACHE_HOME:-$H/.cache}/dontspeak"
    ;;
esac
echo "DontSpeak removed."
rm -f "$0"
UNINSTALL
  chmod +x "$UNINSTALLER"
  say "uninstaller placed: $UNINSTALLER (run it any time to fully remove DontSpeak)"
}

TMP=$(mktemp -d)
# Signals too: a Ctrl-C mid-download must not leave the mktemp dir behind (POSIX sh
# doesn't run the EXIT trap on an unhandled signal).
trap 'rm -rf "$TMP"' EXIT INT TERM HUP

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    case "$ARCH" in arm64|x86_64) : ;; *) die "unsupported macOS arch: $ARCH" ;; esac
    # Release-asset arch token is uname-style everywhere: macOS arm64 → aarch64.
    case "$ARCH" in arm64) AARCH=aarch64 ;; *) AARCH="$ARCH" ;; esac
    ZIP_NAME="dontspeak-<ver>-macos-$AARCH.app.zip"
    url=$(asset_url "dontspeak-[0-9][^/]*-macos-$AARCH\\.app\\.zip") || true
    [ -n "$url" ] || die "no macOS asset ($ZIP_NAME) on the latest release of $REPO"
    sums=$(asset_url "checksums\\.txt")
    say "macOS $ARCH → $url"
    [ "$DRY" = "1" ] && { echo "(dry run) would unzip DontSpeak.app into /Applications and wire --all"; exit 0; }

    zip="$TMP/$(basename "$url")"; http_dl "$url" "$zip"; verify_sha "$zip" "$sums"
    say "installing DontSpeak.app → /Applications"
    out="$TMP/app"; mkdir -p "$out"
    ditto -x -k "$zip" "$out"          # the zip holds DontSpeak.app/ at its root
    [ -d "$out/DontSpeak.app" ] || die "unexpected archive layout (no DontSpeak.app)"
    # Re-run/upgrade path: quit a running instance (app + engine + warm helper) before
    # replacing the bundle — same sequence as scripts/uninstall.sh. Swapping the bundle
    # under a live process leaves the old version running against deleted files.
    if [ -d "/Applications/DontSpeak.app" ]; then
      osascript -e 'quit app "DontSpeak"' 2>/dev/null || true
      sleep 1
      pkill -f "DontSpeak.app/Contents/MacOS/DontSpeak" 2>/dev/null || true
      pkill -f "ds-helper" 2>/dev/null || true
    fi
    rm -rf "/Applications/DontSpeak.app"
    cp -R "$out/DontSpeak.app" /Applications/

    cli="/Applications/DontSpeak.app/Contents/MacOS/dontspeak"
    if [ -x "$cli" ]; then say "wiring clients (MCP + hooks)"; "$cli" wire --all || warn "wire --all reported an issue"
    else warn "no bundled dontspeak CLI in the app — start it and use the Setup Integration action to wire"; fi
    place_uninstaller
    say "launching DontSpeak (first boot downloads the voice models)"
    open -a /Applications/DontSpeak.app || warn "could not auto-launch — open DontSpeak from Applications"
    cat <<EOF

Done. Next:
  • On first launch, grant DontSpeak Accessibility + Microphone
    (System Settings › Privacy & Security) — one grant set, all on DontSpeak.app.
  • Start a NEW Claude Code session to load the DontSpeak MCP server.
  • Models download automatically in the background; watch progress in the app.
  • Uninstall any time:  $UNINSTALLER
    (or just unwire:  /Applications/DontSpeak.app/Contents/MacOS/dontspeak wire --all --remove)
EOF
    ;;

  Linux)
    case "$ARCH" in x86_64|aarch64) : ;; *) die "unsupported Linux arch: $ARCH" ;; esac
    url=$(asset_url "dontspeak-[0-9][^/]*-linux-$ARCH\\.tar\\.gz") || true
    [ -n "$url" ] || die "no Linux tarball (dontspeak-<ver>-linux-$ARCH.tar.gz) on the latest release of $REPO"
    sums=$(asset_url "checksums\\.txt")
    say "Linux $ARCH → $url"
    [ "$DRY" = "1" ] && { echo "(dry run) would extract the tarball and run its install.sh (wires --all)"; exit 0; }

    tgz="$TMP/$(basename "$url")"; http_dl "$url" "$tgz"; verify_sha "$tgz" "$sums"
    say "extracting"
    tar -xzf "$tgz" -C "$TMP"
    inner=$(find "$TMP" -maxdepth 2 -name install.sh -path '*dontspeak-*' | head -n1)
    [ -n "$inner" ] || die "tarball has no install.sh"
    say "running the bundled installer (copies to ~/.local/bin, wires --all)"
    # The bundled installer is bash (pipefail, BASH_SOURCE) — running it with `sh`
    # breaks on distros where sh is dash (Debian/Ubuntu).
    command -v bash >/dev/null 2>&1 || die "the bundled installer needs bash on PATH"
    bash "$inner"
    # Start-at-login: DontSpeak is a resident tray/engine host, so enable autostart by default
    # (parity with the Windows installer's Run key and the retired Inno "start at login" default).
    # The bundled installer wrote the launcher into the XDG applications dir; XDG autostart is
    # just a copy of that .desktop under ~/.config/autostart. Opt out with DONTSPEAK_NO_AUTOSTART=1.
    if [ "${DONTSPEAK_NO_AUTOSTART:-0}" != "1" ]; then
      desktop_src="${XDG_DATA_HOME:-$HOME/.local/share}/applications/dontspeak.desktop"
      autostart_dir="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
      if [ -f "$desktop_src" ]; then
        mkdir -p "$autostart_dir"
        cp "$desktop_src" "$autostart_dir/dontspeak.desktop"
        say "enabled start-at-login ($autostart_dir/dontspeak.desktop; DONTSPEAK_NO_AUTOSTART=1 to skip)"
      fi
    fi
    # Launch the GTK host if a display is available, so the engine boots + models download.
    if [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] && command -v "$HOME/.local/bin/ds-gtk" >/dev/null 2>&1; then
      say "launching DontSpeak"
      ("$HOME/.local/bin/ds-gtk" >/dev/null 2>&1 &) || true
    else
      say "no display detected — launch DontSpeak (ds-gtk) from your desktop to start model download"
    fi
    place_uninstaller
    cat <<EOF

Done. Next:
  • Start a NEW Claude Code session to load the DontSpeak MCP server.
  • Grant /dev/uinput access with the sudo step printed above (synthetic keys / Caps-Lock).
  • Uninstall any time:  $UNINSTALLER
    (or just unwire:  ~/.local/bin/dontspeak wire --all --remove)
EOF
    ;;

  *)
    die "unsupported OS: $OS (Windows: run install.ps1 instead)"
    ;;
esac
