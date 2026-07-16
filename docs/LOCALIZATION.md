# Localization

User-facing UI strings live in `ds-i18n` and render over the `ds-core` C ABI. English
is source + fallback; macOS wording is canonical where hosts had drifted.

## How it works

`locales/<lang>.yml` (currently `en.yml`) embedded via `rust-i18n` inside `ds_core`.
Default locale = OS language (`sys-locale`); override with `ds_set_locale`.

C ABI:

- `ds_t(key)` — string (missing key → key itself)
- `ds_t_args(key, args_json)` — `%{name}` from JSON; numbers formatted by each UI as
  strings; templates stay in catalog
- `ds_set_locale(bcp47)` / `ds_locale()`

Wrappers: macOS `L.t(...)`, Windows `Loc.T(...)` / `{loc:Loc Key=...}`, Linux GTK
via same FFI.

## Key grouping

Top-level groups by surface (`common`, `tray`, `status`, `tools`, …). Same role →
one key across platforms. Idiomatic platform terms get separate keys (`tray.quit` vs
`tray.exit`).

## Add a string

1. Key + English in `rust/crates/ds-i18n/locales/en.yml` under the surface group
   (`status.engine.role_tts` from nested YAML).
2. Call site: `L.t("…")` / `Loc.T("…")` / XAML markup.

YAML-only edits may not rebuild the embed — `cargo clean -p ds-i18n` (or touch
`ds-i18n/src/lib.rs`) first.

## Add a language

New `locales/<lang>.yml` same keys; clean + rebuild. Missing keys fall back to English.

## Native exceptions

OS-owned strings stay platform-native: macOS `Info.plist` names + TCC prompts;
Windows `app.manifest`, Run-key, window/IPC names — localize via `.lproj` / `.resw`
when a non-English locale ships.

Windows `App.DllLoadFailureMessage` (`MessageBoxW` before `ds_core.dll` loads) is
hardcoded English — `Loc.T` is unreachable until the DLL loads. Extension point is
`CultureInfo`-keyed, not the catalog.
