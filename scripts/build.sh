#!/bin/bash
# Build the RUST dontspeakd engine binary. Output: rust/target/release/dontspeakd.
# This is the HEADLESS engine host (Linux/CLI). On macOS the engine runs in-process
# inside DontSpeak.app (built via apps/macos/bundle.sh) — there is no standalone daemon,
# and TCC grants land on the app, not this binary.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$DIR/lib/common.sh"   # PATH setup + shared helpers
RUST_DIR="$(cd "$DIR/.." && pwd)/rust"
command -v cargo >/dev/null 2>&1 || { echo "MISSING: cargo"; exit 1; }

echo "==> building dontspeakd (cargo --release)"
( cd "$RUST_DIR" && cargo build --release -p dontspeakd )
echo "    built: $RUST_DIR/target/release/dontspeakd"
echo
echo "Install it (and the rest of the stack) with scripts/install.sh, which copies"
echo "it to \$DONTSPEAK_INSTALL_DIR (default ~/.local/bin) and signs it."
echo
echo "macOS: rebuild the app with apps/macos/bundle.sh (it hosts the engine)."
echo "Linux/headless: (re)start the systemd service with apps/linux/enable-daemon.sh, or"
echo "nudge a running engine to reload config: kill -HUP \$(cat \"\$HOME/Library/Application Support/org.dontspeak.DontSpeak/dontspeakd.pid\")"
