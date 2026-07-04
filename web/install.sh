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
#   DONTSPEAK_DOWNLOAD_BASE  serve the fixed-name checksums.txt from a mirror; versioned
#                            assets always resolve via the GitHub API regardless
#   DONTSPEAK_DRY_RUN=1    resolve + print the plan, download nothing
#   DONTSPEAK_NO_AUTOSTART=1  Linux: skip enabling start-at-login (macOS N/A — the app
#                             manages its own login item)
#   DONTSPEAK_INSTALL_DIR  Linux: bin dir for the CLI bins + placed uninstaller (default
#                          ~/.local/bin; macOS app always installs to ~/Applications)
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

# Fetch the latest release's asset-URL list ONCE into $ASSET_URLS — both the zip and
# checksums.txt resolve from it. One rate-limited API call per install (anonymous callers
# get 60/hr/IP), and an unreachable API dies HERE with the real cause instead of a
# misleading "no asset on the latest release" later.
ASSET_URLS=""
fetch_assets() {
  ASSET_URLS=$(http_get "$API" \
    | grep -o '"browser_download_url": *"[^"]*"' \
    | sed 's/.*"browser_download_url": *"\([^"]*\)".*/\1/') || true
  [ -n "$ASSET_URLS" ] || die "cannot list release assets from $API (network down, or the anonymous GitHub API rate limit hit)"
}

# Resolve a release asset's download URL by a basename regex, from the fetched list.
# DONTSPEAK_DOWNLOAD_BASE serves FIXED-name assets (checksums.txt) from a mirror; a
# versioned name (it contains a character class) can't be known to a static mirror, so
# those always resolve via the API list.
asset_url() {  # $1 = extended-regex matching the asset filename
  pat="$1"
  if [ -n "${DONTSPEAK_DOWNLOAD_BASE:-}" ]; then
    case "$pat" in
      *\[*) ;;  # versioned — fall through to the API list below
      *)
        lit=$(printf '%s' "$pat" | sed 's/\\//g')   # unescape the ERE to a literal name
        printf '%s/%s\n' "${DONTSPEAK_DOWNLOAD_BASE%/}" "$lit"; return 0 ;;
    esac
  fi
  printf '%s\n' "$ASSET_URLS" | grep -E "/$pat($|\?)" | head -n1
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
# registers), so a one-command script placed on PATH is the idiomatic equivalent.
#
# The heredoc below IS scripts/uninstall.sh, byte-for-byte — the single source of truth
# that apps/linux/uninstall.sh also execs. Never edit it here: edit
# scripts/uninstall.sh and re-embed. packaging_sync.rs (cargo test) fails on any drift.
# BIN must match where the bundled/tarball installer puts binaries — the launch step
# and the placed uninstaller both live there, so an INSTALL_DIR override stays coherent.
BIN="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
UNINSTALLER="$BIN/dontspeak-uninstall"
place_uninstaller() {
  mkdir -p "$(dirname "$UNINSTALLER")"
  cat > "$UNINSTALLER" <<'UNINSTALL'
#!/usr/bin/env bash
# uninstall.sh — THE DontSpeak uninstaller (macOS + Linux): the single source of truth.
#
# Removes the whole install, whichever flow created it:
#   • the app bundle: ~/Applications/DontSpeak.app (macOS) — release (web/install.sh)
#     and dev (apps/macos/bundle.sh) share this ONE per-user layout; DONTSPEAK_APP_DIR
#     overrides it in both
#   • CLI/engine binaries in ~/.local/bin (all flows, and the whole Linux install)
# plus the client wiring (Claude Code hooks + MCP, Claude Desktop MCP, Codex hooks),
# ALL app data / downloaded models / caches / logs / state, the launchers / login
# item, and the placed standalone uninstaller itself.
#
# It runs from two places — the SAME bytes in both, pinned by
# rust/crates/dontspeakd/tests/packaging_sync.rs (cargo test fails on any drift):
#   1. repo checkout:  this file (apps/linux/uninstall.sh execs it)
#   2. installed box:  web/install.sh embeds it verbatim as ~/.local/bin/dontspeak-uninstall
#
# Always resets THIS app's own TCC grants (Accessibility + Microphone + Speech Recognition) on
# macOS, so a reinstall re-prompts cleanly instead of inheriting a stale, pre-selected (and,
# after a signature change, silently non-functional) Privacy & Security entry.
#
# Flags:
#   --udev               Linux: ALSO remove the /dev/uinput udev rule (needs sudo);
#                        your `input` group membership is left intact
#
# Idempotent: every piece is removed best-effort; missing ones are skipped.
set -uo pipefail   # deliberately NOT -e: one missing piece must not abort the teardown

H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"

RM_UDEV=0
for a in "$@"; do
  case "$a" in
    --udev) RM_UDEV=1 ;;
    -h | --help)
      # Header comment only, minus the shebang: from line 2, stop at the first non-# line.
      awk 'NR > 1 && !/^#/ { exit } NR > 1 { sub(/^# ?/, ""); print }' "$0"
      exit 0
      ;;
    *) echo "uninstall: ignoring unknown arg '$a'" >&2 ;;
  esac
done

case "$(uname -s)" in
  Darwin)
    BUNDLE_ID="app.dontspeak.org"

    echo "==> 1. quit the running app + engine + warm helper"
    osascript -e 'quit app "DontSpeak"' 2>/dev/null || true
    sleep 1
    pkill -f "DontSpeak.app/Contents/MacOS/DontSpeak" 2>/dev/null || true
    # -x (exact process name), NOT -f: -f substring-matches EVERY command line, so a
    # bystander like `tail -f ds-helper.log` or an editor with ds-helper.rs open dies too.
    pkill -x ds-helper 2>/dev/null || true

    echo "==> 2. un-wire every client (Claude Code hooks + MCP, Desktop MCP, Codex) before deleting binaries"
    # `wire --all --remove` strips EVERY client's integration: claude_code = hooks
    # (settings.json) + MCP (~/.claude.json); claude_desktop = MCP
    # (claude_desktop_config.json); codex = hooks (~/.codex/config.toml). Prefer the
    # ~/.local/bin CLI, else the CLI bundled in the app (Contents/Helpers).
    APP_DIR="${DONTSPEAK_APP_DIR:-$H/Applications/DontSpeak.app}"
    CLI=""
    for c in \
      "$INSTALL_DIR/dontspeak" \
      "$APP_DIR/Contents/Helpers/dontspeak"; do
      [ -x "$c" ] && { CLI="$c"; break; }
    done
    if [ -n "$CLI" ]; then
      "$CLI" wire --all --remove 2>/dev/null \
        || echo "   (wire --all --remove failed or nothing to remove)"
    else
      echo "   (no dontspeak CLI found — skipping un-wire; strip mcpServers.DontSpeak from"
      echo "    ~/.claude.json and claude_desktop_config.json by hand if they linger)"
    fi

    echo "==> 3. remove the app bundle + installed engine binaries"
    rm -rf "$H/Applications/DontSpeak.app"
    # A bundle installed with the DONTSPEAK_APP_DIR override lives outside the standard
    # per-user layout — honor the same override here or that bundle is never removed.
    [ -n "${DONTSPEAK_APP_DIR:-}" ] && rm -rf "$DONTSPEAK_APP_DIR"
    for b in dontspeak ds-helper; do rm -f "$INSTALL_DIR/$b"; done

    echo "==> 4. remove app data, downloaded models, caches, logs, state"
    # data_dir (config/state) + the legacy ProjectDirs layout; the ONNX model cache; the
    # FluidAudio Core ML / ANE model cache (Kokoro/Parakeet/diarization — its OWN ~900 MB
    # dir, separate from our model_dir); OS app caches.
    rm -rf \
      "$H/Library/Application Support/DontSpeak" \
      "$H/Library/Application Support/org.dontspeak.DontSpeak" \
      "$H/Library/Application Support/FluidAudio" \
      "$H/.cache/fluidaudio" \
      "$H/Library/Caches/DontSpeak" \
      "$H/Library/Caches/app.dontspeak.org" \
      "$H/Library/Caches/org.dontspeak.DontSpeak" \
      "$H/Library/HTTPStorages/app.dontspeak.org" \
      "$H/Library/HTTPStorages/ds-helper" \
      "$H/Library/WebKit/app.dontspeak.org" \
      "$H/Library/Saved Application State/app.dontspeak.org.savedState" \
      "$H/Library/Preferences/app.dontspeak.org.plist" \
      "$H/Library/Logs/DontSpeak"
    # Logs land in ~/Library/Logs/DontSpeak/ (a dir) on current builds, plus a few loose
    # legacy files; crash + diagnostic reports accumulate under their own names.
    rm -f "$H"/Library/Logs/dontspeak*.log* "$H"/Library/Logs/ds-helper.log
    rm -f "$H"/Library/Application\ Support/CrashReporter/ds-*.plist \
      "$H"/Library/Logs/DiagnosticReports/ds-*.ips \
      "$H"/Library/Logs/DiagnosticReports/Retired/ds-*.ips

    echo "==> 4b. remove the DontSpeak hook helpers (install-daemon.sh seeds these into the"
    echo "        SHARED ~/.claude/hooks — so delete only OUR files, never the whole dir)"
    # Mirror install-daemon.sh: it copies/compiles mic-active + capslock (each .swift + the
    # built binary) and copies HOOKS-README.md → README.md into ~/.claude/hooks.
    for f in mic-active mic-active.swift capslock capslock.swift README.md; do
      rm -f "$H/.claude/hooks/$f"
    done

    echo "==> 5. forget the login item (best-effort; SMAppService also reaps it once the app is gone)"
    osascript -e 'tell application "System Events" to delete login item "DontSpeak"' 2>/dev/null || true

    echo "==> 6. reset this app's TCC grants for $BUNDLE_ID so a reinstall re-prompts cleanly"
    # The permissions the app ACTUALLY requests: Accessibility (AXIsProcessTrusted + the
    # CGEventPost dictation inject + the physical-Caps IOHIDManager read, which Accessibility
    # subsumes), Microphone (AVCaptureDevice — STT capture + Test recognition), and Speech
    # Recognition (SFSpeechRecognizer, the System STT engine; prompts on first use). Matches the
    # Info.plist usage keys (Mic + Speech) plus the runtime Accessibility/CGEvent grant.
    tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null || true
    tccutil reset Microphone "$BUNDLE_ID" 2>/dev/null || true
    tccutil reset SpeechRecognition "$BUNDLE_ID" 2>/dev/null || true
    # The dev "DontSpeak Local Dev" self-signed cert (scripts/lib/common.sh
    # ensure_local_sign_identity) is LEFT in place: it's auto-managed by the build and keeps
    # the app signature stable, so the freshly re-granted permission sticks across local
    # rebuilds. TCC is reset above regardless, so no stale grant survives the uninstall.
    ;;

  Linux)
    # XDG roots (see ds-config paths.rs: config/state/cache under the lowercase app id).
    CFG_ROOT="${XDG_CONFIG_HOME:-$H/.config}"
    DATA_ROOT="${XDG_DATA_HOME:-$H/.local/share}"
    CONFIG_DIR="$CFG_ROOT/dontspeak"
    STATE_DIR="${XDG_STATE_HOME:-$H/.local/state}/dontspeak"
    CACHE_DIR="${XDG_CACHE_HOME:-$H/.cache}/dontspeak"
    APPS_DIR="$DATA_ROOT/applications"

    echo "==> 1. stop the running GUI host + warm helper"
    pkill -x ds-gtk 2>/dev/null || true
    # -x, NOT -f: -f substring-matches every command line (see the macOS note above).
    pkill -x ds-helper 2>/dev/null || true

    echo "==> 2. un-wire all client integrations (before deleting the binary)"
    if [ -x "$INSTALL_DIR/dontspeak" ]; then
      "$INSTALL_DIR/dontspeak" wire --all --remove 2>/dev/null \
        || echo "   (wire --all --remove failed or nothing to remove)"
    else
      echo "   (no $INSTALL_DIR/dontspeak — skipping hook removal)"
    fi

    echo "==> 3. remove the installed binaries"
    for b in ds-gtk dontspeak ds-helper; do rm -f "$INSTALL_DIR/$b"; done

    echo "==> 4. remove the .desktop launchers (app menu + autostart) + the app icon"
    rm -f "$APPS_DIR/dontspeak.desktop" \
      "$CFG_ROOT/autostart/dontspeak.desktop" \
      "$DATA_ROOT/icons/hicolor/scalable/apps/dontspeak.svg"
    # Refresh the menu cache so the entry disappears without a re-login (best-effort).
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS_DIR" 2>/dev/null || true
    # The optional --aec PipeWire echo-cancel drop-in (install-gui.sh --aec); a
    # leftover would silently rebuild the AEC graph on every PipeWire restart.
    rm -f "$CFG_ROOT/pipewire/pipewire.conf.d/99-ds-aec.conf"

    echo "==> 4b. remove the DontSpeak hook README (install-daemon.sh seeds it into the"
    echo "        SHARED ~/.claude/hooks — delete only OUR file, never the whole dir)"
    rm -f "$H/.claude/hooks/README.md"

    echo "==> 4c. hand the Caps key back (drop OUR caps:none XKB option, marker-gated)"
    # The engine neutralizes the caps-lock TOGGLE at the keymap level while installed
    # (ds-platform linux/capskey.rs adds `caps:none`; the marker records that WE did).
    # GNOME/KDE options are PERSISTENT and step 5 deletes the marker — so strip the
    # option NOW: an uninstall while the host isn't running (or was SIGKILLed) would
    # otherwise leave Caps Lock dead FOREVER, and a marker-less reinstall treats the
    # leftover option as the user's own and never touches it. Marker-gated exactly like
    # the engine's own release: a `caps:none` the USER set themselves is never stripped.
    CAPS_MARKER="$STATE_DIR/caps-owned"
    if [ -f "$CAPS_MARKER" ]; then
      case "$(cat "$CAPS_MARKER" 2>/dev/null)" in
        gnome)
          # Read-merge-write of the GVariant list, dropping only 'caps:none'.
          RAW="$(gsettings get org.gnome.desktop.input-sources xkb-options 2>/dev/null)" || RAW=""
          if [ -n "$RAW" ]; then
            KEPT="$(printf '%s\n' "$RAW" | grep -o "'[^']*'" | grep -vx "'caps:none'" | paste -sd, -)" || KEPT=""
            gsettings set org.gnome.desktop.input-sources xkb-options "[$KEPT]" 2>/dev/null || true
          fi
          ;;
        kde)
          KREAD=kreadconfig6;   command -v kreadconfig6  >/dev/null 2>&1 || KREAD=kreadconfig5
          KWRITE=kwriteconfig6; command -v kwriteconfig6 >/dev/null 2>&1 || KWRITE=kwriteconfig5
          CUR="$("$KREAD" --file kxkbrc --group Layout --key Options 2>/dev/null)" || CUR=""
          KEPT="$(printf '%s\n' "$CUR" | tr ',' '\n' | grep -vx 'caps:none' | grep -v '^$' | paste -sd, -)" || KEPT=""
          "$KWRITE" --file kxkbrc --group Layout --key Options "$KEPT" 2>/dev/null || true
          dbus-send --session --type=signal /Layouts org.kde.keyboard.reloadConfig 2>/dev/null || true
          ;;
        x11)
          # Volatile (per-X-server) but the session may still be live: reset, then re-add
          # every option except caps:none — setxkbmap has no "remove one" (same dance as
          # the engine's applier). Subshell: `set --` must not clobber OUR script args.
          (
            KEPT="$(setxkbmap -query 2>/dev/null | sed -n 's/^options:[[:space:]]*//p' | tr ',' '\n' | grep -vx 'caps:none' | grep -v '^$')" || KEPT=""
            set -- -option ''
            for o in $KEPT; do set -- "$@" -option "$o"; done
            setxkbmap "$@" 2>/dev/null || true
          )
          ;;
      esac
    fi

    echo "==> 5. remove app data, downloaded models, caches, state"
    # config_dir (settings/speakers/narration spec) + state + the model/onnxruntime cache.
    rm -rf "$CONFIG_DIR" "$STATE_DIR" "$CACHE_DIR"

    if [ "$RM_UDEV" = "1" ]; then
      echo "==> 6. remove the /dev/uinput udev rule (sudo)"
      sudo rm -f /etc/udev/rules.d/99-ds-input.rules 2>/dev/null || true
      sudo udevadm control --reload 2>/dev/null || true
      sudo udevadm trigger 2>/dev/null || true
    else
      echo "==> 6. (udev rule left intact — pass --udev to also remove it; your 'input' group membership is kept)"
    fi
    ;;

  *)
    echo "unsupported OS: $(uname -s) (Windows: uninstall from Settings > Apps)" >&2
    exit 1
    ;;
esac

# Remove the placed standalone uninstaller last (self-delete is safe on unix even while
# it is running; never touches this file in a repo checkout — INSTALL_DIR is ~/.local/bin).
rm -f "$INSTALL_DIR/dontspeak-uninstall"

echo
echo "Done. DontSpeak removed. Reinstall: curl -fsSL https://dontspeak.org/install.sh | sh"
echo "(or from a repo checkout: apps/macos/bundle.sh on macOS, scripts/install.sh + apps/linux/install-gui.sh on Linux)"
UNINSTALL
  chmod +x "$UNINSTALLER"
  say "uninstaller placed: $UNINSTALLER (run it any time to fully remove DontSpeak)"
}

# Everything below runs from main(), invoked on the LAST line — `curl | sh` executes the
# stream as it arrives, so a connection drop mid-transfer would otherwise run every
# complete line received (potentially the destructive clean step) and silently stop. A
# truncated stream now can't execute anything: the `main "$@"` call is the final line.
main() {

TMP=$(mktemp -d)
STAGED=""
cleanup() { rm -rf "$TMP"; [ -n "$STAGED" ] && rm -rf "$STAGED"; :; }
# Signals too: a Ctrl-C mid-download must not leave the mktemp dir behind (POSIX sh
# doesn't run the EXIT trap on an unhandled signal) — and a signal trap that doesn't
# exit would RESUME the script after the interrupted command, mid-install. Stop too.
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM HUP

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    # A Rosetta-translated shell reports x86_64 on an Apple-Silicon machine — installing
    # the Intel build there. Ask the kernel whether this process is translated.
    if [ "$ARCH" = "x86_64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
      ARCH=arm64
    fi
    case "$ARCH" in arm64|x86_64) : ;; *) die "unsupported macOS arch: $ARCH" ;; esac
    # Release-asset arch token is uname-style everywhere: macOS arm64 → aarch64.
    case "$ARCH" in arm64) AARCH=aarch64 ;; *) AARCH="$ARCH" ;; esac
    ZIP_NAME="dontspeak-<ver>-macos-$AARCH.app.zip"
    fetch_assets
    url=$(asset_url "dontspeak-[0-9][^/]*-macos-$AARCH\\.app\\.zip")
    [ -n "$url" ] || die "no macOS asset ($ZIP_NAME) on the latest release of $REPO"
    sums=$(asset_url "checksums\\.txt")
    say "macOS $ARCH → $url"
    [ "$DRY" = "1" ] && { echo "(dry run) would unzip DontSpeak.app into ~/Applications and wire --all"; exit 0; }

    zip="$TMP/$(basename "$url")"; http_dl "$url" "$zip"; verify_sha "$zip" "$sums"
    # The ONE macOS install location, shared with the dev flow (apps/macos/bundle.sh):
    # per-user, so no admin account and no sudo — everything else about an install
    # (wiring, data, models, login item, TCC) is per-user anyway.
    APP="$HOME/Applications/DontSpeak.app"
    say "installing DontSpeak.app → $HOME/Applications"
    out="$TMP/app"; mkdir -p "$out"
    ditto -x -k "$zip" "$out"          # the zip holds DontSpeak.app/ at its root
    [ -d "$out/DontSpeak.app" ] || die "unexpected archive layout (no DontSpeak.app)"
    # Stage the new bundle onto the TARGET volume FIRST — a failed copy (disk full) must
    # abort BEFORE anything is deleted, or a broken install would strip the machine of
    # its working copy. The trap cleans $STAGED on any abort; the final rename below is
    # near-instant.
    mkdir -p "$HOME/Applications"
    STAGED="$HOME/Applications/.DontSpeak.app.staged.$$"
    rm -rf "$STAGED"
    cp -R "$out/DontSpeak.app" "$STAGED" 2>/dev/null \
      || die "cannot write the new app into $HOME/Applications (disk full?)"
    # CLEAN install, not an upgrade-in-place: quit any running instance (app + engine +
    # warm helper), then replace the previous bundle — release and dev installs share
    # this one layout, so there is no other location to probe. Also drop dev/CLI
    # installs in ~/.local/bin: on unix the wire step PREFERS a ~/.local/bin binary over
    # the bundled one (sibling_bin), so a stale copy there would get wired (or, once
    # deleted, leave hooks pointing at a dead path). There is never a "previous version
    # to preserve": user data/models are untouched, everything executable is replaced
    # fresh.
    if [ -d "$APP" ]; then
      osascript -e 'quit app "DontSpeak"' 2>/dev/null || true
      sleep 1
      pkill -f "DontSpeak.app/Contents/MacOS/DontSpeak" 2>/dev/null || true
      # -x (exact name), NOT -f: -f substring-matches every command line, so a bystander
      # like `tail -f ds-helper.log` would be killed too.
      pkill -x ds-helper 2>/dev/null || true
    fi
    rm -rf "$APP"
    for b in dontspeak ds-helper ds-gtk; do rm -f "$BIN/$b"; done
    mv "$STAGED" "$APP"
    STAGED=""

    cli="$APP/Contents/Helpers/dontspeak"
    if [ -x "$cli" ]; then say "wiring clients (MCP + hooks)"; "$cli" wire --all || warn "wire --all reported an issue"
    else warn "no bundled dontspeak CLI in the app — start it and use the Setup Integration action to wire"; fi
    place_uninstaller
    say "launching DontSpeak (first boot downloads the voice models)"
    open -a "$APP" || warn "could not auto-launch — open DontSpeak from ~/Applications"
    cat <<EOF

Done. Next:
  • On first launch, grant DontSpeak Accessibility + Microphone
    (System Settings › Privacy & Security) — one grant set, all on DontSpeak.app.
  • Start a NEW Claude Code session to load the DontSpeak MCP server.
  • Models download automatically in the background; watch progress in the app.
  • Uninstall any time:  $UNINSTALLER
    (or just unwire:  ~/Applications/DontSpeak.app/Contents/Helpers/dontspeak wire --all --remove)
EOF
    ;;

  Linux)
    case "$ARCH" in x86_64|aarch64) : ;; *) die "unsupported Linux arch: $ARCH" ;; esac
    fetch_assets
    url=$(asset_url "dontspeak-[0-9][^/]*-linux-$ARCH\\.tar\\.gz")
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
    if [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] && command -v "$BIN/ds-gtk" >/dev/null 2>&1; then
      say "launching DontSpeak"
      ("$BIN/ds-gtk" >/dev/null 2>&1 &) || true
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

}

main "$@"
