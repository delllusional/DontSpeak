---
name: build-linux
description: Uninstall / build+install / package DontSpeak on Linux (GTK4 desktop host). Three flows — (1) uninstall/clean, (2) local build + reinstall for dev testing, (3) build the distributable tarball (.tar.gz — the one and only Linux package). Use when asked to build, reinstall, package, or uninstall the Linux app. Runs on Linux only.
---

# DontSpeak — Linux (uninstall / install / package)

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

> **Runs on:** Linux only (native, WSL, or a VM — bash + GTK4/libadwaita). **Working dir:** repo root. Same three flows as `build-macos` / `build-windows`.

The host is the **GTK4 + libadwaita desktop app** (`ds-gtk`) — tray, health panel, dictation overlay; it hosts the engine in-process. No separate daemon. Scripts: `scripts/install.sh` + `apps/linux/*.sh`, factored via `scripts/lib/common.sh` — **don't duplicate build logic; edit the scripts**.

**Prereqs:** Rust · a recent GNOME stack — GTK 4.12+, **libadwaita >= 1.7**, gtk4-layer-shell (Ubuntu 26.04 / Fedora 42 era; **Ubuntu 24.04 is too old**) · write access to `/dev/uinput` (udev rule + `input` group, handled by `install-gui.sh` unless `--no-udev`). `install-gui.sh` checks the three `pkg-config` names (`gtk4`, `libadwaita-1`, `gtk4-layer-shell-0`) and prints the exact `apt`/`dnf` install command for whatever's missing — but only for *presence*, not version, so a too-old libadwaita passes that check and fails later at compile time instead.

## 1 — Uninstall / clean

```bash
apps/linux/uninstall.sh           # full removal (thin wrapper over scripts/uninstall.sh)
apps/linux/uninstall.sh --udev    # ALSO remove the /dev/uinput udev rule (sudo)
```
Execs `scripts/uninstall.sh` — the canonical uninstaller (same bytes the release installer places as `dontspeak-uninstall`; `packaging_sync.rs` pins the copies): stops the host, un-wires all clients, removes the binaries, launchers, icon, the `--aec` drop-in, and all data/state/cache. `input`-group membership is left intact. Stop the running host first for a clean reinstall.

## 2 — Build + install (dev)

Two builds, then launch:

1. **Engine + helper + hooks** → `~/.local/bin` (`DONTSPEAK_INSTALL_DIR` overrides):
   ```bash
   scripts/install.sh
   ```
2. **GTK desktop host** (builds `apps/linux/gtk` release → installs `ds-gtk` + `.desktop` + udev/input perms):
   ```bash
   apps/linux/install-gui.sh           # flags: --autostart  --aec  --no-udev
   ```
3. **Launch** from the app menu ("DontSpeak") or `~/.local/bin/ds-gtk`. (If the udev/`input`-group step just ran, log out/in once so the membership takes effect.)

## 3 — Build the package

```bash
apps/linux/package.sh                 # tarball → ./dist  (OUTDIR=~/Desktop to change)
```
- Output: **`dontspeak-<ver>-linux-<arch>.tar.gz`** — the ONE Linux package. Self-contained portable bundle (binaries + `.desktop` + icon + udev rule + `install.sh`, shipped verbatim from `apps/linux/tarball-install.sh`). Extract and run `./install.sh`.

## Notes

- Release CI runs the GTK tests before packaging on both Linux architectures. Test or setup
  failures block the release; only tarball creation and upload are best-effort, so either
  Linux asset may be absent after a packaging-only failure. `uninstall.sh` remains
  unexercised in CI — **verify it on Linux**.
