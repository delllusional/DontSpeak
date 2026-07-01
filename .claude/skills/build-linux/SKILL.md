---
name: build-linux
description: Build / clean-reinstall / package DontSpeak on Linux (GTK4 desktop host). Two use cases — (1) local clean build + reinstall for dev testing, (2) build distributable packages (.tar.gz always; .deb/.rpm/AppImage when their tool is installed). Use when asked to build, reinstall, package, or uninstall the Linux app. Runs on Linux (the WSL/VM host) — NOT on Windows/macOS.
---

# DontSpeak — Linux (build / reinstall / package)

> **Runs on:** Linux (the WSL Ubuntu build host or the VirtualBox VM). Not on the Windows dev box — these are bash + GTK4/libadwaita. **Working dir:** repo root. Symmetrical with `build-windows` / `build-macos`. Use the **Bash** tool on the Linux host.

The host is the **GTK4 + libadwaita desktop app** (`ds-gtk`) — tray, health panel, dictation overlay; it hosts the engine in-process. There is no separate daemon.

Scripts: `scripts/install.sh` + `apps/linux/*.sh`, factored via `scripts/lib/common.sh`. **Don't duplicate build logic** — edit the scripts.

**Prereqs:** Rust · GTK4 + libadwaita dev packages · write access to `/dev/uinput` (the udev rule + `input` group, handled by `install-gui.sh` unless `--no-udev`).

## Use case 1 — local clean build + reinstall (dev)

Two steps — engine bins first, then the GUI host:

1. **Engine + helper + hooks** → `~/.local/bin` (`DONTSPEAK_INSTALL_DIR` to override):
   ```bash
   scripts/install.sh
   ```
2. **GTK desktop host** (builds `apps/linux/gtk` release → installs `ds-gtk` + `.desktop` + udev/input perms):
   ```bash
   apps/linux/install-gui.sh           # flags: --autostart  --aec  --no-udev
   ```
3. **Launch** from the app menu ("DontSpeak") or directly:
   ```bash
   ~/.local/bin/ds-gtk
   ```
   (If the udev/`input`-group step just ran, log out/in once so the group membership takes effect.)

For a **clean** reinstall: stop the running host first, then run `apps/linux/uninstall.sh` (see below), then re-run the steps.

## Use case 2 — build a distributable package

```bash
apps/linux/package.sh                 # all formats → ./dist  (OUTDIR=~/Desktop to change)
apps/linux/package.sh --skip-appimage # tarball + deb + rpm only
```
Builds the engine bins + the GTK host, then emits to `dist/`:

- **`dontspeak-<ver>-<arch>.tar.gz`** — **always**. Self-contained portable bundle (binaries + `.desktop` + icon + udev rule + an `install.sh`); the universal baseline, like the Windows portable zip. Extract and run `./install.sh`.
- **`.deb`** — when `cargo deb` is installed (`cargo install cargo-deb`). Layout from `[package.metadata.deb]` in `apps/linux/gtk/Cargo.toml`; GTK deps auto-detected via `$auto`.
- **`.rpm`** — when `cargo generate-rpm` is installed (`cargo install cargo-generate-rpm`). Layout from `[package.metadata.generate-rpm]`.
- **AppImage** — **experimental**; only when `linuxdeploy` + `linuxdeploy-plugin-gtk` are on PATH (GTK bundling is finicky — verify on the target). Skip with `--skip-appimage`.

Each native format is best-effort: a missing tool is skipped with an install hint, so the tarball always succeeds. The tarball/`.deb`/`.rpm` path is now CI-exercised on every release (see NOTE); the **AppImage** path + `uninstall.sh` remain unexercised — **verify those on Linux**.

> NOTE: `release.yml` now builds + uploads the Linux packages (`.tar.gz`/`.deb`/`.rpm`) via a `linux` job on `ubuntu-26.04` that runs `apps/linux/package.sh --skip-appimage` (installing `cargo-deb` + `cargo-generate-rpm` and the real GTK/libadwaita/layer-shell deps) and uploads them as the `linux-packages` artifact (`if-no-files-found: error`).

## Uninstall / clean

```bash
apps/linux/uninstall.sh           # stop host, un-wire hooks, remove bins + launchers + data
apps/linux/uninstall.sh --udev    # ALSO remove the /dev/uinput udev rule (sudo)
```
Mirrors the macOS `scripts/uninstall.sh`: stops the GUI host, un-wires the Claude Code hooks, removes `~/.local/bin/{ds-gtk,dontspeak,ds-helper}`, the `.desktop` launcher + autostart entry, and the app data/state/cache (`~/.config/dontspeak`, `~/.local/state/dontspeak`, `~/.cache/dontspeak`). The `input`-group membership is left intact.

## Notes

- The package + uninstall scripts were added 2026-06-28 to close the Windows/macOS symmetry. The `package.sh --skip-appimage` path (tarball + `.deb` + `.rpm`) is now run in CI on `ubuntu-26.04` every release, so it's exercised. The **AppImage** path and `uninstall.sh` are still **unexercised on Linux** — first run of those on a Linux host should be treated as verification.
