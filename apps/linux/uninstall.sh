#!/usr/bin/env bash
# uninstall.sh -- repo-checkout entry point for the Linux uninstall. The ACTUAL logic
# lives in scripts/install/bundle/uninstall.sh -- the single source of truth, which the Linux package
# carries as a real file and places as ~/.local/bin/dontspeak-uninstall.
# packaging_sync.rs pins all copies in sync.
#
# Stops the GUI host, un-wires all clients, removes the installed binaries, the
# .desktop launchers (menu + autostart), and ALL app data / caches / state.
#
#   apps/linux/uninstall.sh           # remove binaries + data + launchers
#   apps/linux/uninstall.sh --udev    # ALSO remove the /dev/uinput udev rule (sudo)
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/scripts/install/bundle/uninstall.sh" "$@"
