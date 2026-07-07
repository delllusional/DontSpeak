#!/usr/bin/env bash
# uninstall.sh — THE DontSpeak uninstaller (macOS + Linux): the single source of truth.
#
# Removes the whole install, whichever flow created it:
#   • the app bundle: ~/Applications/DontSpeak.app (macOS) — release (web/install.sh)
#     and dev (apps/macos/bundle.sh) share this ONE per-user layout; DONTSPEAK_APP_DIR
#     overrides it in both
#   • CLI/engine binaries in ~/.local/bin (all flows, and the whole Linux install)
# plus the client wiring (Claude Code hooks + MCP, Codex hooks),
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

    echo "==> 2. un-wire every client (Claude Code hooks + MCP, Codex) before deleting binaries"
    # `wire --all --remove` strips EVERY client's integration: claude_code = hooks
    # (settings.json) + MCP (~/.claude.json); codex = hooks (~/.codex/config.toml). Prefer the
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
      echo "    ~/.claude.json by hand if it lingers)"
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
