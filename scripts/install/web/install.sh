#!/bin/sh
# DontSpeak one-command installer -- macOS + Linux.
#
#   curl -fsSL https://github.com/delllusional/DontSpeak/releases/latest/download/install.sh | sh
#
# Downloads the prebuilt app for this OS/arch from the latest GitHub Release,
# verifies its SHA-256, installs it, wires the MCP server + voice hooks into every
# detected client (`dontspeak wire --reconcile`), and launches the app once so the voice
# models download themselves on first boot. No compiler required.
#
# Programmers who want a from-source build should instead clone the repo and run
# scripts/install/local/install.sh (this script never builds).
#
# Env overrides:
#   DONTSPEAK_REPO         owner/repo to fetch releases from (default delllusional/DontSpeak)
#   DONTSPEAK_DOWNLOAD_BASE  serve the fixed-name checksums.txt from a mirror; versioned
#                            assets always resolve via the GitHub API regardless
#   DONTSPEAK_ARCHIVE      install an explicit local app zip/tarball instead of downloading
#                          latest (development path; the archive must match this machine)
#   DONTSPEAK_DRY_RUN=1    resolve + print the plan, download nothing
#   DONTSPEAK_NO_AUTOSTART=1  Linux: skip enabling start-at-login (macOS N/A -- the app
#                             manages its own login item)
#   DONTSPEAK_INSTALL_DIR  bin dir for CLI launchers + placed uninstaller (default
#                          ~/.local/bin; macOS app always installs to ~/Applications)
#   DONTSPEAK_INSTALL_LOCK_WAIT  seconds to wait for a concurrent installer before failing
#                                (default 600)
set -eu

REPO="${DONTSPEAK_REPO:-delllusional/DontSpeak}"
API="https://api.github.com/repos/$REPO/releases/latest"
DRY="${DONTSPEAK_DRY_RUN:-0}"
ARCHIVE="${DONTSPEAK_ARCHIVE:-}"

say()  { printf '==> %s\n' "$*"; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
die()  { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

if [ -n "$ARCHIVE" ]; then
  [ -f "$ARCHIVE" ] || die "local archive not found: $ARCHIVE"
  archive_dir=$(cd "$(dirname "$ARCHIVE")" && pwd) \
    || die "cannot resolve local archive: $ARCHIVE"
  ARCHIVE="$archive_dir/$(basename "$ARCHIVE")"
fi

# -- HTTP: prefer curl, fall back to wget -------------------------------------
if [ -z "$ARCHIVE" ]; then
  if command -v curl >/dev/null 2>&1; then
    http_get()  { curl -fsSL "$1"; }                 # to stdout
    http_dl()   { curl -fsSL -o "$2" "$1"; }         # url file
  elif command -v wget >/dev/null 2>&1; then
    http_get()  { wget -qO- "$1"; }
    http_dl()   { wget -qO "$2" "$1"; }
  else
    die "need curl or wget on PATH"
  fi
fi

# Fetch the latest release's asset-URL list ONCE into $ASSET_URLS -- both the zip and
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
      *\[*) ;;  # versioned -- fall through to the API list below
      *)
        lit=$(printf '%s' "$pat" | sed 's/\\//g')   # unescape the ERE to a literal name
        printf '%s/%s\n' "${DONTSPEAK_DOWNLOAD_BASE%/}" "$lit"; return 0 ;;
    esac
  fi
  printf '%s\n' "$ASSET_URLS" | grep -E "/$pat($|\?)" | head -n1
}

# Verify $1 against the sha256 line for its basename in the checksums.txt at $2 (a URL).
verify_sha() {  # $1 = file, $2 = checksums url
  file="$1"; sums_url="$2"; base=$(basename "$file")
  [ -n "$sums_url" ] || die "release is missing checksums.txt"
  sums=$(http_get "$sums_url") || die "cannot download checksums.txt"
  [ -n "$sums" ] || die "checksums.txt is empty"
  # A release manifest may be uploaded from Windows. Strip CR before the
  # end-anchored filename match so CRLF and LF manifests verify identically.
  sums=$(printf '%s\n' "$sums" | tr -d '\015')
  # Match either sha256sum format: text "<hash>  name" or binary "<hash> *name" -- i.e. the
  # separator right before the basename is a space or a '*'.
  want=$(printf '%s\n' "$sums" | grep -E "[ *]$base\$" | awk '{print $1}' | head -n1)
  [ -n "$want" ] || die "$base is not listed in checksums.txt"
  if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$file" | awk '{print $1}')
  elif command -v shasum   >/dev/null 2>&1; then got=$(shasum -a 256 "$file" | awk '{print $1}')
  else die "need sha256sum or shasum to verify $base"; fi
  [ "$want" = "$got" ] || die "checksum mismatch for $base (want $want, got $got)"
  say "verified $base (sha256 ok)"
}

# Each package carries its canonical uninstaller as a real payload file. macOS copies
# that resource beside the CLI; Linux's bundled installer performs the equivalent copy.
BIN="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
UNINSTALLER="$BIN/dontspeak-uninstall"
place_uninstaller() {
  source="$1"
  [ -f "$source" ] || die "package is missing its canonical uninstaller: $source"
  mkdir -p "$(dirname "$UNINSTALLER")"
  cp "$source" "$UNINSTALLER"
  chmod +x "$UNINSTALLER"
  say "uninstaller placed: $UNINSTALLER (run it any time to fully remove DontSpeak)"
}

# -- BEGIN destination lock ---------------------------------------------------
# Serializes the destructive finalization (recheck -> stop -> replace -> wire) of ONE install
# destination across processes; downloads and staging stay outside it (unique per-invocation
# names, safe in parallel). Issue #198.
#
# COPY: byte-identical in scripts/install/web/install.sh and apps/linux/tarball-install.sh
# (packaging_sync.rs pins the two equal). Both ship standalone -- one through `curl | sh`, one
# inside the tarball -- so neither can source a shared file. POSIX sh only: install.sh runs
# under /bin/sh (dash on Debian/Ubuntu).
#
# mkdir is the atomic primitive because no OS-released shell lock exists on both platforms:
# flock(1) is absent on macOS, shlock(1) on Linux. (The Windows installer gets an OS-released
# one from an exclusively-shared file handle and needs none of the rules below.) A mkdir lock
# outlives its owner, so it is breakable two ways:
#   - the owner was recorded on THIS host and no longer answers `kill -0` -> break now;
#   - otherwise -- a foreign host token (a shared $HOME can hold another machine's lock, where
#     a pid means nothing), an unreadable owner file, or an owner that still answers -> break
#     only once the lock is DS_LOCK_STALE_MIN old.
# The age cap also covers an owner that answers because its pid was REUSED by an unrelated
# process, which liveness alone cannot distinguish from a live owner; with no cap such a lock
# would block installs until that process happened to exit. Real finalization is seconds, so a
# lock this old means its owner is wedged, and breaking it is no worse than the unserialized
# behavior this replaces.
#
# Every rm below ends in `|| :` because under `set -e` a failing command that ends an if-body,
# a case-branch or a trap handler exits the shell: a lock another uid owns (rm -> EPERM) would
# otherwise kill the installer outright instead of waiting out its deadline, and a failing
# release would skip the rest of cleanup and rewrite a successful install's exit status.
DS_LOCK_DIR=""
DS_LOCK_OWNED=0
DS_LOCK_STALE_MIN=60

ds_lock_path() {  # $1 = destination -> sibling ".<name>.ds-install.lock"
  printf '%s/.%s.ds-install.lock' "$(dirname "$1")" "$(basename "$1")"
}

# 0 = the lock may be broken.
ds_lock_stale() {
  ds_lock_owner="$(cat "$DS_LOCK_DIR/owner" 2>/dev/null)" || ds_lock_owner=""
  ds_lock_pid="${ds_lock_owner%% *}"
  ds_lock_host="${ds_lock_owner#* }"
  case "$ds_lock_pid" in
    ''|*[!0-9]*) : ;;
    *)
      if [ "$ds_lock_host" = "$(uname -n)" ] && ! kill -0 "$ds_lock_pid" 2>/dev/null; then
        return 0
      fi
      ;;
  esac
  # find exits 0 either way and prints the path only when older, so age is decided by the
  # output, not the status.
  [ -n "$(find "$DS_LOCK_DIR" -maxdepth 0 -mmin "+$DS_LOCK_STALE_MIN" 2>/dev/null)" ]
}

# Breaking is itself serialized with the same atomic mkdir: two waiters that both saw one
# breakable lock would otherwise race `rm -rf` against each other's fresh `mkdir`. The slot is
# force-reclaimed after a minute -- its own critical section is milliseconds, so an older slot
# is unambiguously abandoned.
ds_lock_break() {
  ds_lock_breaker="$DS_LOCK_DIR.breaker"
  if ! mkdir "$ds_lock_breaker" 2>/dev/null; then
    if [ -n "$(find "$ds_lock_breaker" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      rm -rf "$ds_lock_breaker" || :
    fi
    return 0
  fi
  # Recheck inside the slot: another breaker may already have removed this lock and a third
  # process taken a fresh one. Residual, age path only: a lock whose owner is still alive can
  # be removed if that owner releases and a third process re-acquires between this recheck and
  # the rm -- a sub-millisecond window that first requires the lock to be an hour old.
  if ds_lock_stale; then rm -rf "$DS_LOCK_DIR" || :; fi
  rmdir "$ds_lock_breaker" 2>/dev/null || :
}

# $1 = destination path. Blocks until owned, then records "<pid> <host>". DS_LOCK_DIR is set
# before ownership so the timeout message can name the lock; release is gated on DS_LOCK_OWNED.
ds_lock_acquire() {
  DS_LOCK_DIR="$(ds_lock_path "$1")"
  mkdir -p "$(dirname "$DS_LOCK_DIR")"
  ds_lock_waited=0
  ds_lock_notified=0
  while :; do
    if mkdir "$DS_LOCK_DIR" 2>/dev/null; then
      # Claim first, describe second: killed in between, the dir is still ours to remove
      # (ds_lock_release reads a missing owner file as our own crash residue).
      DS_LOCK_OWNED=1
      printf '%s %s\n' "$$" "$(uname -n)" > "$DS_LOCK_DIR/owner"
      return 0
    fi
    # Only "already taken" means wait. An unwritable parent fails here too, and polling that
    # for the whole deadline would report a concurrent installer that does not exist.
    [ -d "$DS_LOCK_DIR" ] || {
      printf 'ERROR: %s\n' "cannot create the install lock $DS_LOCK_DIR" >&2
      exit 1
    }
    if ds_lock_stale; then ds_lock_break; fi
    if [ "$ds_lock_notified" = 0 ] && [ "$ds_lock_waited" -ge 2 ]; then
      printf '==> %s\n' "waiting for another DontSpeak installer to finish"
      ds_lock_notified=1
    fi
    if [ "$ds_lock_waited" -ge "${DONTSPEAK_INSTALL_LOCK_WAIT:-600}" ]; then
      printf 'ERROR: %s\n' "another DontSpeak installer is still finalizing $1 (lock: $DS_LOCK_DIR)" >&2
      exit 1
    fi
    sleep 1
    ds_lock_waited=$((ds_lock_waited + 1))
  done
}

# Deleting the dir IS the release: a mkdir lock that outlives its owner can only be re-taken
# by force-breaking it. (The Windows installer is the mirror image -- there the file must stay,
# because waiters hold an open handle to it.) Owner-gated: a lock broken out from under us and
# re-taken belongs to someone else.
ds_lock_release() {
  [ "$DS_LOCK_OWNED" = 1 ] || return 0
  DS_LOCK_OWNED=0
  ds_lock_owner="$(cat "$DS_LOCK_DIR/owner" 2>/dev/null)" || ds_lock_owner=""
  case "$ds_lock_owner" in ''|"$$ "*) rm -rf "$DS_LOCK_DIR" || : ;; esac
  DS_LOCK_DIR=""
}
# -- END destination lock -----------------------------------------------------

# Everything below runs from main(), invoked on the LAST line -- `curl | sh` executes the
# stream as it arrives, so a connection drop mid-transfer would otherwise run every
# complete line received (potentially the destructive clean step) and silently stop. A
# truncated stream now can't execute anything: the `main "$@"` call is the final line.
main() {

TMP=$(mktemp -d)
STAGED=""
# Release first: hand the shared destination to a waiting installer before spending time on
# this process's own per-invocation cleanup (a staged .app copy is not small).
cleanup() { ds_lock_release; rm -rf "$TMP"; [ -n "$STAGED" ] && rm -rf "$STAGED"; :; }
# Signals too: a Ctrl-C mid-download must not leave the mktemp dir behind (POSIX sh
# doesn't run the EXIT trap on an unhandled signal) -- and a signal trap that doesn't
# exit would RESUME the script after the interrupted command, mid-install. Stop too.
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM HUP

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
  Darwin)
    # A Rosetta-translated shell reports x86_64 on an Apple-Silicon machine -- installing
    # the Intel build there. Ask the kernel whether this process is translated.
    if [ "$ARCH" = "x86_64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = "1" ]; then
      ARCH=arm64
    fi
    case "$ARCH" in arm64|x86_64) : ;; *) die "unsupported macOS arch: $ARCH" ;; esac
    # Release-asset arch token is uname-style everywhere: macOS arm64 -> aarch64.
    case "$ARCH" in arm64) AARCH=aarch64 ;; *) AARCH="$ARCH" ;; esac
    ZIP_NAME="dontspeak-<ver>-macos-$AARCH.app.zip"
    if [ -n "$ARCHIVE" ]; then
      archive_name=$(basename "$ARCHIVE")
      case "$archive_name" in
        dontspeak-?*-macos-"$AARCH".app.zip) : ;;
        *) die "archive '$archive_name' does not match this machine (expected $ZIP_NAME)" ;;
      esac
      zip="$ARCHIVE"
      source_label="$ARCHIVE"
    else
      fetch_assets
      url=$(asset_url "dontspeak-[0-9][^/]*-macos-$AARCH\\.app\\.zip")
      [ -n "$url" ] || die "no macOS asset ($ZIP_NAME) on the latest release of $REPO"
      sums=$(asset_url "checksums\\.txt")
      zip="$TMP/$(basename "$url")"
      source_label="$url"
    fi
    say "macOS $ARCH -> $source_label"
    [ "$DRY" = "1" ] && { echo "(dry run) would unzip DontSpeak.app into ~/Applications and wire --reconcile"; exit 0; }

    if [ -n "$ARCHIVE" ]; then
      say "using local archive"
    else
      http_dl "$url" "$zip"
      verify_sha "$zip" "$sums"
    fi
    # The ONE macOS install location, shared with the dev flow (apps/macos/bundle.sh):
    # per-user, so no admin account and no sudo -- everything else about an install
    # (wiring, data, models, login item, TCC) is per-user anyway.
    APP="$HOME/Applications/DontSpeak.app"
    say "installing DontSpeak.app -> $HOME/Applications"
    out="$TMP/app"; mkdir -p "$out"
    ditto -x -k "$zip" "$out"          # the zip holds DontSpeak.app/ at its root
    [ -d "$out/DontSpeak.app" ] || die "unexpected archive layout (no DontSpeak.app)"
    [ -f "$out/DontSpeak.app/Contents/Resources/uninstall.sh" ] \
      || die "incomplete archive (no canonical uninstall.sh payload)"
    # Stage the new bundle onto the TARGET volume FIRST -- a failed copy (disk full) must
    # abort BEFORE anything is deleted, or a broken install would strip the machine of
    # its working copy. The trap cleans $STAGED on any abort; the final rename below is
    # near-instant.
    mkdir -p "$HOME/Applications"
    STAGED="$HOME/Applications/.DontSpeak.app.staged.$$"
    rm -rf "$STAGED"
    cp -R "$out/DontSpeak.app" "$STAGED" 2>/dev/null \
      || die "cannot write the new app into $HOME/Applications (disk full?)"
    # Everything below replaces the ONE shared destination -- serialize it against a second
    # installer (#198). Staging above is per-invocation, so it needs no lock. The EXIT/signal
    # traps armed above already cover the release.
    ds_lock_acquire "$APP"
    # CLEAN install, not an upgrade-in-place: quit any running instance (app + engine +
    # warm helper), then replace the previous bundle -- release and dev installs share
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
    if [ -x "$cli" ]; then
      mkdir -p "$BIN"
      ln -s "$cli" "$BIN/dontspeak"
      say "launcher placed: $BIN/dontspeak"
      say "wiring clients (MCP + hooks)"
      "$cli" wire --reconcile || warn "wire --reconcile reported an issue"
    else warn "no bundled dontspeak CLI in the app -- start it; clients are wired automatically at launch"; fi
    place_uninstaller "$APP/Contents/Resources/uninstall.sh"
    say "launching DontSpeak (first boot downloads the voice models)"
    open -a "$APP" || warn "could not auto-launch -- open DontSpeak from ~/Applications"
    cat <<EOF

Done. Next:
  - On first launch, grant DontSpeak Accessibility + Microphone
    (System Settings > Privacy & Security) -- one grant set, all on DontSpeak.app.
  - Start a NEW session in each installed client to load its DontSpeak MCP server.
  - Models download automatically in the background; watch progress in the app.
  - Uninstall any time:  $UNINSTALLER
    (or just unwire:  ~/Applications/DontSpeak.app/Contents/Helpers/dontspeak wire --all --remove)
EOF
    ;;

  Linux)
    case "$ARCH" in x86_64|aarch64) : ;; *) die "unsupported Linux arch: $ARCH" ;; esac
    TGZ_NAME="dontspeak-<ver>-linux-$ARCH.tar.gz"
    if [ -n "$ARCHIVE" ]; then
      archive_name=$(basename "$ARCHIVE")
      case "$archive_name" in
        dontspeak-?*-linux-"$ARCH".tar.gz) : ;;
        *) die "archive '$archive_name' does not match this machine (expected $TGZ_NAME)" ;;
      esac
      tgz="$ARCHIVE"
      source_label="$ARCHIVE"
    else
      fetch_assets
      url=$(asset_url "dontspeak-[0-9][^/]*-linux-$ARCH\\.tar\\.gz")
      [ -n "$url" ] || die "no Linux tarball ($TGZ_NAME) on the latest release of $REPO"
      sums=$(asset_url "checksums\\.txt")
      tgz="$TMP/$(basename "$url")"
      source_label="$url"
    fi
    say "Linux $ARCH -> $source_label"
    [ "$DRY" = "1" ] && { echo "(dry run) would extract the tarball and run its install.sh (wires --all)"; exit 0; }

    if [ -n "$ARCHIVE" ]; then
      say "using local archive"
    else
      http_dl "$url" "$tgz"
      verify_sha "$tgz" "$sums"
    fi
    say "extracting"
    tar -xzf "$tgz" -C "$TMP"
    inner=$(find "$TMP" -maxdepth 2 -name install.sh -path '*dontspeak-*' | head -n1)
    [ -n "$inner" ] || die "tarball has no install.sh"
    say "running the bundled installer (copies to ~/.local/bin, wires --all)"
    # The bundled installer is bash (pipefail, BASH_SOURCE) -- running it with `sh`
    # breaks on distros where sh is dash (Debian/Ubuntu).
    command -v bash >/dev/null 2>&1 || die "the bundled installer needs bash on PATH"
    bash "$inner"
    [ -x "$UNINSTALLER" ] || die "bundled installer did not place $UNINSTALLER"
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
      say "no display detected -- launch DontSpeak (ds-gtk) from your desktop to start model download"
    fi
    cat <<EOF

Done. Next:
  - Start a NEW session in each installed client to load its DontSpeak MCP server.
  - Grant /dev/uinput access with the sudo step printed above (synthetic keys / Caps-Lock).
  - Uninstall any time:  $UNINSTALLER
    (or just unwire:  ~/.local/bin/dontspeak wire --all --remove)
EOF
    ;;

  *)
    die "unsupported OS: $OS (Windows: run install.ps1 instead)"
    ;;
esac

}

main "$@"
