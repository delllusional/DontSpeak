#!/usr/bin/env bash
# install-daemon.sh — build + install the dontspeak ENGINE BINARIES + hooks with a
# BUILD_ID. (Name kept for compatibility; there is no standalone daemon any more —
# DontSpeak.app hosts the engine in-process.) Called by BOTH:
#   • scripts/install.sh  (first-time / full CLI install + the settings.json snippet), and
#   • apps/macos/bundle.sh (so building the app ALWAYS rebuilds+installs matching binaries).
#
# IMPORTANT (see docs/BUILD-DEPLOY.md): this installs to ~/.local/bin only. The RUNNING APP
# spawns its OWN BUNDLED `Contents/MacOS/ds-helper` and runs the engine in-process, so a
# HELPER or ENGINE change is NOT live until a full `bundle.sh` (or a manual copy-into-bundle +
# re-sign + relaunch). Only HOOK / MCP changes in the `dontspeak` binary go live via this script
# (the wired hooks invoke ~/.local/bin/dontspeak directly).
#
# What it does (idempotent, macOS-first):
#   1. compute a BUILD_ID (git short hash + -dirty) unless DONTSPEAK_BUILD_ID is set,
#   2. cargo build --release the 3 bins WITH that id baked into dontspeakd,
#   3. install them to $INSTALL_DIR (default ~/.local/bin),
#   4. codesign dontspeakd with a STABLE identity (so the Accessibility/Input-
#      Monitoring grant survives rebuilds), ad-hoc for the rest,
#   5. install the optional swift hook helpers (macOS only),
#   6. defensively remove any stale standalone launchd daemon (the app hosts it now),
#   (logging is ~/Library/Logs/dontspeak.log with in-process rotation, no conf.)
#
# Inputs (env): DONTSPEAK_INSTALL_DIR, DONTSPEAK_BUILD_ID, DONTSPEAK_CODESIGN_ID.
# Echoes the resolved BUILD_ID as its LAST line (callers capture it for Info.plist).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Shared helpers: PATH setup, sed_escape, bak, compute_build_id, find_codesign_id.
. "$REPO/scripts/lib/common.sh"
RUST_DIR="$REPO/rust"
H="$HOME"
INSTALL_DIR="${DONTSPEAK_INSTALL_DIR:-$H/.local/bin}"
UNAME="$(uname -s)"

# ── 1. BUILD_ID (git short hash + -dirty), unless the caller pinned one ───────────
# compute_build_id (scripts/lib/common.sh) is the SAME id the macOS app stamps, so the
# engine and app stay in lockstep (the app's drift check compares them).
DONTSPEAK_BUILD_ID="$(compute_build_id "$REPO")"; export DONTSPEAK_BUILD_ID
echo "==> engine BUILD_ID = $DONTSPEAK_BUILD_ID" >&2

mkdir -p "$INSTALL_DIR"

# ── 2. build the 3 bins (dontspeakd carries the BUILD_ID via its build.rs) ──────────
echo "==> build engine + hooks + kokoro/mcp (release)" >&2
( cd "$RUST_DIR" && cargo build --release \
    -p dontspeakd -p dontspeak -p ds-tts \
    --bin dontspeakd --bin dontspeak --bin ds-helper ) >&2

echo "==> install binaries → $INSTALL_DIR" >&2
REL="$RUST_DIR/target/release"
for b in dontspeakd dontspeak ds-helper; do
  install -m 0755 "$REL/$b" "$INSTALL_DIR/$b"
done

# ── 3. codesign: STABLE identity for dontspeakd (TCC grant survives rebuilds) ───────
if [ "$UNAME" = "Darwin" ]; then
  STABLE_ID="$(find_codesign_id || true)"   # shared resolver (scripts/lib/common.sh); empty → ad-hoc. `|| true`: with no identity the pipeline's grep exits 1, and under `set -euo pipefail` a bare command-subst assignment would abort before the ad-hoc fallback below.
  sign_stable() {
    codesign --force --identifier "org.dontspeak.daemon" --sign "$STABLE_ID" "$INSTALL_DIR/$1" 2>/dev/null \
      && echo "   signed $1 (stable: ${STABLE_ID%% (*}…)" >&2 \
      || { echo "   !! stable-sign $1 failed; ad-hoc fallback" >&2; codesign --force --sign - "$INSTALL_DIR/$1" 2>/dev/null; }
  }
  for b in dontspeakd dontspeak ds-helper; do
    case "$b" in
      dontspeakd)
        if [ -n "$STABLE_ID" ]; then sign_stable "$b"
        else codesign --force --sign - "$INSTALL_DIR/$b" 2>/dev/null \
               && echo "   ad-hoc signed $b (no stable identity — grant RE-PROMPTS on rebuild; set DONTSPEAK_CODESIGN_ID)" >&2; fi ;;
      *) codesign --force --sign - "$INSTALL_DIR/$b" 2>/dev/null && echo "   ad-hoc signed $b" >&2 || true ;;
    esac
  done
fi

# ── 4. hooks → ~/.claude/hooks (optional swift helpers; macOS only) ───────────────
echo "==> hooks → ~/.claude/hooks" >&2
mkdir -p "$H/.claude/hooks"
if [ "$UNAME" = "Darwin" ] && command -v swiftc >/dev/null 2>&1; then
  for s in mic-active capslock; do
    if [ -f "$REPO/claude/hooks/$s.swift" ]; then
      cp -f "$REPO/claude/hooks/$s.swift" "$H/.claude/hooks/$s.swift"
      swiftc -O "$H/.claude/hooks/$s.swift" -o "$H/.claude/hooks/$s" \
        && echo "   compiled $s" >&2 || echo "   !! swiftc $s failed (continuing)" >&2
    fi
  done
fi
cp -f "$REPO/claude/hooks/HOOKS-README.md" "$H/.claude/hooks/README.md" 2>/dev/null || true

# ── 5. NO standalone launchd daemon on macOS ──────────────────────────────────────
# DontSpeak.app HOSTS the engine in-process (ds_engine_start) and owns the RPC
# socket. A standalone launchd `dontspeakd` would be a SECOND engine fighting the app
# for the dontspeak.sock in our data dir — exactly the duplicate-engine bug. So we do NOT install
# one; instead we DEFENSIVELY remove any stale agent left by an older install, and
# the MCP server launches the app (app.dontspeak.org) when the engine is needed.
if [ "$UNAME" = "Darwin" ]; then
  echo "==> remove any stale standalone daemon (app hosts the engine)" >&2
  mkdir -p "$H/Library/Logs"
  stale="$H/Library/LaunchAgents/org.dontspeak.daemon.plist"
  [ -f "$stale" ] && { launchctl unload "$stale" 2>/dev/null || true; rm -f "$stale"; \
    echo "   removed stale org.dontspeak.daemon" >&2; }
  # Logging writes ~/Library/Logs/dontspeak.log with lean in-process size rotation
  # (rename-based, sudo-free) — there is no newsyslog conf to install.
fi

# LAST line: the resolved id, for callers (bundle.sh stamps Info.plist with it).
echo "$DONTSPEAK_BUILD_ID"
