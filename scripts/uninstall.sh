#!/usr/bin/env bash
# uninstall.sh — completely remove the macOS DontSpeak install (the mirror of install.sh /
# bundle.sh). Quits the running app + engine, un-wires the Claude Code hooks, deletes the
# app bundle, the in-process engine binaries, and ALL app data / caches / logs / state, so
# the next install starts truly clean.
#
# With --reset-permissions it also resets the app's TCC grants (Accessibility + Microphone
# + Input Monitoring) for `app.dontspeak.org`, so a fresh install re-triggers the OS
# permission prompts / re-creates the System Settings rows.
#
# Idempotent: every piece is removed best-effort; missing ones are skipped. macOS-focused
# (the engine runs in-process in DontSpeak.app on macOS; Linux/Windows have their own hosts).
#
#   scripts/uninstall.sh                      # remove app + data, leave TCC grants
#   scripts/uninstall.sh --reset-permissions  # ALSO reset Accessibility/Mic grants
set -uo pipefail   # deliberately NOT -e: one missing piece must not abort the teardown

RESET_PERMS=0
for a in "$@"; do
  case "$a" in
    --reset-permissions) RESET_PERMS=1 ;;
    -h | --help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "uninstall: ignoring unknown arg '$a'" >&2 ;;
  esac
done

H="$HOME"
APP="$H/Applications/DontSpeak.app"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
BUNDLE_ID="app.dontspeak.org"

echo "==> 1. quit the running app + engine + warm helper"
osascript -e 'quit app "DontSpeak"' 2>/dev/null || true
sleep 1
pkill -f "DontSpeak.app/Contents/MacOS/DontSpeak" 2>/dev/null || true
pkill -f "ds-helper" 2>/dev/null || true
pkill -x dontspeakd 2>/dev/null || true

echo "==> 2. un-wire the Claude Code hooks (before deleting the binary)"
if [ -x "$INSTALL_DIR/dontspeak" ]; then
  "$INSTALL_DIR/dontspeak" wire-hooks --remove 2>/dev/null \
    || echo "   (wire-hooks --remove failed or nothing to remove)"
else
  echo "   (no $INSTALL_DIR/dontspeak — skipping hook removal)"
fi

echo "==> 3. remove the app bundle + installed engine binaries"
rm -rf "$APP"
for b in dontspeak dontspeakd ds-helper; do rm -f "$INSTALL_DIR/$b"; done

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
rm -f "$H"/Library/Logs/dontspeak*.log* "$H"/Library/Logs/dontspeak.daemon.*.log \
      "$H"/Library/Logs/ds-helper.log
rm -f "$H"/Library/Application\ Support/CrashReporter/ds-*.plist \
      "$H"/Library/Logs/DiagnosticReports/ds-*.ips \
      "$H"/Library/Logs/DiagnosticReports/Retired/ds-*.ips
rm -f "$H/Library/LaunchAgents/org.dontspeak.daemon.plist"

echo "==> 5. forget the login item (best-effort; SMAppService also reaps it once the app is gone)"
osascript -e 'tell application "System Events" to delete login item "DontSpeak"' 2>/dev/null || true

if [ "$RESET_PERMS" = "1" ]; then
  echo "==> 6. reset TCC grants for $BUNDLE_ID (re-prompts on next install)"
  tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null || true
  tccutil reset Microphone "$BUNDLE_ID" 2>/dev/null || true
  tccutil reset ListenEvent "$BUNDLE_ID" 2>/dev/null || true
else
  echo "==> 6. (TCC grants left intact — pass --reset-permissions to also reset them)"
fi

echo
echo "Done. DontSpeak removed. Reinstall with: ./apps/macos/bundle.sh && open ~/Applications/DontSpeak.app"
