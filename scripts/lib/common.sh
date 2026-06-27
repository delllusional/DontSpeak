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

# resolve_sign_identity — find_codesign_id, but echo "-" (ad-hoc) when none is found,
# the form `codesign --sign` expects for an ad-hoc signature.
resolve_sign_identity() {
  local id; id="$(find_codesign_id)"
  printf '%s' "${id:--}"
}
