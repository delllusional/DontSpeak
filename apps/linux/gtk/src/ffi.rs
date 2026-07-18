//! Safe Rust wrappers over the `ds-core` C ABI — the same surface the macOS (Swift) and
//! Windows (C#) hosts bind. Entry points are `pub extern "C" fn` in Rust (safe to call);
//! owned `*mut c_char` returns are copied into `String` and freed with `ds_string_free`.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use ds_core::ffi as sys;
use serde::Deserialize;

/// Typed Usage deck decoded immediately at the C ABI boundary.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct UsageDeck {
    pub(crate) cards: Vec<UsageCard>,
}

/// Typed per-agent last-good value consumed by the GTK view.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct UsageCard {
    pub(crate) agent: String,
    /// Signed-in account (usually email) when the client exposes one.
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

/// Copy an owned C string from a `ds_*` return into a Rust `String`, then free it. NULL → "".
fn take(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is non-null; every `ds_*` return is either NULL or a valid NUL-terminated
    // string owned by `ds-core` until `ds_string_free`. Copy before free.
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
// BLOCKING (same ds_ipc round-trip as wait). No longer called from on_activate (see main.rs);
// kept for parity with the other hosts' one-shot status fetch.
#[allow(dead_code)]
pub fn model_status_json() -> String {
    take(sys::ds_model_status_json())
}
/// BLOCKING until status seq differs from `since` or `timeout_ms`. Background thread only.
pub fn model_status_wait(since: u64, timeout_ms: u32) -> String {
    take(sys::ds_model_status_wait(since, timeout_ms))
}
/// Instant typed deck: installed agent cards + last-good cache. No network.
pub fn agent_usage_skeleton() -> Option<UsageDeck> {
    serde_json::from_str(&take(sys::ds_agent_usage_skeleton_json())).ok()
}

/// Blocking typed single-card load. Background thread; force bypasses 60s soft cache.
pub fn agent_usage_card(agent: &str, force_refresh: bool) -> Option<UsageCard> {
    let c = CString::new(agent).unwrap_or_default();
    serde_json::from_str(&take(sys::ds_agent_usage_card_json(
        c.as_ptr(),
        force_refresh as u8,
    )))
    .ok()
}
pub fn tools_json() -> String {
    take(sys::ds_tools_json())
}
/// Shared libraries catalog (`ds-model::libraries::catalog`) — same JSON Windows renders.
pub fn libraries_json() -> String {
    take(sys::ds_libraries_json())
}
/// Raw activity-log JSON tail (`[{source,level,text},…]`). Push path keeps JSON so the UI
/// can filter with shared [`ds_log`] rules before flattening.
pub fn log_tail_json(max_bytes: u32) -> String {
    take(sys::ds_logs_json(max_bytes))
}

/// Like [`log_tail_json`] but BLOCKS until any `*.log` under the logs dir changes or
/// `timeout_ms` elapses. Client-side fs watch (not engine IPC). Background thread only.
pub fn log_wait_json(max_bytes: u32, timeout_ms: u32) -> String {
    take(sys::ds_logs_wait(max_bytes, timeout_ms))
}

/// Parse + filter combined-log JSON with shared [`ds_log`] rules, then flatten for the text view.
/// Empty result distinguishes no lines vs no match via the optional out-params.
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
/// Erase the on-disk activity log (unified + rotated + aux). Irreversible — confirm first
/// (AdwAlertDialog in `ui.rs`).
pub fn logs_clear() {
    sys::ds_logs_clear();
}
/// Localized lifecycle note for a not-ready engine (`status_fmt::engine_state_word`).
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
/// Startup update check (`ds_update_check_json`): blocking HTTP to GitHub. Background thread
/// only (see `main.rs`). `"{}"` on any failure; missing `update_available` ⇒ false.
pub fn update_check_json() -> String {
    take(sys::ds_update_check_json())
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
/// Localized string with `%{name}` placeholders via `ds_t_args` (same as macOS/Windows).
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

// ── Formatters (`status_fmt` — shared with macOS/Windows) ────────────────────
/// Duration with leading+trailing zero units dropped (e.g. "12m 04s", "1d 05h").
pub fn duration_live(secs: f64) -> String {
    take(sys::ds_duration_live(secs))
}

/// Usage remaining duration from UTC epoch (e.g. "2d 05h"; no seconds).
pub fn usage_resets_in(resets_at_unix: i64) -> String {
    take(sys::ds_usage_resets_in(resets_at_unix))
}

/// Runtime label for a resolved provider token (cpu/cuda/coreml/ane).
pub fn runtime_label(provider: &str) -> String {
    let c = CString::new(provider).unwrap_or_default();
    take(sys::ds_runtime_label(c.as_ptr()))
}

/// Stat range `"avg<unit>  ·  lo–hi"` (`precision` decimals; `unit_key` = catalog unit key).
pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: u32, unit_key: &str) -> String {
    let c = CString::new(unit_key).unwrap_or_default();
    take(sys::ds_stats_range(lo, avg, hi, precision, c.as_ptr()))
}

/// Count + audio-duration stat `"<count>  <secs> s"`.
pub fn stats_count(count: u64, audio_secs: f64) -> String {
    take(sys::ds_stats_count(count, audio_secs))
}

/// Decimal file size ("325 MB" / "12 KB") — same builder every Libraries tab uses.
pub fn human_size(bytes: u64) -> String {
    take(sys::ds_human_size(bytes))
}
