// C ABI boundary. Pointer validity is the native host's responsibility (Swift/C#).
#![allow(clippy::not_unsafe_ptr_arg_deref)]

//! Stable C ABI for the native UI (committed `dontspeak.h`, cbindgen). Small and
//! HANDLE-FREE: read-only probes, in-process engine lifecycle, mute/TTS provider/locale,
//! shared UI constants/formatters/i18n, string free. Rich control lives in DontSpeak.

use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::{engine, models};

// `catch_unwind` is a no-op under `panic = "abort"`, voiding this safety boundary for
// in-process hosts. `release-ffi` (rust/Cargo.toml) forces `panic = "unwind"` — every
// cdylib/staticlib build of ds-core MUST use it (see apps/macos/build.sh,
// apps/windows/installer/build-common.ps1).
#[cfg(panic = "abort")]
compile_error!(
    "ds-core must not be built with panic=\"abort\" -- its extern \"C\" boundary relies on \
     catch_unwind (see guard_val/guard_str below), which is a documented no-op under an abort \
     strategy and would let any panic reachable from an extern \"C\" fn abort the whole hosting \
     process. Build with `cargo build --profile release-ffi -p ds-core` (or any profile with \
     panic=\"unwind\") instead of the default `release` profile."
);

// Lifecycle state lives in [`crate::host`] (sole owner); these are thin u8 adapters
// matching committed `dontspeak.h`.

/// Start engine on background thread if not running. 1 = running, 0 = failure.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_start() -> u8 {
    guard_val(0, || crate::host::engine_start() as u8)
}

/// Stop engine (clear run flag, join). 1 if was running. Safe on quit.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_stop() -> u8 {
    guard_val(0, || crate::host::engine_stop() as u8)
}

/// Re-read config without restart. 1 if engine running.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_reload() -> u8 {
    guard_val(0, || crate::host::engine_reload() as u8)
}

/// Global MUTE (tray checkbox). `on != 0` silences audio; playback keeps draining.
/// IPC to engine; 1 if delivered, 0 if engine down.
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

/// Open OS system-voice settings (macOS Spoken Content / Windows Speech).
/// False on Linux (no portable page, issue #74). 1 if launched. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_open_voice_settings() -> u8 {
    guard_val(0, || ds_tts::system::open_voice_settings() as u8)
}

/// Run `f`, returning `default` on panic (must not cross FFI).
fn guard_val<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(default)
}

/// Like [`guard_val`] for string returns: heap-allocate `default` only on the panic path.
/// Eager `guard_val(to_cstring(default), …)` would allocate every call and leak the unused
/// `*mut c_char` (no destructor). `unwrap_or_else` builds the fallback only when needed.
fn guard_str(default: &'static str, f: impl FnOnce() -> *mut c_char) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| to_cstring(default))
}

/// Heap C string; caller frees with `ds_string_free`. Interior NUL → "".
fn to_cstring(s: impl Into<Vec<u8>>) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("").unwrap().into_raw(),
    }
}

/// Inbound C string → owned String (empty if NULL or invalid UTF-8).
fn cstr_or_empty(p: *const c_char) -> String {
    cstr_or(p, "")
}

/// Inbound C string; `default` for NULL or invalid UTF-8 (valid empty string kept as-is).
fn cstr_or(p: *const c_char, default: &str) -> String {
    if p.is_null() {
        return default.to_string();
    }
    // SAFETY: `p` non-null; C ABI contract — NUL-terminated, valid for the call;
    // copied into owned String before return.
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_str()
        .unwrap_or(default)
        .to_string()
}

/// Kokoro (TTS) model set present + valid? Disk probe. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_kokoro_present_global() -> u8 {
    guard_val(0, || models::is_kokoro_present() as u8)
}

/// Full Parakeet-ONNX (STT) asset set present + valid? HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_parakeet_onnx_present_global() -> u8 {
    guard_val(0, || models::is_parakeet_onnx_present() as u8)
}

/// Engine running? Pidfile probe. HANDLE-FREE, safe off main thread.
#[unsafe(no_mangle)]
pub extern "C" fn ds_engine_running_global() -> u8 {
    guard_val(0, || engine::is_running() as u8)
}

/// Model-status JSON (presence + subsystem map). Owned `char*`, free with
/// `ds_string_free`; `"{}"` if engine down. HANDLE-FREE, safe off main thread.
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

/// Like [`ds_model_status_json`] but BLOCKS until status `seq` differs from `since` or
/// `timeout_ms` elapses. PUSH transport for the dictation overlay: call on a dedicated
/// background thread (never UI) in a loop. Pass `since = 0` first (returns immediately).
/// JSON `seq` is the next `since`. Owned `char*`; `"{}"` if engine down.
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

/// Lifetime duration down to seconds, leading zero units dropped. Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_duration_live(secs: f64) -> *mut c_char {
    guard_str("", || to_cstring(crate::status_fmt::duration_live(secs)))
}

/// Runtime label for provider token (ane|coreml|cuda|cpu; unknown verbatim). Owned
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

/// Tray kind via [`ds_status::tray_icon_kind`]. `tray_indicator_json`: JSON string array
/// (NULL/malformed → `[]`). Owned `char*`. HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_tray_icon_kind(
    stt_active: u8,
    tts_active: u8,
    tray_indicator_json: *const c_char,
) -> *mut c_char {
    guard_str("idle", || {
        let raw = cstr_or_empty(tray_indicator_json);
        let indicators: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        let kind = ds_status::tray_icon_kind(stt_active != 0, tts_active != 0, &indicators);
        to_cstring(kind.as_str())
    })
}

/// Config `tts_engine` → model_status key (see [`ds_status::ActiveTtsSlot`]). Owned `char*`.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_active_tts_slot(tts_engine: *const c_char) -> *mut c_char {
    guard_str("", || {
        let slot = ds_status::ActiveTtsSlot::from_engine(&cstr_or_empty(tts_engine))
            .map(|s| s.as_str())
            .unwrap_or("");
        to_cstring(slot)
    })
}

/// Config `stt_engine` → model_status key (see [`ds_status::ActiveSttSlot`]). Owned `char*`.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_active_stt_slot(stt_engine: *const c_char) -> *mut c_char {
    guard_str("", || {
        let slot = ds_status::ActiveSttSlot::from_engine(&cstr_or_empty(stt_engine))
            .map(|s| s.as_str())
            .unwrap_or("");
        to_cstring(slot)
    })
}

/// `ds_tools::DIARIZATION_ENABLED` — single flip for every host. 1 shown / 0 hidden.
/// HANDLE-FREE.
#[unsafe(no_mangle)]
pub extern "C" fn ds_diarization_ui_enabled() -> u8 {
    guard_val(0, || ds_tools::DIARIZATION_ENABLED as u8)
}

/// Session TTS provider: "cpu"|"cuda"|"coreml"|"ane"|"auto" (NULL/unknown → "auto").
/// Restarts warm Kokoro + resets TTS stats only if provider actually changes. 1 if
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
        let libraries: serde_json::Value =
            serde_json::from_str(&take_string(ds_libraries_json())).unwrap();
        assert!(!libraries.as_array().unwrap().is_empty());
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
    fn shared_selection_and_tray_exports_match_ds_status() {
        let built_in = CString::new("built_in").unwrap();
        let system = CString::new("system").unwrap();
        let off = CString::new("off").unwrap();
        let claude = CString::new("claude_code").unwrap();
        assert_eq!(take_string(ds_active_tts_slot(built_in.as_ptr())), "kokoro");
        assert_eq!(
            take_string(ds_active_tts_slot(system.as_ptr())),
            "tts_system"
        );
        assert_eq!(take_string(ds_active_tts_slot(off.as_ptr())), "");
        assert_eq!(
            take_string(ds_active_stt_slot(built_in.as_ptr())),
            "parakeet"
        );
        assert_eq!(
            take_string(ds_active_stt_slot(claude.as_ptr())),
            "claude_code"
        );
        assert_eq!(take_string(ds_active_stt_slot(system.as_ptr())), "system");
        assert_eq!(take_string(ds_active_stt_slot(off.as_ptr())), "");

        let ind = CString::new(r#"["stt","tts"]"#).unwrap();
        assert_eq!(
            take_string(ds_tray_icon_kind(1, 0, ind.as_ptr())),
            "recording"
        );
        assert_eq!(
            take_string(ds_tray_icon_kind(0, 1, ind.as_ptr())),
            "speaking"
        );
        assert_eq!(take_string(ds_tray_icon_kind(1, 1, ind.as_ptr())), "recording");
        let empty = CString::new("[]").unwrap();
        assert_eq!(
            take_string(ds_tray_icon_kind(1, 1, empty.as_ptr())),
            "idle"
        );
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
