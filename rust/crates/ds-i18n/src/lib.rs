//! ds-i18n — the shared DontSpeak localization catalog.
//!
//! User-facing strings live here as YAML (`locales/*.yml`); **English is the source of
//! truth and the fallback**. They are looked up at runtime via `rust-i18n` and reached
//! from every platform UI through ds-core's C ABI (`ds_t` / `ds_t_args`
//! / `ds_set_locale` / `ds_locale`), so macOS (Swift) and Windows (WinUI)
//! render ONE catalog instead of duplicating near-identical literals.
//!
//! Scope: only **app-rendered** strings belong here. OS-rendered metadata — macOS
//! `Info.plist` usage descriptions, the Windows app manifest — stays in each platform's
//! native resources (it can't be served over the FFI).
//!
//! Keys are mostly **shared**; a few are **Windows-only** (`tray.start_at_login`, the
//! taskbar hints). Platform-idiomatic terms that legitimately differ are kept on distinct
//! keys on purpose (`tray.quit` = "Quit" on macOS, `tray.exit` = "Exit" on Windows), NOT
//! force-merged.

use std::sync::Once;

rust_i18n::i18n!("locales", fallback = "en");

// First touch wins: either an explicit `set_locale` from the UI, or OS detection. Both
// consume this Once so a later lookup can't re-run OS detection and clobber a UI choice.
static INIT: Once = Once::new();

/// Make the OS language the active locale (best-effort); English is the fallback for any
/// locale or key we don't have. Runs at most once, lazily, on the first lookup.
fn init_from_os() {
    if let Some(loc) = sys_locale::get_locale() {
        // rust-i18n matches on the language subtag, e.g. "de-DE" → "de".
        let lang = loc.split(['-', '_']).next().unwrap_or("en");
        rust_i18n::set_locale(lang);
    }
}

fn ensure_init() {
    INIT.call_once(init_from_os);
}

/// Set the active locale (BCP-47 or a bare language tag). Unknown locales fall back to
/// English at lookup time. Marks initialization done so OS detection won't override it.
pub fn set_locale(locale: &str) {
    INIT.call_once(|| {});
    rust_i18n::set_locale(locale);
}

/// The active locale tag — so a UI's native number formatter can match the catalog's
/// language when it formats values to inject via [`t_args_json`].
pub fn locale() -> String {
    ensure_init();
    rust_i18n::locale().to_string()
}

/// Look up `key` in the active locale (English fallback). A missing key returns the key
/// itself, so a gap is visible rather than blank.
pub fn t(key: &str) -> String {
    ensure_init();
    rust_i18n::t!(key).to_string()
}

/// Look up `key` and interpolate `%{name}` placeholders from a JSON object of
/// `{ "name": value }` (values may be strings or numbers). Used for the parameterized
/// strings; the caller formats numbers (locale-aware, natively) and passes them in, so
/// number formatting can stay native while the sentence template lives in the catalog.
pub fn t_args_json(key: &str, args_json: &str) -> String {
    let s = t(key);
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(args_json)
    {
        // Reuse rust-i18n's own `%{name}` interpolator: it walks the template once,
        // substitutes each `%{key}`, and leaves unknown placeholders intact. Numbers/bools
        // stringify via their JSON form; strings pass through unquoted.
        let mut patterns: Vec<&str> = Vec::with_capacity(map.len());
        let mut values: Vec<String> = Vec::with_capacity(map.len());
        for (k, v) in &map {
            patterns.push(k.as_str());
            values.push(match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
        return rust_i18n::replace_patterns(&s, &patterns, &values);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_lookup_and_fallback() {
        set_locale("en");
        assert_eq!(t("tray.quit"), "Quit");
        assert_eq!(t("common.nav_status"), "Status");
        // A missing key returns the key itself (visible gap, not blank).
        assert_eq!(t("nope.not.here"), "nope.not.here");
    }

    #[test]
    fn interpolates_named_args() {
        set_locale("en");
        // String placeholder (a live key — `about.version` was retired when the About
        // surface moved into the Status row; the version number is supplied separately).
        assert_eq!(
            t_args_json("status.engine.status.failed", r#"{"why":"no model"}"#),
            "Failed — no model"
        );
        // Numbers stringify; missing placeholders are left intact.
        assert_eq!(
            t_args_json("status.engine.status.downloading", r#"{"pct":42}"#),
            "Downloading 42%"
        );
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        set_locale("xx");
        assert_eq!(t("tray.quit"), "Quit");
    }
}
