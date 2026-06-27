---
name: build-macos
description: Build / clean-reinstall / package DontSpeak on macOS. Two use cases — (1) local clean build + reinstall of DontSpeak.app for dev testing, (2) build a distributable .dmg (signed + notarized when Apple creds are set). Use when asked to build, reinstall, package, or cut the macOS app. Runs on a Mac (locally or over the Mac SSH host) — NOT on Windows/Linux.
---

# DontSpeak — macOS (build / reinstall / package)

> **Runs on:** a Mac (Apple Silicon; locally or over the Mac SSH host). Cannot run on the Windows dev box — these are bash + Xcode + `codesign`/`notarytool`. **Working dir:** repo root. Symmetrical with `build-windows` / `build-linux`. Use the **Bash** tool on the Mac.

Scripts live under `apps/macos/` and `scripts/`, already factored via `apps/macos/bundle-lib.sh` + `scripts/lib/common.sh` (the source of truth). This skill runs them — **don't duplicate build logic**.

**Prereqs:** Xcode + command-line tools · Rust with `aarch64-apple-darwin` (and `x86_64-apple-darwin` for the Intel slice) · `brew install imagemagick librsvg` (DMG background art). Signing/notarization needs an Apple Developer ID cert + an app-specific password (see `docs/signing.md`); without them, builds fall back to ad-hoc/unsigned.

The macOS app **hosts the engine in-process** (`ds-core` C ABI) — there is no standalone daemon. TCC grants (Accessibility / Mic / Input Monitoring) attach to `DontSpeak.app`.

## Use case 1 — local clean build + reinstall (dev)

```bash
apps/macos/bundle.sh
```
This runs the whole dev install: `install-daemon.sh` (engine + helper bins → `~/.local/bin`, stable-signed) → `build.sh` (Rust `release-ffi` staticlib + `swift build`) → compile the icon → assemble + codesign **`~/Applications/DontSpeak.app`** (override `DONTSPEAK_APP_DIR`).

Then launch (registers the login item + starts the in-process engine):
```bash
open "$HOME/Applications/DontSpeak.app"
```

- For a **clean** install, run `scripts/uninstall.sh` first (add `--reset-permissions` to also clear TCC so the OS re-prompts). Quit the app before reinstalling so files aren't in use.
- **Gotcha** (`install-daemon.sh` header): a HELPER or ENGINE change is NOT live until a full `bundle.sh` — the running app spawns its OWN bundled `Contents/MacOS/ds-helper` and runs the engine in-process. Only HOOK/MCP changes in the `dontspeak` bin go live via `install-daemon.sh` alone.

## Use case 2 — build a distributable package

```bash
apps/macos/dist-dmgs.sh
```
- Output: **`~/Desktop/DontSpeak-<arch>.dmg`** (override `OUTDIR`). One styled drag-to-`/Applications` DMG per arch.
- `DONTSPEAK_ARCHES` — default `arm64`; set `"arm64 x86_64"` for both slices (the Intel slice ships without the Apple-Silicon-only Core ML shim).
- `DONTSPEAK_DIST` — default **`1`** = hardened-runtime Developer-ID sign + notarize + staple (needs the Apple creds). Set `DONTSPEAK_DIST=0` for a local ad-hoc unsigned DMG (first launch hits Gatekeeper).
- Notarize separately (if built unsigned-then-signed): `DONTSPEAK_NOTARY_PROFILE=<profile> apps/macos/notarize.sh ~/Desktop/DontSpeak-arm64.dmg`.
- Sub-scripts: `make-dmg.sh` (styled image) and `notarize.sh` (staple) — `dist-dmgs.sh` orchestrates them.

## Uninstall / clean

```bash
scripts/uninstall.sh                  # remove app + data, leave TCC grants
scripts/uninstall.sh --reset-permissions   # also reset Accessibility/Mic/Input Monitoring
```
Quits the running app + engine, un-wires the Claude Code hooks, deletes the bundle + `~/.local/bin` engine bins + all app data/caches/logs.

## Notes

- The full multi-arch signed release is tag-triggered CI (`release.yml`, `macos-26` runner) — this skill is the fast local path.
- `scripts/install.sh` is the CLI-only path (engine bins + settings snippet, no `.app`) — use the app `bundle.sh` flow for normal dev.
