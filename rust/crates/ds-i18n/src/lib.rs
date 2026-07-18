//! Shared localization catalog.
//!
//! User-facing strings live as YAML (`locales/*.yml`); **English is the source of truth
//! and the fallback**. Looked up via `rust-i18n`, reached from every platform UI through
//! ds-core's C ABI (`ds_t` / `ds_t_args` / `ds_set_locale` / `ds_locale`) — one catalog
//! for macOS and Windows.
//!
//! Scope: **app-rendered** strings only. OS-rendered metadata (Info.plist usage
//! descriptions, Windows app manifest) stays in native resources (can't cross FFI).
//!
//! Keys are mostly shared; a few are Windows-only. Platform-idiomatic terms stay on
//! distinct keys on purpose (`tray.quit` vs `tray.exit`), not force-merged.

use std::sync::Once;

rust_i18n::i18n!("locales", fallback = "en");

// First touch wins: explicit `set_locale` or OS detection. Once so a later lookup
// can't re-run OS detection and clobber a UI choice.
static INIT: Once = Once::new();

/// OS language as active locale (best-effort); English fallback. At most once, lazy.
fn init_from_os() {
    if let Some(loc) = sys_locale::get_locale() {
        // rust-i18n matches language subtag, e.g. "de-DE" → "de".
        let lang = loc.split(['-', '_']).next().unwrap_or("en");
        rust_i18n::set_locale(lang);
    }
}

fn ensure_init() {
    INIT.call_once(init_from_os);
}

/// Set active locale (BCP-47 or bare language tag). Marks init done so OS won't override.
pub fn set_locale(locale: &str) {
    INIT.call_once(|| {});
    rust_i18n::set_locale(locale);
}

/// Active locale tag — so a UI number formatter can match the catalog language.
pub fn locale() -> String {
    ensure_init();
    rust_i18n::locale().to_string()
}

/// Look up `key` (English fallback). Missing key returns the key itself (visible gap).
pub fn t(key: &str) -> String {
    ensure_init();
    rust_i18n::t!(key).to_string()
}

/// Look up `key` and interpolate `%{name}` from a JSON object. Caller formats numbers
/// natively and passes them in — templates stay in the catalog.
pub fn t_args_json(key: &str, args_json: &str) -> String {
    let s = t(key);
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(args_json)
    {
        // rust-i18n's interpolator: unknown placeholders left intact.
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
        // Missing key returns itself (visible gap, not blank).
        assert_eq!(t("nope.not.here"), "nope.not.here");
    }

    #[test]
    fn interpolates_named_args() {
        set_locale("en");
        assert_eq!(
            t_args_json("status.engine.status.failed", r#"{"why":"no model"}"#),
            "Failed — no model"
        );
        // Numbers stringify; missing placeholders left intact.
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

    /// Drift gate: hosts render Usage card titles as `usage.provider.<client_source>`,
    /// so wiring a new client without a catalog entry would surface the raw key in the UI
    /// (`t()` returns the key on a miss).
    #[test]
    fn every_wireable_client_has_a_usage_provider_label() {
        set_locale("en");
        for c in ds_client::ClientSource::CLIENTS {
            let key = format!("usage.provider.{}", c.as_str());
            assert_ne!(t(&key), key, "missing en.yml entry for {key}");
        }
    }
}
