#!/usr/bin/env bash
# notarize.sh — notarize + staple a signed DontSpeak.app.
#
# Prereq: the artifact is ALREADY Developer-ID signed with hardened runtime + secure
# timestamp (dist-apps.sh / bundle-lib.sh do this when DONTSPEAK_DIST=1).
#
# Credentials (pick one):
#   • DONTSPEAK_NOTARY_PROFILE — a stored notarytool keychain profile (recommended).
#       One-time setup:
#         xcrun notarytool store-credentials <name> \
#           --apple-id <you@example.com> --team-id <TEAMID> --password <app-specific-pw>
#       then: export DONTSPEAK_NOTARY_PROFILE=<name>
#   • DONTSPEAK_APPLE_ID + DONTSPEAK_TEAM_ID + DONTSPEAK_APP_PASSWORD (app-specific password).
#
# Usage: macos/notarize.sh <path/to/DontSpeak.app>
set -euo pipefail

TARGET="${1:?usage: notarize.sh <DontSpeak.app>}"
[ -e "$TARGET" ] || { echo "no such file: $TARGET" >&2; exit 1; }

# Resolve credentials → notarytool auth args.
AUTH=()
if [ -n "${DONTSPEAK_NOTARY_PROFILE:-}" ]; then
  AUTH=(--keychain-profile "$DONTSPEAK_NOTARY_PROFILE")
elif [ -n "${DONTSPEAK_APPLE_ID:-}" ] && [ -n "${DONTSPEAK_TEAM_ID:-}" ] && [ -n "${DONTSPEAK_APP_PASSWORD:-}" ]; then
  AUTH=(--apple-id "$DONTSPEAK_APPLE_ID" --team-id "$DONTSPEAK_TEAM_ID" --password "$DONTSPEAK_APP_PASSWORD")
else
  echo "ERROR: no notary credentials." >&2
  echo "  Set DONTSPEAK_NOTARY_PROFILE (recommended), or the trio" >&2
  echo "  DONTSPEAK_APPLE_ID + DONTSPEAK_TEAM_ID + DONTSPEAK_APP_PASSWORD." >&2
  echo "  One-time: xcrun notarytool store-credentials <name> --apple-id <id> --team-id <team> --password <app-specific-pw>" >&2
  exit 2
fi

# A .app must be zipped for submission to the notary service.
ZIP="$(mktemp -u).zip"; ditto -c -k --keepParent "$TARGET" "$ZIP"
cleanup() { rm -f "$ZIP"; }
trap cleanup EXIT

echo "==> submitting $(basename "$TARGET") to the notary service (waits for the verdict)…"
xcrun notarytool submit "$ZIP" "${AUTH[@]}" --wait

echo "==> stapling the ticket to $(basename "$TARGET")"
xcrun stapler staple "$TARGET"

echo "==> verifying with Gatekeeper"
spctl -a -vvv --type exec "$TARGET"

echo "✔ notarized + stapled: $TARGET"
