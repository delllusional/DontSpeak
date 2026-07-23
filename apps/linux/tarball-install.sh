#!/usr/bin/env bash
# tarball-install.sh — the installer that ships INSIDE the Linux portable tarball as
# ./install.sh (package.sh copies this file in verbatim; single source, no embedded
# heredoc copy to drift). Copies binaries to ~/.local/bin, installs the launcher +
# icon, wires every detected client, and prints the one sudo step (/dev/uinput).
#
# Runs from the extracted tarball root — it resolves payload paths relative to itself,
# so it has no repo dependencies. Env: DONTSPEAK_INSTALL_DIR overrides ~/.local/bin;
# DONTSPEAK_INSTALL_LOCK_WAIT sets the seconds to wait for a concurrent installer (default 600).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${DONTSPEAK_INSTALL_DIR:-$HOME/.local/bin}"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
# ── BEGIN destination lock ───────────────────────────────────────────────────
# Serializes the destructive finalization (recheck → stop → replace → wire) of ONE install
# destination across processes; downloads and staging stay outside it (unique per-invocation
# names, safe in parallel). Issue #198.
#
# COPY: byte-identical in scripts/install/web/install.sh and apps/linux/tarball-install.sh
# (packaging_sync.rs pins the two equal). Both ship standalone — one through `curl | sh`, one
# inside the tarball — so neither can source a shared file. POSIX sh only: install.sh runs
# under /bin/sh (dash on Debian/Ubuntu).
#
# mkdir is the atomic primitive because no OS-released shell lock exists on both platforms:
# flock(1) is absent on macOS, shlock(1) on Linux. (The Windows installer gets an OS-released
# one from an exclusively-shared file handle and needs none of the rules below.) A mkdir lock
# outlives its owner, so it is breakable two ways:
#   • the owner was recorded on THIS host and no longer answers `kill -0` → break now;
#   • otherwise — a foreign host token (a shared $HOME can hold another machine's lock, where
#     a pid means nothing), an unreadable owner file, or an owner that still answers → break
#     only once the lock is DS_LOCK_STALE_MIN old.
# The age cap also covers an owner that answers because its pid was REUSED by an unrelated
# process, which liveness alone cannot distinguish from a live owner; with no cap such a lock
# would block installs until that process happened to exit. Real finalization is seconds, so a
# lock this old means its owner is wedged, and breaking it is no worse than the unserialized
# behavior this replaces.
#
# Every rm below ends in `|| :` because under `set -e` a failing command that ends an if-body,
# a case-branch or a trap handler exits the shell: a lock another uid owns (rm → EPERM) would
# otherwise kill the installer outright instead of waiting out its deadline, and a failing
# release would skip the rest of cleanup and rewrite a successful install's exit status.
DS_LOCK_DIR=""
DS_LOCK_OWNED=0
DS_LOCK_STALE_MIN=60

ds_lock_path() {  # $1 = destination → sibling ".<name>.ds-install.lock"
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
# force-reclaimed after a minute — its own critical section is milliseconds, so an older slot
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
  # the rm — a sub-millisecond window that first requires the lock to be an hour old.
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
# by force-breaking it. (The Windows installer is the mirror image — there the file must stay,
# because waiters hold an open handle to it.) Owner-gated: a lock broken out from under us and
# re-taken belongs to someone else.
ds_lock_release() {
  [ "$DS_LOCK_OWNED" = 1 ] || return 0
  DS_LOCK_OWNED=0
  ds_lock_owner="$(cat "$DS_LOCK_DIR/owner" 2>/dev/null)" || ds_lock_owner=""
  case "$ds_lock_owner" in ''|"$$ "*) rm -rf "$DS_LOCK_DIR" || : ;; esac
  DS_LOCK_DIR=""
}
# ── END destination lock ─────────────────────────────────────────────────────
# Refuse an incomplete package before changing the machine. Every platform package carries
# its canonical uninstaller as payload; a missing one is a broken artifact, not optional.
[ -f "$HERE/uninstall.sh" ] || { echo "install: package is missing uninstall.sh" >&2; exit 1; }
# Serialize stop → replace → wire against a second installer writing the same per-user
# destination (#198). The payload check above is about this package, not the destination, so it
# stays outside the lock. ds_lock_acquire creates $BIN.
# Traps are armed BEFORE the acquire: a signal landing between a successful mkdir and the trap
# would leak the lock dir. Release is gated on DS_LOCK_OWNED, so arming them early is a no-op
# until the lock is actually taken. A signal trap that doesn't exit would RESUME the script
# mid-install (same reason scripts/install/web/install.sh exits from its signal traps).
trap ds_lock_release EXIT
trap 'ds_lock_release; exit 130' INT TERM HUP
ds_lock_acquire "$BIN/dontspeak"
# Stop a running host/helper first — `install` over a live binary is unguarded
# (mirrors the macOS clean step, which quits the app before replacing it).
pkill -x ds-gtk 2>/dev/null || true
pkill -f ds-helper 2>/dev/null || true
install -d "$BIN" "$APPS" "$ICONS"
install -m0755 "$HERE"/bin/* "$BIN/"
# Quote the Exec value: the desktop-entry spec word-splits it, so an install dir with a
# space would silently launch "/home/my" instead of the binary. Escape sed's replacement
# metacharacters (\, &, and the | delimiter) so an install dir containing any of them
# doesn't corrupt the generated Exec line.
ESC_BIN="$(printf '%s' "$BIN" | sed -e 's/[\\&|]/\\&/g')"
sed "s|^Exec=ds-gtk|Exec=\"$ESC_BIN/ds-gtk\"|" "$HERE/share/applications/dontspeak.desktop" > "$APPS/dontspeak.desktop"
install -m0644 "$HERE/share/icons/hicolor/scalable/apps/dontspeak.svg" "$ICONS/dontspeak.svg"
# Start-at-login by default (parity with macOS SMAppService and the Windows Run key) —
# dontspeak.desktop's own comment says the installer copies it into ~/.config/autostart/.
# Opt out with DONTSPEAK_NO_AUTOSTART=1 (same env var scripts/install/web/install.sh honors for its own,
# now-redundant copy of this step when it wraps this script).
if [ "${DONTSPEAK_NO_AUTOSTART:-0}" != "1" ]; then
  AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
  install -d "$AUTOSTART"
  cp "$APPS/dontspeak.desktop" "$AUTOSTART/dontspeak.desktop"
fi
# Standalone uninstaller onto PATH (parity with the other platform installers) — once
# the extracted tarball dir is deleted, this is the only removal path left.
install -m0755 "$HERE/uninstall.sh" "$BIN/dontspeak-uninstall"
"$BIN/dontspeak" wire --reconcile 2>/dev/null || echo "(wire skipped)"
echo
echo "Installed to $BIN. To grant /dev/uinput (synthetic keys), once:"
echo "  sudo install -m0644 '$HERE/udev/99-ds-input.rules' /etc/udev/rules.d/99-ds-input.rules"
echo "  sudo udevadm control --reload && sudo udevadm trigger && sudo usermod -aG input \"\$USER\"   # then re-login"
echo "Launch: $BIN/ds-gtk  (or the \"DontSpeak\" app menu entry)"
if [ "${DONTSPEAK_NO_AUTOSTART:-0}" != "1" ]; then
  echo "Start-at-login enabled (~/.config/autostart/dontspeak.desktop; DONTSPEAK_NO_AUTOSTART=1 to skip)"
fi
echo "Uninstall any time: $BIN/dontspeak-uninstall"
