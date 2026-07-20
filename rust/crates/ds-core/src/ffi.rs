// C ABI; pointer validity is the host's. HANDLE-FREE probes + lifecycle (`dontspeak.h`).
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! Stable C ABI for native UI (cbindgen `dontspeak.h`).

use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{engine, models};

// release-ffi: panic=unwind — catch_unwind is a no-op under abort.
#[cfg(panic = "abort")]
compile_error!(
    "ds-core must not be built with panic=\"abort\" -- its extern \"C\" boundary relies on \
     catch_unwind (see guard_val/guard_str below), which is a documented no-op under an abort \
     strategy and would let any panic reachable from an extern \"C\" fn abort the whole hosting \
     process. Build with `cargo build --profile release-ffi -p ds-core` (or any profile with \
     panic=\"unwind\") instead of the default `release` profile."
);

// Lifecycle in [`crate::host`]; thin u8 adapters for `dontspeak.h`.

/// 1 = ok.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_start() -> u8 {
    guard_val(0, || crate::host::engine_start() as u8)
}

/// 1 if was running. Safe on quit.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_stop() -> u8 {
    guard_val(0, || crate::host::engine_stop() as u8)
}

/// 1 if ok.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_reload() -> u8 {
    guard_val(0, || crate::host::engine_reload() as u8)
}

/// Mute (`on != 0`); playback drains. 1 if IPC delivered.
#[unsafe(no_mangle)]
pub extern "C" fn ds_set_muted(on: u8) -> u8 {
    guard_val(0, || {
        let Some(paths) = ds_config::Paths::resolve() else {
            return 0;
        };
        match ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SetMuted { on: on != 0 },
        ) {
            Ok(ds_ipc::Response::Done) => 1,
            _ => 0,
        }
    })
}

/// OS voice settings. 0 on Linux (#74). HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_open_voice_settings() -> u8 {
    guard_val(0, || ds_tts::system::open_voice_settings() as u8)
}

/// Panic across FFI → `default`.
fn guard_val<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

/// Panic path allocates `default` only (eager CString would leak on success).
fn guard_str(default: &'static str, f: impl FnOnce() -> *mut c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| to_cstring(default))
}

/// Free with `ds_string_free`. Interior NUL → "".
fn to_cstring(s: impl Into<Vec<u8>>) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("").unwrap().into_raw(),
    }
}

fn cstr_or_empty(p: *const c_char) -> String {
    cstr_or(p, "")
}

/// `default` for NULL / invalid UTF-8; empty string kept.
fn cstr_or(p: *const c_char, default: &str) -> String {
    if p.is_null() {
        return default.to_string();
    }
    // SAFETY: non-null C ABI — NUL-terminated, valid for call; copied before return.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_str()
        .unwrap_or(default)
        .to_string()
}

/// Kokoro present+valid? Disk probe. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_kokoro_present_global() -> u8 {
    guard_val(0, || models::is_kokoro_present() as u8)
}

/// Full Parakeet-ONNX set present+valid? HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_parakeet_onnx_present_global() -> u8 {
    guard_val(0, || models::is_parakeet_onnx_present() as u8)
}

/// Pidfile probe. HANDLE-FREE, off-main-thread ok.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_running_global() -> u8 {
    guard_val(0, || engine::is_running() as u8)
}

/// Model-status JSON. Owned `char*` (`ds_string_free`); `"{}"` if down. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_model_status_json() -> *mut c_char {
    guard_str("{}", || {
        let Some(paths) = ds_config::Paths::resolve() else {
            return to_cstring("{}");
        };
        match ds_ipc::request(&paths.engine_sock, &ds_ipc::Request::ModelStatus) {
            Ok(ds_ipc::Response::ModelStatus { status }) => to_cstring(status.to_string()),
            _ => to_cstring("{}"),
        }
    })
}

/// Block until `seq` ≠ `since` or timeout. Overlay push: background thread loop
/// (`since = 0` first). Owned `char*`; `"{}"` if down.
#[unsafe(no_mangle)]
pub extern "C" fn ds_model_status_wait(since: u64, timeout_ms: u32) -> *mut c_char {
    guard_str("{}", || {
        let Some(paths) = ds_config::Paths::resolve() else {
            return to_cstring("{}");
        };
        match ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::WaitModelStatus {
                since,
                timeout_ms: timeout_ms as u64,
            },
        ) {
            Ok(ds_ipc::Response::ModelStatus { status }) => to_cstring(status.to_string()),
            _ => to_cstring("{}"),
        }
    })
}

/// Usage skeleton (`ds_agent_usage::skeleton`). No network. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_agent_usage_skeleton_json() -> *mut c_char {
    const EMPTY: &str = r#"{"cards":[]}"#;
    guard_str(EMPTY, || to_cstring(ds_agent_usage::skeleton().to_json()))
}

/// Blocking card refresh (`ClientSource` token). Off UI thread. `refresh` skips soft cache.
/// Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_agent_usage_card_json(agent: *const c_char, refresh: u8) -> *mut c_char {
    const EMPTY: &str = r#"{"agent":"unknown","rows":[]}"#;
    guard_str(EMPTY, || {
        let token = cstr_or_empty(agent);
        let source = ds_agent_usage::parse_agent(&token);
        to_cstring(ds_agent_usage::refresh_card(source, refresh != 0).to_json())
    })
}

/// User-click authorize + force refresh (off UI; may ACL-prompt on macOS). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_agent_usage_card_authorize_json(agent: *const c_char) -> *mut c_char {
    const EMPTY: &str = r#"{"agent":"unknown","rows":[]}"#;
    guard_str(EMPTY, || {
        let token = cstr_or_empty(agent);
        let source = ds_agent_usage::parse_agent(&token);
        to_cstring(ds_agent_usage::authorize_card(source).to_json())
    })
}

/// Aggregate refresh (tests/tooling). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_agent_usage_json(refresh: u8) -> *mut c_char {
    const EMPTY: &str = r#"{"cards":[]}"#;
    guard_str(EMPTY, || {
        to_cstring(ds_agent_usage::snapshot(refresh != 0).to_json())
    })
}

/// Tools-window catalog: JSON array `{name, description, params:[…]}` — ordered params,
/// same `ds-tools` catalog as MCP (no drift). Each param gets localized `detail` via
/// [`crate::status_fmt::tool_param_detail`]. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_tools_json() -> *mut c_char {
    guard_str("[]", || {
        let mut catalog = ds_tools::catalog_ui();
        if let Some(tools) = catalog.as_array_mut() {
            for tool in tools {
                let Some(params) = tool.get_mut("params").and_then(|p| p.as_array_mut()) else {
                    continue;
                };
                for param in params {
                    let detail = crate::status_fmt::tool_param_detail(param);
                    if let Some(obj) = param.as_object_mut() {
                        obj.insert("detail".into(), serde_json::Value::String(detail));
                    }
                }
            }
        }
        to_cstring(catalog.to_string())
    })
}

/// Libraries/credits catalog from the same download registry every platform fetches
/// (models + runtime). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_libraries_json() -> *mut c_char {
    guard_str("[]", || {
        to_cstring(ds_model::libraries::catalog().to_string())
    })
}

/// Combined activity-log tail for Logs tab: `{source, level, text}` merging unified log
/// with sibling aux logs. `max_bytes` per file; shared-read while engine appends.
/// `"[]"` if none. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_logs_json(max_bytes: u32) -> *mut c_char {
    guard_str("[]", || match ds_config::Paths::resolve() {
        Some(paths) => to_cstring(ds_log::combined_log_json(&paths.log_file, max_bytes as u64)),
        None => to_cstring("[]"),
    })
}

/// Like [`ds_logs_json`] but BLOCKS until any `*.log` in the logs dir changes (not
/// rotated `*.log.N`) or `timeout_ms` elapses, then returns the full current tail (no
/// since-token). Client-side fs watch, not engine IPC. Dedicated background thread only.
/// Owned `char*`; `"[]"` if Paths fail. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_logs_wait(max_bytes: u32, timeout_ms: u32) -> *mut c_char {
    guard_str("[]", || {
        let Some(paths) = ds_config::Paths::resolve() else {
            return to_cstring("[]");
        };
        ds_log::wait_logs_changed(
            &paths.log_file,
            std::time::Duration::from_millis(timeout_ms as u64),
        );
        to_cstring(ds_log::combined_log_json(&paths.log_file, max_bytes as u64))
    })
}

/// Erase entire on-disk activity log (unified + rotated + aux). Irreversible — UI must
/// confirm. No-op if empty. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_logs_clear() {
    guard_val((), || {
        if let Some(paths) = ds_config::Paths::resolve() {
            ds_log::clear_logs(&paths.log_file);
        }
    })
}

/// Product homepage URL — single source for every platform. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_homepage_url() -> *mut c_char {
    guard_str("", || to_cstring(crate::HOMEPAGE_URL))
}

/// Brand colors JSON (`seed_purple`, `mic_orange`, `warning`) — single cross-platform
/// source. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_brand_colors_json() -> *mut c_char {
    guard_str("{}", || to_cstring(crate::BRAND_COLORS_JSON))
}

/// Logs-tab colors JSON (`levels`, `source_palette`) — sibling of `ds_brand_colors_json`.
/// Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_log_colors_json() -> *mut c_char {
    guard_str("{}", || to_cstring(crate::LOG_COLORS_JSON))
}

/// One random Usage speaking-card wash: `{"r","g","b","a"}` (opaque RGB + wash alpha).
/// New color each call; hosts freeze while `speaker` is unchanged. Owned `char*`.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_random_pastel_wash_json() -> *mut c_char {
    guard_str(
        "{}",
        || to_cstring(crate::pastel::random_pastel_wash_json()),
    )
}

/// Workspace version for About. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_version() -> *mut c_char {
    guard_str("", || to_cstring(crate::VERSION))
}

/// Startup update check vs GitHub API (blocking HTTP — only network FFI; call off UI
/// thread). Compares latest tag to [`crate::VERSION`]. Returns
/// `{"update_available", "current_version", "latest_version", "html_url"}`. Any failure
/// → `"{}"`; host must treat missing `update_available` as false (never show pill).
/// Owned `char*`.
#[unsafe(no_mangle)]
pub extern "C" fn ds_update_check_json() -> *mut c_char {
    ds_update_check_json_at("https://api.github.com")
}

/// Test seam for [`ds_update_check_json`]: mockable `api_base`, real marshaling + VERSION.
fn ds_update_check_json_at(api_base: &str) -> *mut c_char {
    guard_str("{}", || {
        let json = ds_model::update_check::check_for_update_at(api_base, crate::VERSION)
            .map(|info| info.to_json())
            .unwrap_or_else(|_| "{}".to_string());
        to_cstring(json)
    })
}

// Shared ds-i18n catalog — every host UI; English fallback; locale defaults to OS.

/// Set active UI locale (BCP-47 / bare tag). NULL no-op; unknown → English at lookup.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_set_locale(locale: *const c_char) {
    guard_val((), || {
        let loc = cstr_or_empty(locale);
        if !loc.is_empty() {
            ds_i18n::set_locale(&loc);
        }
    });
}

/// Active UI locale tag (for matching native number formatters). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_locale() -> *mut c_char {
    guard_str("en", || to_cstring(ds_i18n::locale()))
}

/// Localized string by `key`; missing key returns the key. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_t(key: *const c_char) -> *mut c_char {
    guard_str("", || to_cstring(ds_i18n::t(&cstr_or_empty(key))))
}

/// Localized string with `%{name}` from `args_json` (`{ "name": value }`). Owned `char*`.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_t_args(key: *const c_char, args_json: *const c_char) -> *mut c_char {
    guard_str("", || {
        to_cstring(ds_i18n::t_args_json(
            &cstr_or_empty(key),
            &cstr_or_empty(args_json),
        ))
    })
}

// Shared status-panel formatters (was duplicated per host). Culture-sensitive number
// formatting (if any beyond these complete strings) stays in each UI.

/// Hover word for engine lifecycle state. Tokens: running|idle|warming|downloading|
/// failed|blocked|missing. `progress` = overall byte-weighted 0..1 (downloading only);
/// `why` = failure reason (failed only). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_state_word(
    state: *const c_char,
    progress: f64,
    why: *const c_char,
) -> *mut c_char {
    guard_str("", || {
        to_cstring(crate::status_fmt::engine_state_word(
            &cstr_or_empty(state),
            progress,
            &cstr_or_empty(why),
        ))
    })
}

/// Lifetime / remaining duration; leading and trailing zero units dropped.
/// Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_duration_live(secs: f64) -> *mut c_char {
    guard_str("", || to_cstring(crate::status_fmt::duration_live(secs)))
}

/// See [`crate::status_fmt::usage_resets_in`]. Owned `char*`.
#[unsafe(no_mangle)]
pub extern "C" fn ds_usage_resets_in(resets_at_unix: i64) -> *mut c_char {
    guard_str("", || {
        to_cstring(crate::status_fmt::usage_resets_in(resets_at_unix))
    })
}

/// Runtime label for provider token (mlx|coreml|cuda|cpu; unknown verbatim). Owned
/// `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_runtime_label(provider: *const c_char) -> *mut c_char {
    guard_str("", || {
        to_cstring(crate::status_fmt::runtime_label(&cstr_or_empty(provider)))
    })
}

/// Stat range `"avg{unit}  ·  lo–hi"`. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_stats_range(
    lo: f64,
    avg: f64,
    hi: f64,
    precision: u32,
    unit_key: *const c_char,
) -> *mut c_char {
    guard_str("", || {
        to_cstring(crate::status_fmt::stats_range(
            lo,
            avg,
            hi,
            precision as usize,
            &cstr_or_empty(unit_key),
        ))
    })
}

/// Count + audio duration stat. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_stats_count(count: u64, audio_secs: f64) -> *mut c_char {
    guard_str("", || {
        to_cstring(crate::status_fmt::stats_count(count, audio_secs))
    })
}

/// Human file size (decimal SI). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_human_size(bytes: u64) -> *mut c_char {
    guard_str("", || to_cstring(crate::status_fmt::human_size(bytes)))
}

/// Tray kind via [`ds_status::tray_icon_kind`]. `tray_indicator_json`: JSON array of
/// [`ds_status::StatusTrayKind`] tokens (NULL/malformed → `[]`). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_tray_icon_kind(
    stt_active: u8,
    tts_active: u8,
    tray_indicator_json: *const c_char,
) -> *mut c_char {
    guard_str("idle", || {
        let raw = cstr_or_empty(tray_indicator_json);
        let indicators: Vec<ds_status::StatusTrayKind> =
            serde_json::from_str(&raw).unwrap_or_default();
        let kind = ds_status::tray_icon_kind(stt_active != 0, tts_active != 0, &indicators);
        to_cstring(kind.as_str())
    })
}

/// `ds_tools::DIARIZATION_ENABLED` — single flip for every host. 1 shown / 0 hidden.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_diarization_ui_enabled() -> u8 {
    guard_val(0, || ds_tools::DIARIZATION_ENABLED as u8)
}

/// Session TTS provider: "cpu"|"cuda"|"coreml"|"mlx"|"auto" (NULL/unknown → "auto").
/// Restarts the warm helper + resets TTS stats only if the realized provider changes. 1 if
/// delivered; new provider/stats via `ds_model_status_json`.
#[unsafe(no_mangle)]
pub extern "C" fn ds_set_provider(provider: *const c_char) -> u8 {
    guard_val(0, || {
        // NULL/invalid UTF-8 → "auto".
        let provider = cstr_or(provider, "auto");
        let Some(paths) = ds_config::Paths::resolve() else {
            return 0;
        };
        match ds_ipc::request(
            &paths.engine_sock,
            &ds_ipc::Request::SetProvider { provider },
        ) {
            Ok(ds_ipc::Response::Done) => 1,
            _ => 0,
        }
    })
}

/// Free a `char*` from any ds_* function. NULL no-op.
#[unsafe(no_mangle)]
pub extern "C" fn ds_string_free(s: *mut c_char) {
    guard_val((), || {
        if !s.is_null() {
            // SAFETY: `s` from `to_cstring` (into_raw), not yet freed; from_raw once.
            unsafe { drop(CString::from_raw(s)) };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take_string(ptr: *mut c_char) -> String {
        assert!(!ptr.is_null());
        // SAFETY: freshly returned FFI string; owned until ds_string_free below.
        let value = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        ds_string_free(ptr);
        value
    }

    #[test]
    fn ffi_string_marshaling_and_panic_guards_use_documented_fallbacks() {
        assert_eq!(guard_val(7, || panic!("boundary panic")), 7);
        assert_eq!(
            take_string(guard_str("fallback", || panic!("string boundary panic"))),
            "fallback"
        );
        assert_eq!(take_string(to_cstring(b"bad\0value".to_vec())), "");

        let invalid_utf8 = [0xffu8, 0];
        assert_eq!(cstr_or(std::ptr::null(), "default"), "default");
        assert_eq!(cstr_or(invalid_utf8.as_ptr().cast(), "default"), "default");
        let empty = CString::new("").unwrap();
        assert_eq!(cstr_or(empty.as_ptr(), "default"), "");
        ds_string_free(std::ptr::null_mut());
    }

    #[test]
    fn handle_free_metadata_exports_return_owned_valid_values() {
        assert_eq!(take_string(ds_homepage_url()), crate::HOMEPAGE_URL);
        assert_eq!(take_string(ds_version()), crate::VERSION);

        let brand: serde_json::Value =
            serde_json::from_str(&take_string(ds_brand_colors_json())).unwrap();
        assert!(brand["seed_purple"].as_str().is_some());
        let logs: serde_json::Value =
            serde_json::from_str(&take_string(ds_log_colors_json())).unwrap();
        assert!(logs["source_palette"].as_array().is_some());
        let wash: serde_json::Value =
            serde_json::from_str(&take_string(ds_random_pastel_wash_json())).unwrap();
        assert!(wash["r"].as_u64().is_some());
        assert!(wash["a"].as_f64().is_some());
        let libraries: serde_json::Value =
            serde_json::from_str(&take_string(ds_libraries_json())).unwrap();
        assert!(!libraries.as_array().unwrap().is_empty());
    }

    #[test]
    fn agent_usage_card_json_unknown_agent_has_no_rows() {
        let c = CString::new("not_a_client").unwrap();
        let json = take_string(ds_agent_usage_card_json(c.as_ptr(), 1));
        let card: ds_agent_usage::UsageCard = serde_json::from_str(&json).unwrap();
        assert!(card.rows.is_empty());
    }

    // Non-client exits before Paths::resolve (no dirs, no prompt).
    #[test]
    fn agent_usage_card_authorize_json_unknown_agent_is_empty_without_auth() {
        let c = CString::new("not_a_client").unwrap();
        let json = take_string(ds_agent_usage_card_authorize_json(c.as_ptr()));
        let card: ds_agent_usage::UsageCard = serde_json::from_str(&json).unwrap();
        assert!(card.rows.is_empty());
        assert!(!card.needs_auth);
    }

    #[test]
    fn formatter_exports_accept_c_inputs_and_return_freeable_strings() {
        let unknown = CString::new("custom-runtime").unwrap();
        assert_eq!(
            take_string(ds_runtime_label(unknown.as_ptr())),
            "custom-runtime"
        );
        assert!(!take_string(ds_duration_live(3_661.0)).is_empty());
        assert!(!take_string(ds_human_size(1_500_000)).is_empty());
        assert!(!take_string(ds_stats_count(3, 1.5)).is_empty());

        let state = CString::new("failed").unwrap();
        let why = CString::new("runtime unavailable").unwrap();
        assert!(
            take_string(ds_engine_state_word(state.as_ptr(), 0.0, why.as_ptr()))
                .ends_with("runtime unavailable")
        );
        assert_eq!(take_string(ds_t(std::ptr::null())), "");
        ds_set_locale(std::ptr::null());
    }

    #[test]
    fn shared_tray_export_matches_ds_status() {
        let ind = CString::new(r#"["stt","tts"]"#).unwrap();
        assert_eq!(
            take_string(ds_tray_icon_kind(1, 0, ind.as_ptr())),
            "recording"
        );
        assert_eq!(
            take_string(ds_tray_icon_kind(0, 1, ind.as_ptr())),
            "speaking"
        );
        assert_eq!(
            take_string(ds_tray_icon_kind(1, 1, ind.as_ptr())),
            "recording"
        );
        let empty = CString::new("[]").unwrap();
        assert_eq!(take_string(ds_tray_icon_kind(1, 1, empty.as_ptr())), "idle");
        assert_eq!(
            take_string(ds_tray_icon_kind(1, 1, std::ptr::null())),
            "idle"
        );

        assert_eq!(
            ds_diarization_ui_enabled() != 0,
            ds_tools::DIARIZATION_ENABLED
        );
    }

    /// FFI path through real marshaling + VERSION against a mock (not a retest of
    /// `check_for_update_at` itself).
    #[test]
    fn ds_update_check_json_at_reads_a_mocked_release_through_the_real_ffi_path() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(200).json_body(serde_json::json!({
                "tag_name": "v9.9.9",
                "html_url": "https://github.com/delllusional/DontSpeak/releases/tag/v9.9.9",
            }));
        });

        let ptr = ds_update_check_json_at(&server.base_url());
        // SAFETY: non-null CString from guard_str/to_cstring; free after read.
        let json = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        ds_string_free(ptr);
        mock.assert();

        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["update_available"], true);
        assert_eq!(v["latest_version"], "9.9.9");
    }

    /// Failing endpoint must degrade to `"{}"`, never propagate an error.
    #[test]
    fn ds_update_check_json_at_degrades_to_empty_object_on_a_failing_endpoint() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/repos/delllusional/DontSpeak/releases/latest");
            then.status(404);
        });

        let ptr = ds_update_check_json_at(&server.base_url());
        // SAFETY: non-null CString from guard_str/to_cstring; free after read.
        let json = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        ds_string_free(ptr);
        mock.assert();

        assert_eq!(json, "{}");
    }

    /// FFI enrichment: every param has `detail`; enum params get "one of: …".
    #[test]
    fn ds_tools_json_enriches_every_param_with_detail() {
        let ptr = ds_tools_json();
        // SAFETY: non-null CString from guard_str/to_cstring; free after read.
        let json = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        ds_string_free(ptr);

        let catalog: serde_json::Value = serde_json::from_str(&json).unwrap();
        let tools = catalog.as_array().expect("catalog is an array");
        let mut saw_enum = false;
        for tool in tools {
            for param in tool["params"].as_array().unwrap_or(&Vec::new()) {
                // Key always present (even empty) so hosts read unconditionally.
                let detail = param
                    .get("detail")
                    .and_then(|d| d.as_str())
                    .expect("param has a detail field");
                if param
                    .get("enum")
                    .and_then(|e| e.as_array())
                    .is_some_and(|v| !v.is_empty())
                {
                    saw_enum = true;
                    assert!(
                        detail.starts_with("one of: "),
                        "enum param {} → {detail:?}",
                        param["name"]
                    );
                }
            }
        }
        assert!(saw_enum, "catalog has at least one enum param to qualify");
    }
}
