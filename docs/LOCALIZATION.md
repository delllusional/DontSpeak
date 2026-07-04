# Localization

All user-facing UI strings live in one shared Rust catalog (`ds-i18n`) and are
rendered by every platform UI over the `ds-core` C ABI — one key, one English value,
read by all three apps. English is the source of truth and the fallback; macOS
wording is canonical where the two platforms had drifted. Keeping the catalog in Rust
rather than per-platform resource files means a translation is written once and
guaranteed to match across macOS, Windows, and Linux.

## How it works

`ds-i18n` holds `locales/<lang>.yml` (currently `en.yml`), embedded at compile time
via `rust-i18n` and shipped inside the shared `ds_core` library — no external
resource files to package. The active locale defaults to the OS language (via
`sys-locale`) and can be overridden with `ds_set_locale`.

The C ABI exposes:

- `ds_t(key)` — localized string (English fallback; a missing key returns the key).
- `ds_t_args(key, args_json)` — same, with `%{name}` placeholders filled from a JSON
  object. Numbers are formatted natively by each UI (culture-aware) and passed in as
  strings; only the sentence template lives in the catalog.
- `ds_set_locale(bcp47)` / `ds_locale()`.

Each platform wraps these thinly: macOS through `L.t(...)`
(`apps/macos/Sources/DontSpeak/Localization.swift`), Windows through `Loc.T(...)`
and the `{loc:Loc Key=...}` XAML markup extension (`apps/windows/winui/Loc.cs` /
`LocExtension.cs`), and Linux from the GTK host over the same `ds_t` FFI calls.

## Key grouping

Top-level keys are grouped by the surface they render on (`common`, `tray`,
`status`, `tools`, `libraries`, `logs`, …), so a translator can work one screen at a
time. Strings that are semantically the same and shown in the same role — the
product name, screen labels, TTS/STT role labels — share a single key across
platforms rather than duplicating per-OS copies that could drift. Platform-idiomatic
terms that legitimately differ get separate keys on purpose (`tray.quit` vs
`tray.exit`).

## Adding a string

1. Add the key and English value to `rust/crates/ds-i18n/locales/en.yml`, nested
   under the group for its surface (e.g. `status: { engine: { role_tts: ... } }`
   flattens to `status.engine.role_tts`).
2. Use it: macOS `L.t("status.engine.role_tts")`, Windows `Loc.T("status.engine.role_tts")`,
   or XAML `{loc:Loc Key=status.engine.role_tts}`.

`rust-i18n` embeds the YAML at compile time and doesn't always re-run when only a
`.yml` changes — after editing `en.yml`, force a re-embed with
`cargo clean -p ds-i18n` (or touch `ds-i18n/src/lib.rs`) before rebuilding.

## Adding a language

Drop a new `locales/<lang>.yml` with the same keys, `cargo clean -p ds-i18n`, and
rebuild — no code change needed. Any key missing from the new file falls back to
English.

## Native channel

A few OS-rendered strings can't come from an FFI call and stay in each platform's
native resources instead: macOS `Info.plist` (`CFBundleName`/`CFBundleDisplayName`,
the TCC usage-description prompts) and Windows `app.manifest` identity, the registry
Run-key, and window-class/IPC names. These are localized through the platform's own
resource mechanism (`.lproj` / `.resw`) if and when a non-English locale ships.

The Windows `ds_core.dll`-load-failure `MessageBoxW` in `apps/windows/winui/App.xaml.cs`
(`App.DllLoadFailureMessage`) is a similar necessary exception: it fires from the
`NativeLibrary.TryLoad("ds_core.dll")` failure branch in `OnLaunched`, before `ds_core.dll` —
and therefore `Loc.T`, which P/Invokes into it — is reachable at all. It stays a hardcoded,
English-only string with a `CultureInfo`-keyed extension point rather than a real catalog
lookup, since there's nothing else to localize it against at that point in startup.
