//! Safe wrappers over the `ds-core` C ABI (same surface as macOS/Windows hosts).
//! Owned `*mut c_char` returns are copied into `String` and freed with `ds_string_free`.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use ds_core::ffi as sys;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct UsageDeck {
    pub(crate) cards: Vec<UsageCard>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct UsageCard {
    pub(crate) agent: String,
    /// Signed-in account when the client exposes one.
    #[serde(default)]
    pub(crate) account: Option<String>,
    pub(crate) rows: Vec<UsageRow>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct UsageRow {
    pub(crate) period: String,
    pub(crate) used_percent: f64,
    pub(crate) resets_at_unix: i64,
}

/// Owned `ds_*` C string → Rust `String` + free. NULL → `""`.
fn take(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null; every ds_* return is NULL or a valid NUL-terminated string
    // owned by ds-core until ds_string_free. Copy before free.
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    sys::ds_string_free(p);
    s
}

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

pub fn set_muted(on: bool) -> bool {
    sys::ds_set_muted(on as u8) != 0
}
#[allow(dead_code)]
pub fn set_provider(which: &str) -> bool {
    let c = CString::new(which).unwrap_or_default();
    sys::ds_set_provider(c.as_ptr()) != 0
}

// BLOCKING (same ds_ipc round-trip as wait). Host-parity only — GTK main thread must not call.
#[allow(dead_code)]
pub fn model_status_json() -> String {
    take(sys::ds_model_status_json())
}
/// BLOCKING until status seq ≠ `since` or timeout. Background thread only.
pub fn model_status_wait(since: u64, timeout_ms: u32) -> String {
    take(sys::ds_model_status_wait(since, timeout_ms))
}
/// Instant deck: installed agents + last-good cache (local only).
pub fn agent_usage_skeleton() -> Option<UsageDeck> {
    serde_json::from_str(&take(sys::ds_agent_usage_skeleton_json())).ok()
}

/// BLOCKING single-card load. Background thread; force bypasses 60s soft cache.
pub fn agent_usage_card(agent: &str, refresh: bool) -> Option<UsageCard> {
    let c = CString::new(agent).unwrap_or_default();
    serde_json::from_str(&take(sys::ds_agent_usage_card_json(
        c.as_ptr(),
        refresh as u8,
    )))
    .ok()
}
/// BLOCKING aggregate deck refresh (diagnostics).
#[allow(dead_code)]
pub fn agent_usage(refresh: bool) -> Option<UsageDeck> {
    serde_json::from_str(&take(sys::ds_agent_usage_json(refresh as u8))).ok()
}
pub fn tools_json() -> String {
    take(sys::ds_tools_json())
}
/// Shared libraries catalog (ds-model) — same JSON every host renders.
pub fn libraries_json() -> String {
    take(sys::ds_libraries_json())
}
/// Activity-log JSON tail. Keep JSON so UI can filter with shared ds_log rules.
pub fn log_tail_json(max_bytes: u32) -> String {
    take(sys::ds_logs_json(max_bytes))
}

/// BLOCKING on log-dir change or timeout (client fs watch). Background thread only.
pub fn log_wait_json(max_bytes: u32, timeout_ms: u32) -> String {
    take(sys::ds_logs_wait(max_bytes, timeout_ms))
}

/// Parse + filter via ds_log; returns (total, shown, flat).
pub fn filter_and_flatten_logs(json: &str, query: &str) -> (usize, usize, String) {
    let lines = ds_log::parse_logs_json(json);
    let total = lines.len();
    let filtered: Vec<ds_log::LogLine> = ds_log::filter_logs(&lines, query)
        .into_iter()
        .map(|(_, l)| l.clone())
        .collect();
    let shown = filtered.len();
    (total, shown, ds_log::flatten_log_lines(&filtered))
}
/// Erase on-disk activity log. Irreversible — confirm first.
pub fn logs_clear() {
    sys::ds_logs_clear();
}
pub fn engine_state_word(state: &str, progress: f64, why: &str) -> String {
    let s = CString::new(state).unwrap_or_default();
    let w = CString::new(why).unwrap_or_default();
    take(sys::ds_engine_state_word(s.as_ptr(), progress, w.as_ptr()))
}

pub fn version() -> String {
    take(sys::ds_version())
}
pub fn homepage_url() -> String {
    take(sys::ds_homepage_url())
}
pub fn brand_colors_json() -> String {
    take(sys::ds_brand_colors_json())
}
/// One random Usage speaking wash `{"r","g","b","a"}` (shared recipe).
pub fn random_pastel_wash_json() -> String {
    take(sys::ds_random_pastel_wash_json())
}
/// BLOCKING HTTP to GitHub. Background thread only. `"{}"` on failure.
pub fn update_check_json() -> String {
    take(sys::ds_update_check_json())
}
pub fn set_locale(locale: &str) {
    if let Ok(c) = CString::new(locale) {
        sys::ds_set_locale(c.as_ptr());
    }
}
/// Localized string (missing key returns the key).
pub fn t(key: &str) -> String {
    let c = CString::new(key).unwrap_or_default();
    take(sys::ds_t(c.as_ptr()))
}
/// Localized string with `%{name}` placeholders via `ds_t_args`.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let key_c = CString::new(key).unwrap_or_default();
    let mut obj = serde_json::Map::with_capacity(args.len());
    for (k, v) in args {
        obj.insert(
            (*k).to_string(),
            serde_json::Value::String((*v).to_string()),
        );
    }
    let args_json =
        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".into());
    let args_c = CString::new(args_json).unwrap_or_default();
    take(sys::ds_t_args(key_c.as_ptr(), args_c.as_ptr()))
}

// Shared status_fmt builders (macOS/Windows parity).
pub fn duration_live(secs: f64) -> String {
    take(sys::ds_duration_live(secs))
}

pub fn usage_resets_in(resets_at_unix: i64) -> String {
    take(sys::ds_usage_resets_in(resets_at_unix))
}

pub fn runtime_label(provider: &str) -> String {
    let c = CString::new(provider).unwrap_or_default();
    take(sys::ds_runtime_label(c.as_ptr()))
}

pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: u32, unit_key: &str) -> String {
    let c = CString::new(unit_key).unwrap_or_default();
    take(sys::ds_stats_range(lo, avg, hi, precision, c.as_ptr()))
}

pub fn stats_count(count: u64, audio_secs: f64) -> String {
    take(sys::ds_stats_count(count, audio_secs))
}

pub fn human_size(bytes: u64) -> String {
    take(sys::ds_human_size(bytes))
}
