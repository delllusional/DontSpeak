---
name: build-linux
description: Uninstall / build+install / package DontSpeak on Linux (GTK4 desktop host). Three flows — (1) uninstall/clean, (2) local build + reinstall for dev testing, (3) build the distributable tarball (.tar.gz — the one and only Linux package). Use when asked to build, reinstall, package, or uninstall the Linux app. Runs on Linux only.
---

# DontSpeak — Linux (uninstall / install / package)

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Linux only (native/WSL/VM), repo root. Host: GTK4 + libadwaita `ds-gtk` (in-process
engine). Scripts: `scripts/install/local/install.sh`, `apps/linux/*.sh` — edit those,
don't duplicate.

**Prereqs:** Rust · GTK 4.12+ · **libadwaita ≥ 1.7** · gtk4-layer-shell (Ubuntu 26.04 /
Fedora 42 era; 24.04 too old) · `/dev/uinput` write (udev + `input` group via
`install-gui.sh` unless `--no-udev`). `install-gui.sh` checks pkg-config **presence**
only — too-old libadwaita fails at compile.

## 1 — Uninstall

```bash
apps/linux/uninstall.sh           # full removal
apps/linux/uninstall.sh --udev    # also drop uinput udev rule (sudo)
```

Wraps `scripts/install/bundle/uninstall.sh` (same as release `dontspeak-uninstall`).
Leaves `input` group membership.

## 2 — Build + install (dev)

```bash
scripts/install/local/install.sh   # engine + helper + hooks → ~/.local/bin
apps/linux/install-gui.sh          # flags: --autostart  --aec  --no-udev
# launch: app menu or ~/.local/bin/ds-gtk  (log out/in if udev just ran)
```

## 3 — Package

```bash
apps/linux/package.sh              # OUTDIR overrides; default ./dist
```

Output: `dontspeak-<ver>-linux-<arch>.tar.gz` only Linux package. Extract → `./install.sh`.

## Notes

Release CI: GTK tests block; tarball upload best-effort. `uninstall.sh` not in CI —
verify on Linux.
