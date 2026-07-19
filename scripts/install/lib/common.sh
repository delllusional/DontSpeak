#!/usr/bin/env bash
# common.sh — source only. Single source for install-engine + macOS bundler helpers:
# compute_build_id, find_codesign_id, resolve_sign_identity, require_engine_symbol,
# swift_build_resilient. Also normalizes PATH.

# Toolchain dirs for non-interactive shells (launchd/cron/IDE).
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"

# compute_build_id [repo_dir] — $DONTSPEAK_BUILD_ID, else git short-12 [+-dirty], else "dev".
compute_build_id() {
  if [ -n "${DONTSPEAK_BUILD_ID:-}" ]; then printf '%s' "$DONTSPEAK_BUILD_ID"; return; fi
  local repo="${1:-.}" id
  id="$(git -C "$repo" rev-parse --short=12 HEAD 2>/dev/null || echo dev)"
  # Outside a repo, git diff exits 128 — don't turn "dev" into "dev-dirty".
  if [ "$id" != "dev" ] \
     && { ! git -C "$repo" diff --quiet 2>/dev/null || ! git -C "$repo" diff --cached --quiet 2>/dev/null; }; then
    id="${id}-dirty"
  fi
  printf '%s' "$id"
}

# require_engine_symbol BIN — has ds_engine_start (stale SwiftPM can drop force_load).
# Use grep -c not -q (pipefail + SIGPIPE → false FATAL).
require_engine_symbol() {
  local n
  n="$(nm "$1" 2>/dev/null | grep -cE '^[0-9a-fA-F]+ [Tt] _?ds_engine_start$' || true)"
  [ "${n:-0}" -gt 0 ]
}

# find_codesign_id — $DONTSPEAK_CODESIGN_ID, else first matching identity.
# Dist: Developer ID only. Local: Apple Development + untrusted Local Dev (no -v).
find_codesign_id() {
  if [ -n "${DONTSPEAK_CODESIGN_ID:-}" ]; then printf '%s' "$DONTSPEAK_CODESIGN_ID"; return; fi
  local pattern='"(Developer ID Application|Apple Development): [^"]+"'
  [ "${DONTSPEAK_DIST:-0}" = "1" ] && pattern='"Developer ID Application: [^"]+"'
  local id
  id="$(security find-identity -v -p codesigning 2>/dev/null | grep -Eo "$pattern" | head -1 | tr -d '"')"
  # Local Dev is untrusted (hidden by -v); stable cdhash keeps TCC across rebuilds.
  if [ -z "$id" ] && [ "${DONTSPEAK_DIST:-0}" != "1" ]; then
    id="$(security find-identity -p codesigning 2>/dev/null | grep -Eo '"DontSpeak Local Dev"' | head -1 | tr -d '"')"
  fi
  printf '%s' "$id"
}

# ensure_local_sign_identity — mint Local Dev cert once if none (TCC-stable). No-op
# dist / pinned id / NO_AUTOSIGN. Chatter → stderr (stdout = identity only).
ensure_local_sign_identity() {
  [ "${DONTSPEAK_DIST:-0}" = "1" ] && return 0
  [ -n "${DONTSPEAK_CODESIGN_ID:-}" ] && return 0
  [ "${DONTSPEAK_NO_AUTOSIGN:-0}" = "1" ] && return 0
  [ -n "$(find_codesign_id)" ] && return 0
  command -v openssl >/dev/null 2>&1 || {
    echo "   WARN: no codesigning identity and openssl missing — build will be ad-hoc (TCC grants won't persist). See docs/SIGNING.md." >&2
    return 0
  }
  echo "   no codesigning identity — minting self-signed 'DontSpeak Local Dev' once (stable signature → TCC grants persist)…" >&2
  local td; td="$(mktemp -d)" || return 0
  local pw="dontspeak" p12ok=0 legacy
  if openssl req -x509 -newkey rsa:2048 -nodes -keyout "$td/k.key" -out "$td/c.crt" -days 3650 \
       -subj "/CN=DontSpeak Local Dev" \
       -addext "extendedKeyUsage=critical,codeSigning" \
       -addext "basicConstraints=critical,CA:false" \
       -addext "keyUsage=critical,digitalSignature" >/dev/null 2>&1; then
    # OpenSSL 3 needs -legacy for Apple import; LibreSSL has no -legacy — try both.
    for legacy in "-legacy" ""; do
      if openssl pkcs12 -export $legacy -inkey "$td/k.key" -in "$td/c.crt" -out "$td/id.p12" \
           -name "DontSpeak Local Dev" -passout "pass:$pw" >/dev/null 2>&1; then p12ok=1; break; fi
    done
  fi
  if [ "$p12ok" = 1 ] && security import "$td/id.p12" \
       -k "$HOME/Library/Keychains/login.keychain-db" -P "$pw" -T /usr/bin/codesign -A >/dev/null 2>&1; then
    echo "   imported 'DontSpeak Local Dev' into the login keychain — grant each permission once; it sticks thereafter." >&2
  else
    echo "   WARN: couldn't mint/import the local signing cert — build will fall back to ad-hoc (TCC grants won't persist). See docs/SIGNING.md to do it by hand." >&2
  fi
  rm -rf "$td"
  return 0
}

# resolve_sign_identity — ensure local cert, then identity or "-" (ad-hoc).
resolve_sign_identity() {
  ensure_local_sign_identity
  local id; id="$(find_codesign_id)"
  printf '%s' "${id:--}"
}

# swift_build_resilient PKG_DIR ARGS… — one retry on stale ModuleCache path mismatch only.
swift_build_resilient() {
  local pkg="$1"; shift
  local log; log="$(mktemp)"
  if ( cd "$pkg" && swift build "$@" ) >"$log" 2>&1; then
    cat "$log"; rm -f "$log"; return 0
  fi
  cat "$log"
  if grep -q "module cache path" "$log"; then
    echo "   stale Swift module cache — clearing .build ModuleCache and retrying once" >&2
    rm -f "$log"
    find "$pkg/.build" -type d -name ModuleCache -prune -exec rm -rf {} + 2>/dev/null || true
    ( cd "$pkg" && swift build "$@" )
    return $?
  fi
  rm -f "$log"; return 1
}
