---
name: build-macos
description: Uninstall / build+install / package DontSpeak on macOS. Three flows — (1) uninstall/clean, (2) local build + reinstall of DontSpeak.app for dev testing, (3) build the distributable .app.zip (signed + notarized when Apple creds are set). Use when asked to build, reinstall, package, uninstall, or cut the macOS app. Runs on macOS only.
---

# DontSpeak — macOS (uninstall / install / package)

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

macOS only, repo root. In-process engine via `ds-core`; TCC on `DontSpeak.app`.
Scripts under `apps/macos/` + `scripts/` — edit those, don't duplicate.

**Prereqs:** Xcode + CLT (Xcode 26 for `AppIcon.icon` / SDK 26) · Rust
`aarch64-apple-darwin` (+ `x86_64-apple-darwin` for Intel) · **`librsvg`**
(`brew install librsvg`) for menu-bar glyph + legacy `.icns`. Signing: [SIGNING.md](../../../docs/SIGNING.md).

### App icon (dual path)

`bundle-lib.sh:compile_icon` ships both:

- **`Assets.car`** — Liquid Glass (macOS 26+)
- **`AppIcon.icns`** — complete 10-size fallback for ≤25 (actool's `.icns` is a stub;
  `legacy_icns()` rebuilds from `assets/app-icon.svg` via `rsvg-convert` → `iconutil`)

Missing `rsvg-convert` → warn + keep stub (bad Finder icon). `LSUIElement=true` =
menu-bar agent (no Dock icon by design).

## 1 — Uninstall

```bash
scripts/install/bundle/uninstall.sh
```

Quits app, un-wires clients, removes `~/Applications/DontSpeak.app` (or
`DONTSPEAK_APP_DIR`), `~/.local/bin` bins, data/caches/logs. **Always** resets this
app's TCC (Accessibility, Microphone, Speech Recognition) via `tccutil` so reinstall
re-prompts cleanly after signature change. Keeps `DontSpeak Local Dev` cert.

## 2 — Build + install (dev)

```bash
apps/macos/bundle.sh
open "$HOME/Applications/DontSpeak.app"
```

`bundle.sh`: install-engine → `dontspeak wire --reconcile` (all four clients unless
`exclude_clients`: Claude, Codex, Qwen, Grok — hooks + MCP as registered) → build +
icon + codesign app. Host also re-reconciles at boot. Same layout as release
(`~/Applications`).

- Engine/helper changes need full `bundle.sh` (app uses bundled helper + in-process engine).
- CLI-only: `scripts/install/local/install.sh` (no `.app`).

## 3 — Package

```bash
apps/macos/dist-apps.sh
```

- Output: `~/Desktop/dontspeak-<version>-macos-<arch>.app.zip` (`OUTDIR` overrides)
- `DONTSPEAK_ARCHES` default `arm64`; `"arm64 x86_64"` for both (Intel without Core ML shim)
- `DONTSPEAK_DIST=1` (default): requires Developer ID + hardened runtime; `0` = ad-hoc
- Notarize if `DONTSPEAK_NOTARY_PROFILE` or Apple ID/team/password trio set
- Standalone: `DONTSPEAK_NOTARY_PROFILE=… apps/macos/notarize.sh <path>/DontSpeak.app`

Full multi-arch release = tag CI (`macos-26`).
