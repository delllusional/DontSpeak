# -- BEGIN destination lock ---------------------------------------------------
# Shared by repository-local install flows. Standalone release installers carry
# the same implementation inline because they cannot source repository files.
DS_LOCK_DIR=""
DS_LOCK_OWNED=0
DS_LOCK_STALE_MIN=60

ds_lock_path() {
  printf '%s/.%s.ds-install.lock' "$(dirname "$1")" "$(basename "$1")"
}

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
  [ -n "$(find "$DS_LOCK_DIR" -maxdepth 0 -mmin "+$DS_LOCK_STALE_MIN" 2>/dev/null)" ]
}

ds_lock_break() {
  ds_lock_breaker="$DS_LOCK_DIR.breaker"
  if ! mkdir "$ds_lock_breaker" 2>/dev/null; then
    if [ -n "$(find "$ds_lock_breaker" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
      rm -rf "$ds_lock_breaker" || :
    fi
    return 0
  fi
  if ds_lock_stale; then rm -rf "$DS_LOCK_DIR" || :; fi
  rmdir "$ds_lock_breaker" 2>/dev/null || :
}

ds_lock_acquire() {
  DS_LOCK_DIR="$(ds_lock_path "$1")"
  mkdir -p "$(dirname "$DS_LOCK_DIR")"
  ds_lock_waited=0
  ds_lock_notified=0
  while :; do
    if mkdir "$DS_LOCK_DIR" 2>/dev/null; then
      DS_LOCK_OWNED=1
      printf '%s %s\n' "$$" "$(uname -n)" > "$DS_LOCK_DIR/owner"
      return 0
    fi
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

ds_lock_release() {
  [ "$DS_LOCK_OWNED" = 1 ] || return 0
  DS_LOCK_OWNED=0
  ds_lock_owner="$(cat "$DS_LOCK_DIR/owner" 2>/dev/null)" || ds_lock_owner=""
  case "$ds_lock_owner" in ''|"$$ "*) rm -rf "$DS_LOCK_DIR" || : ;; esac
  DS_LOCK_DIR=""
}
# -- END destination lock -----------------------------------------------------
