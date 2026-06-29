#!/usr/bin/env bash
# common.sh — shared helpers for the DontSpeak build/install scripts. SOURCE this file
# (`. scripts/lib/common.sh`); do NOT execute it.
#
# It is the SINGLE source of truth for the two pieces of logic that the engine
# installer (scripts/install-daemon.sh) and the macOS app bundler
# (apps/macos/bundle-lib.sh) MUST agree on:
#   • compute_build_id   — the lockstep BUILD_ID stamped into BOTH the engine and the
#                          app; the app compares them at runtime, so a hand-maintained
#                          second copy that drifts triggers a false "rebuild needed".
#   • find_codesign_id   — the codesigning identity, resolved identically everywhere.
# Defining either twice is exactly what this file exists to prevent.
#
# Provides (all safe to call after sourcing): sed_escape, bak, compute_build_id,
# find_codesign_id, resolve_sign_identity. Sourcing also normalizes PATH (below).

# Prepend the usual toolchain dirs (cargo + Homebrew) so cargo/swift/magick resolve the
# same way under launchd/cron/IDE shells that don't load an interactive login PATH.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}"

# sed_escape STR — escape &, |, \ so STR is safe to use as a sed replacement string.
sed_escape() { printf '%s' "$1" | sed -e 's/[&|\\]/\\&/g'; }

# bak FILE — timestamped backup copy of FILE if it exists (never fails the caller).
bak() { [ -f "$1" ] && cp -f "$1" "$1.bak.$(date +%s)" && echo "  backed up $1"; true; }

# compute_build_id [repo_dir] — the lockstep build id: a pinned $DONTSPEAK_BUILD_ID if
# set, else git short-12 of repo_dir's HEAD (default: cwd) + "-dirty" when the tree has
# uncommitted/staged changes, else "dev" with no git. Both honoring $DONTSPEAK_BUILD_ID
# is what keeps the engine and app ids identical.
compute_build_id() {
  if [ -n "${DONTSPEAK_BUILD_ID:-}" ]; then printf '%s' "$DONTSPEAK_BUILD_ID"; return; fi
  local repo="${1:-.}" id
  id="$(git -C "$repo" rev-parse --short=12 HEAD 2>/dev/null || echo dev)"
  if ! git -C "$repo" diff --quiet 2>/dev/null || ! git -C "$repo" diff --cached --quiet 2>/dev/null; then
    id="${id}-dirty"
  fi
  printf '%s' "$id"
}

# find_codesign_id — echo the first macOS codesigning identity, or EMPTY if none found.
# Honors $DONTSPEAK_CODESIGN_ID. In dist mode ($DONTSPEAK_DIST=1) ONLY a "Developer ID
# Application" identity qualifies (Apple Development / ad-hoc can't be notarized);
# otherwise Apple Development is also accepted for local installs.
find_codesign_id() {
  if [ -n "${DONTSPEAK_CODESIGN_ID:-}" ]; then printf '%s' "$DONTSPEAK_CODESIGN_ID"; return; fi
  local pattern='"(Developer ID Application|Apple Development): [^"]+"'
  [ "${DONTSPEAK_DIST:-0}" = "1" ] && pattern='"Developer ID Application: [^"]+"'
  local id
  id="$(security find-identity -v -p codesigning 2>/dev/null | grep -Eo "$pattern" | head -1 | tr -d '"')"
  # Local-dev fallback: a self-signed "DontSpeak Local Dev" cert. It's untrusted, so
  # `find-identity -v` (valid-only) hides it — query WITHOUT -v. A stable signature
  # keeps TCC grants (Accessibility / Input Monitoring) across rebuilds; without it
  # ad-hoc rotates the cdhash and every grant breaks. Skipped in dist mode. See docs/signing.md.
  if [ -z "$id" ] && [ "${DONTSPEAK_DIST:-0}" != "1" ]; then
    id="$(security find-identity -p codesigning 2>/dev/null | grep -Eo '"DontSpeak Local Dev"' | head -1 | tr -d '"')"
  fi
  printf '%s' "$id"
}

# ensure_local_sign_identity — for LOCAL (non-dist) builds, guarantee a STABLE codesigning
# identity exists so TCC grants (Accessibility / Input Monitoring) survive rebuilds. A clean
# clone has no identity, so ad-hoc signing rotates the cdhash every build and every grant
# breaks; this mints the self-signed "DontSpeak Local Dev" cert ONCE (the manual recipe from
# docs/signing.md, automated) and imports it into the login keychain. Idempotent: returns
# immediately if any usable identity (Developer ID / Apple Development / the local-dev cert)
# already exists. No-ops in dist mode (DONTSPEAK_DIST=1 requires a real Developer ID), when
# DONTSPEAK_CODESIGN_ID pins one, or when DONTSPEAK_NO_AUTOSIGN=1 opts out. All chatter →
# stderr so resolve_sign_identity's stdout stays a clean identity string.
ensure_local_sign_identity() {
  [ "${DONTSPEAK_DIST:-0}" = "1" ] && return 0
  [ -n "${DONTSPEAK_CODESIGN_ID:-}" ] && return 0
  [ "${DONTSPEAK_NO_AUTOSIGN:-0}" = "1" ] && return 0
  [ -n "$(find_codesign_id)" ] && return 0
  command -v openssl >/dev/null 2>&1 || {
    echo "   WARN: no codesigning identity and openssl missing — build will be ad-hoc (TCC grants won't persist). See docs/signing.md." >&2
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
    # OpenSSL 3 defaults to a MAC Apple's `security import` rejects → need -legacy; LibreSSL
    # has no -legacy flag → try with, then without, so both toolchains work on a clean box.
    for legacy in "-legacy" ""; do
      if openssl pkcs12 -export $legacy -inkey "$td/k.key" -in "$td/c.crt" -out "$td/id.p12" \
           -name "DontSpeak Local Dev" -passout "pass:$pw" >/dev/null 2>&1; then p12ok=1; break; fi
    done
  fi
  if [ "$p12ok" = 1 ] && security import "$td/id.p12" \
       -k "$HOME/Library/Keychains/login.keychain-db" -P "$pw" -T /usr/bin/codesign -A >/dev/null 2>&1; then
    echo "   imported 'DontSpeak Local Dev' into the login keychain — grant each permission once; it sticks thereafter." >&2
  else
    echo "   WARN: couldn't mint/import the local signing cert — build will fall back to ad-hoc (TCC grants won't persist). See docs/signing.md to do it by hand." >&2
  fi
  rm -rf "$td"
  return 0
}

# resolve_sign_identity — find_codesign_id, but echo "-" (ad-hoc) when none is found,
# the form `codesign --sign` expects for an ad-hoc signature. First runs
# ensure_local_sign_identity so a clean local build self-provisions a stable cert
# (skipped in dist mode) instead of silently falling back to grant-breaking ad-hoc.
resolve_sign_identity() {
  ensure_local_sign_identity
  local id; id="$(find_codesign_id)"
  printf '%s' "${id:--}"
}
