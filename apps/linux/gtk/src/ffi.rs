//! Safe Rust wrappers over the `ds-core` C ABI — the SAME stable surface the macOS
//! (Swift) and Windows (C#) hosts bind, called directly here (the entry points are
//! `pub extern "C" fn` defined in Rust, so calling them is safe; only their internal
//! pointer derefs are unsafe, encapsulated by core). String returns are owned `*mut c_char`
//! we copy into a Rust `String` and free with `ds_string_free`.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use ds_core::ffi as sys;

/// Copy an owned C string returned by a `ds_*` function into a Rust `String`, then
/// free it through the ABI. NULL → empty string.
fn take(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    sys::ds_string_free(p);
    s
}

// ── Engine lifecycle ─────────────────────────────────────────────────────────
pub fn engine_start() -> bool {
    sys::ds_engine_start() != 0
}
pub fn engine_stop() -> bool {
    sys::ds_engine_stop() != 0
}
#[allow(dead_code)]
pub fn engine_reload() -> bool {
    sys::ds_engine_reload() != 0
}

// ── Control ──────────────────────────────────────────────────────────────────
pub fn set_muted(on: bool) -> bool {
    sys::ds_set_muted(on as u8) != 0
}
#[allow(dead_code)]
pub fn set_provider(which: &str) -> bool {
    let c = CString::new(which).unwrap_or_default();
    sys::ds_set_provider(c.as_ptr()) != 0
}

// ── Status ───────────────────────────────────────────────────────────────────
pub fn model_status_json() -> String {
    take(sys::ds_model_status_json())
}
/// BLOCKING: returns when the status sequence differs from `since` or `timeout_ms` elapses.
/// Call ONLY on a dedicated background thread (never the GTK main thread).
pub fn model_status_wait(since: u64, timeout_ms: u32) -> String {
    take(sys::ds_model_status_wait(since, timeout_ms))
}
pub fn tools_json() -> String {
    take(sys::ds_tools_json())
}
/// The shared cross-platform LIBRARIES catalog (downloaded open-source projects + licenses) —
/// the SAME `ds-model::libraries::catalog` JSON the Windows host renders.
pub fn libraries_json() -> String {
    take(sys::ds_libraries_json())
}
/// The tail of the unified activity log (up to `max_bytes`), for the Logs view. Core exposes
/// the combined log as a JSON array of `{source, level, text}` (`ds_logs_json`, the SAME
/// payload the Windows host parses for per-source coloring); the GTK Logs `TextView` shows
/// plain text, so we flatten it to `"[source] text"` lines here.
pub fn log_tail(max_bytes: u32) -> String {
    let json = take(sys::ds_logs_json(max_bytes));
    match serde_json::from_str::<Vec<serde_json::Value>>(&json) {
        Ok(entries) => entries
            .iter()
            .map(|e| {
                let src = e.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let text = e.get("text").and_then(|v| v.as_str()).unwrap_or("");
                if src.is_empty() {
                    text.to_string()
                } else {
                    format!("[{src}] {text}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        // Fall back to the raw payload if it isn't the expected JSON array.
        Err(_) => json,
    }
}
/// Localized engine lifecycle NOTE ("Downloading 45%", "Starting…", "Failed — <why>", …) for a
/// not-ready engine — the SAME `status_fmt::engine_state_word` the Swift/C# hosts show.
pub fn engine_state_word(state: &str, progress: f64, why: &str) -> String {
    let s = CString::new(state).unwrap_or_default();
    let w = CString::new(why).unwrap_or_default();
    take(sys::ds_engine_state_word(s.as_ptr(), progress, w.as_ptr()))
}

// ── Metadata + i18n ──────────────────────────────────────────────────────────
pub fn version() -> String {
    take(sys::ds_version())
}
pub fn homepage_url() -> String {
    take(sys::ds_homepage_url())
}
pub fn brand_colors_json() -> String {
    take(sys::ds_brand_colors_json())
}
pub fn set_locale(locale: &str) {
    if let Ok(c) = CString::new(locale) {
        sys::ds_set_locale(c.as_ptr());
    }
}
/// Localized string by key (English fallback; missing key returns the key).
pub fn t(key: &str) -> String {
    let c = CString::new(key).unwrap_or_default();
    take(sys::ds_t(c.as_ptr()))
}

// ── Formatters (the shared `status_fmt`, same as the macOS/Windows hosts) ──────
/// Localized lifetime duration down to seconds, leading zero-units dropped
/// (e.g. "12m 04s", "1d 02h 03m 04s"). The SAME `ds-core` formatter macOS and
/// Windows bind — Linux no longer hand-rolls its own divergent copy.
pub fn duration_live(secs: f64) -> String {
    take(sys::ds_duration_live(secs))
}

/// Localized RUNTIME label for a resolved provider token (ort_cpu/ort_cuda/ort_coreml/ane).
/// The SAME shared formatter the macOS/Windows hosts call — no Linux-local mapping.
pub fn runtime_label(provider: &str) -> String {
    let c = CString::new(provider).unwrap_or_default();
    take(sys::ds_runtime_label(c.as_ptr()))
}

/// A stat RANGE string "avg<unit>  ·  lo–hi" (shared formatter; `precision` decimals,
/// `unit_key` = the catalog unit key). The SAME builder the Swift/C# hosts call.
pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: u32, unit_key: &str) -> String {
    let c = CString::new(unit_key).unwrap_or_default();
    take(sys::ds_stats_range(lo, avg, hi, precision, c.as_ptr()))
}

/// A COUNT + audio-duration stat string "<count>  <secs> s" (shared formatter).
pub fn stats_count(count: u64, audio_secs: f64) -> String {
    take(sys::ds_stats_count(count, audio_secs))
}
