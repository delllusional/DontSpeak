# Localization

> **Rule for all new code:** every new user-facing UI string MUST be added to the shared
> catalog (`rust/crates/ds-i18n/locales/en.yml`) and rendered through the FFI — never
> hardcode a literal in Swift, C#, or XAML. One key, one English value, both platforms.
> See [Adding a string](#adding-a-string). (OS-rendered metadata is the only exception —
> see [Native channel](#native-channel-not-in-the-catalog).)

All user-facing UI strings live in **one shared Rust catalog** (`rust/crates/ds-i18n`)
and are rendered by every platform UI over the C ABI. macOS (Swift) and Windows (WinUI)
no longer hold their own copies of near-identical literals — **English is the source of
truth and the fallback**, and macOS wording is canonical where the two platforms had
drifted.

## How it works

- **Catalog:** `ds-i18n` holds `locales/<lang>.yml` (currently `en.yml`), embedded at
  compile time via `rust-i18n` (3.x). The whole catalog ships inside the shared
  `libds_core.a` (macOS) / `ds_core.dll` (Windows) — no external resource
  files to package.
- **Locale:** defaults to the OS language via `sys-locale`, resolved lazily on first
  lookup. Override with `ds_set_locale`.
- **C ABI** (in `ds-core`, declared in `apps/macos/Sources/CDontSpeak/include/dontspeak.h`):
  - `ds_t(key)` → localized string (English fallback; a missing key returns the key)
  - `ds_t_args(key, args_json)` → same, with `%{name}` placeholders filled from a
    JSON object `{ "name": value }`. Numbers are formatted **natively** by each UI
    (culture-aware) and passed in as strings; the sentence template lives in the catalog.
  - `ds_set_locale(bcp47)` / `ds_locale()`
- **macOS** calls these through `L.t(...)` (`apps/macos/Sources/DontSpeak/Localization.swift`).
- **Windows** calls them through `Loc.T(...)` (`apps/windows/winui/Loc.cs`); XAML uses the
  `{loc:Loc Key=...}` markup extension (`apps/windows/winui/LocExtension.cs`).
- **Linux** calls them from the GTK host (`apps/linux/gtk/`) over the same FFI
  (`ds_t` / `ds_set_locale`, via `ffi.rs`).

OS-rendered metadata is **not** in the catalog (it can't come from an FFI call) — see
"Native channel" below.

## Key grouping

Top-level keys are grouped by the **surface** the string is rendered on, so a translator
can work one screen at a time:

- `common` — strings reused on **more than one** surface (the brand name, the nav/tray
  labels, the dash).
- `tray` — the tray / status-bar menu (macOS) and notification-area menu (Windows).
- `status` — the Status screen: its Engines group (`status.engine.*`), the panel chrome
  (`status.caps_lock`, `status.permission.*`, …), and the stats numbers (`status.stats.*`).
- `tools` — the Tools screen.
- `libraries` — the Credits tab (the downloaded models/runtimes + their licenses,
  rendered from the shared `ds-model` catalog; nav label key `common.nav_credits`).
- `logs` — the Log tab (the read-only `dontspeak.log` tail; filter/empty/no-match chrome;
  nav label key `common.nav_log`).

## Key classes

The catalog mixes three kinds of keys, by design:

- **shared** — most keys (rendered by both platforms).
- **macOS-only** — e.g. the About-screen badges.
- **Windows-only** — e.g. `tray.start_at_login`.

Platform-idiomatic terms that legitimately differ are kept on **separate keys on
purpose**, not force-merged: `tray.quit` = "Quit" (macOS) vs `tray.exit` = "Exit"
(Windows).

Conversely, strings that are **semantically the same and shown in the same role** use
**one key**, even across platforms or panel sections — never a second copy that can drift:

- **Product name** → `common.app_name` ("DontSpeak"). It is also the "DontSpeak" status
  row title on both platforms (no separate `status.engine_title`). *(The Rust
  `ds-config::brand::DISPLAY_NAME` const is the parallel single source for the
  NON-catalog native surfaces — installer DisplayName, the SessionStart banner — which
  can't call the FFI.)*
- **Screen labels** → `common.nav_status` / `common.nav_tools` ("Status" / "Tools"). Shared
  by the Windows NavigationView tabs **and** the macOS tray menu items (same word, same
  destination). Only the platform-idiomatic *actions* (`tray.quit` vs `tray.exit`) split.
- **TTS / STT** → `status.engine.role_tts` / `status.engine.role_stt`. The lifetime-usage
  labels reuse these (no separate `status.stats.lifetime_tts` / `_stt`); only
  `status.stats.lifetime_all_time` is stats-specific.

## Adding a string

1. Add the key + English value to `rust/crates/ds-i18n/locales/en.yml`, under the
   group for its surface (nested YAML; `status: { engine: { role_tts: ... } }` flattens to
   `status.engine.role_tts`).
2. Use it: macOS `L.t("status.engine.role_tts")` / Windows `Loc.T("status.engine.role_tts")`
   / XAML `{loc:Loc Key=status.engine.role_tts}`.

> **Build gotcha:** `rust-i18n` embeds the YAML at compile time and does not always
> re-run when *only* a `.yml` changes. After editing `en.yml`, force a re-embed with
> `cargo clean -p ds-i18n` (or touch `ds-i18n/src/lib.rs`) before rebuilding.

## Adding a language

Drop a new `locales/<lang>.yml` (same keys), `cargo clean -p ds-i18n`, rebuild. No code
change. Anything missing falls back to English. (Note: plurals are currently English-
style flat forms — same as the pre-migration code; CLDR plural rules would be a later
move to Fluent if richer grammar is needed.)

## Native channel (NOT in the catalog)

OS-rendered metadata stays in each platform's native resources:

- **macOS `Info.plist`** — `CFBundleName`/`CFBundleDisplayName` and the two
  `NS*UsageDescription` TCC prompts. To localize: add `Contents/Resources/en.lproj/
  InfoPlist.strings` **and** `CFBundleDevelopmentRegion` to `Info.plist` (currently
  absent), and have `bundle-lib.sh` assemble the `.lproj` (the app is a hand-built
  SwiftPM bundle, not Xcode). *Not yet done — tracked below.*
- **Windows** — `app.manifest` identity, registry Run-key, window-class/IPC names.
  Fixed identity, never localized. (A localized Store display name would use `.resw`
  `ms-resource`, parallel to `InfoPlist.strings`.)

## Status / follow-ups

- ✅ **Rust foundation** (`ds-i18n` + FFI) and **macOS** migration — done, builds/tests green.
- ✅ **WinUI foundation** — `Loc.cs`, `LocExtension.cs`, `Native.cs` engine-state words
  (reconciled onto macOS wording), tray menu.
- ✅ **WinUI Status surface** — name/version/usage via `Loc.T` (version + lifetime usage
  live under the Status "DontSpeak" row; there is no separate About surface), and the two
  nav tab labels via `{loc:Loc Key=common.nav_status}` / `common.nav_tools`.
- ✅ **WinUI literals migrated** (the mechanical `Loc.T` / `{loc:Loc}` swaps; keys in
  `en.yml`. Still wants a Windows build to verify on-device):
  - `MainWindow.xaml` — nav, section headers, stat labels via `{loc:Loc Key=...}`.
  - `MainWindow.xaml.cs` — `running`/`stopped`, `on`/`off`, the unloaded/empty-state
    blurbs, the stats lines (`status.stats.spoken` / `transcribed`), `Download`/
    `Retry`, `required`/`optional`/`any` — all via `Loc.T(...)`.
  - `App.xaml.cs` — the tray balloon (`tray.hint_tray_title` / `tray.hint_tray_body`).
- ⏳ **macOS `InfoPlist.strings`** + `CFBundleDevelopmentRegion` + `bundle-lib.sh` step
  (only needed once a non-English locale is added).
