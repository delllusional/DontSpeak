---
name: build-macos
description: Uninstall / build+install / package DontSpeak on macOS. Three flows — (1) uninstall/clean, (2) local build + reinstall of DontSpeak.app for dev testing, (3) build the distributable .app.zip (signed + notarized when Apple creds are set). Use when asked to build, reinstall, package, uninstall, or cut the macOS app. Runs on macOS only.
---

# DontSpeak — macOS (uninstall / install / package)

> **Runs on:** macOS only (bash + Xcode + `codesign`/`notarytool`; Apple Silicon for the arm64 slice). **Working dir:** repo root. Same three flows as `build-windows` / `build-linux`.

The app **hosts the engine in-process** (`ds-core` C ABI) — no separate daemon. TCC grants (Accessibility / Mic / Speech Recognition) attach to `DontSpeak.app`. Scripts live under `apps/macos/` + `scripts/`, factored via `apps/macos/bundle-lib.sh` + `scripts/lib/common.sh` — **don't duplicate build logic; edit the scripts**.

**Prereqs:** Xcode + CLT (Xcode 26 — `AppIcon.icon` needs SDK 26 to compile) · Rust with `aarch64-apple-darwin` (+ `x86_64-apple-darwin` for the Intel slice) · **`librsvg`** (`brew install librsvg`) for the `rsvg-convert` used to render the menu-bar glyph AND the legacy app-icon `.icns` (see **App icon** below — without it the icon degrades on macOS < 26). Signing/notarization needs a Developer ID cert + app-specific password (`docs/SIGNING.md`); without them builds fall back to ad-hoc/unsigned.

### App icon — dual path (macOS 26 Liquid Glass + everything older)

`bundle-lib.sh:compile_icon` compiles `apps/macos/AppIcon.icon` (Icon Composer) into **two** representations, and the app ships **both**:

- **`Assets.car`** (`CFBundleIconName=AppIcon`) — `actool`'s macOS-26 Liquid Glass rendition. Only macOS 26+ can read it.
- **`AppIcon.icns`** (`CFBundleIconFile=AppIcon`) — the fallback for macOS ≤ 25. `actool`'s own `.icns` is a **stub** (16px + 128px only), so `legacy_icns()` overwrites it with a **complete 10-size** `.icns` rendered from the master `assets/app-icon.svg` via `rsvg-convert` → `iconutil`.

Ship only `actool`'s output and macOS 14/15 get no readable Assets.car AppIcon *and* a near-empty `.icns` → Finder/Dock fall back to a blurry/generic icon (this shipped in v0.1.0 on Intel/Sequoia). If `rsvg-convert` is missing, the build **warns and keeps the stub** (degraded icon) rather than failing — so `librsvg` is a real prereq for a correct release. `LSUIElement=true` means DontSpeak is a menu-bar agent with no Dock icon by design; the Finder/Get-Info icon is what this fixes.

## 1 — Uninstall / clean

```bash
scripts/uninstall.sh   # remove app + data; ALWAYS resets this app's TCC grants
```
Quits the app + engine, un-wires all clients, deletes the app bundle (`~/Applications` — the ONE per-user layout both the dev and release flows install to; `DONTSPEAK_APP_DIR` honored) + `~/.local/bin` engine bins + all data/caches/logs. Always resets this app's TCC grants — Accessibility, Microphone, and Speech Recognition (the three it actually requests) — via `tccutil reset <svc> app.dontspeak.org`, so a reinstall re-prompts cleanly instead of inheriting a stale, pre-selected Privacy & Security entry, which after a signature change shows enabled but silently fails. The self-signed `DontSpeak Local Dev` cert is left in place (keeps the signature stable so the re-granted permission sticks across rebuilds). This IS the canonical uninstaller (also embedded in `web/install.sh`; `packaging_sync.rs` pins the copies byte-for-byte). Quit the app first so files aren't in use.

## 2 — Build + install (dev)

```bash
apps/macos/bundle.sh
open "$HOME/Applications/DontSpeak.app"    # launch: registers login item, starts the engine
```
`bundle.sh` does the whole dev install: `install-daemon.sh` (engine + helper bins → `~/.local/bin`, stable-signed, places `dontspeak-uninstall`) → `dontspeak wire --reconcile` (converges every registry client to config.toml's `exclude_clients`, absent ⇒ all: Claude Code hooks + MCP, Codex hooks, Qwen Code hooks + MCP) → `build.sh` (Rust `release-ffi` staticlib + `swift build`) → icon compile → assemble + codesign **`~/Applications/DontSpeak.app`** (`DONTSPEAK_APP_DIR` overrides; the uninstaller honors the same). Release installs (`web/install.sh`) use the SAME per-user location — one layout, no `/Applications` copy to fight over the login item, the wire target, or TCC.

- **Gotcha:** a helper or engine change is NOT live until a full `bundle.sh` — the app spawns its OWN bundled `ds-helper` and runs the engine in-process. Only hook/MCP changes in the `dontspeak` bin go live via `install-daemon.sh` alone.
- `scripts/install.sh` is the CLI-only path (engine bins, then `dontspeak wire --reconcile`; no `.app`) — normal dev uses `bundle.sh`.

## 3 — Build the package

```bash
apps/macos/dist-apps.sh
```
- Output: **`~/Desktop/dontspeak-<version>-macos-<aarch64|x86_64>.app.zip`** (`OUTDIR` overrides) — a signed `DontSpeak.app` zip per arch (notarized + stapled when notary creds are set); `web/install.sh` unzips it into `~/Applications`.
- `DONTSPEAK_ARCHES` — default `arm64`; `"arm64 x86_64"` for both (the Intel slice ships without the Apple-Silicon-only Core ML shim).
- `DONTSPEAK_DIST` — default `1`: requires a Developer ID Application identity and hardened-runtime-signs with it (fails fast without one). `0` = local ad-hoc unsigned zip (first launch hits Gatekeeper).
- Notarization is gated separately on credentials: set `DONTSPEAK_NOTARY_PROFILE` or the `DONTSPEAK_APPLE_ID`/`DONTSPEAK_TEAM_ID`/`DONTSPEAK_APP_PASSWORD` trio, else the zip ships signed but un-notarized.
- Notarize a pre-built app separately: `DONTSPEAK_NOTARY_PROFILE=<profile> apps/macos/notarize.sh <path>/DontSpeak.app`.

## Notes

- The full multi-arch signed release is tag-triggered CI (`release.yml`, `macos-26` runner) — this skill is the fast local path.
