//! Shared status-panel formatters for every native UI (macOS SwiftUI, Windows WinUI,
//! Linux GTK) via the C ABI. Formerly duplicated per-platform: engine-state word,
//! live duration, runtime label, RTF/first-audio range + count stats.
//!
//! Stat formatters return the COMPLETE string (number formatting included) so hosts
//! render byte-identically. Decimal separator is `.` and not yet locale-aware —
//! deliberate "de-dup first, prettify later" tradeoff.

/// Fill a catalog string's `%{name}` placeholders (mirrors rust-i18n; avoids JSON
/// round-trip for these NUL-free args).
fn fill(key: &str, pairs: &[(&str, &str)]) -> String {
    let mut s = ds_i18n::t(key);
    for (name, value) in pairs {
        s = s.replace(&format!("%{{{name}}}"), value);
    }
    s
}

/// One-line status note for a row when the engine isn't ready.
///
/// `state` is a model-status token via [`ds_status::EngineState`] (note for
/// `Missing`/`Warming`/`Blocked`/`Downloading`/`Failed`); ready/unrecognized → "".
/// `progress` is the overall 0..1 byte-weighted download fraction (not per-file; see
/// `ds_model::coreml_repo::ensure_coreml_repos`). `why` is the failure reason (empty →
/// generic default). ONE cross-platform path via [`crate::ffi::ds_engine_state_word`].
pub fn engine_state_word(state: &str, progress: f64, why: &str) -> String {
    use ds_status::EngineState;
    match EngineState::parse(state) {
        Some(EngineState::Missing) => ds_i18n::t("status.engine.status.missing"),
        Some(EngineState::Warming) => ds_i18n::t("status.engine.status.warming"),
        Some(EngineState::Blocked) => ds_i18n::t("status.engine.status.blocked"),
        Some(EngineState::Downloading) => {
            let pct = (progress * 100.0).round() as i64;
            if pct <= 0 {
                // FluidAudio ANE/Core ML fetches often report no fraction — avoid "0%".
                ds_i18n::t("status.engine.status.downloading_indeterminate")
            } else {
                fill(
                    "status.engine.status.downloading",
                    &[("pct", &pct.to_string())],
                )
            }
        }
        Some(EngineState::Failed) => {
            if why.is_empty() {
                ds_i18n::t("status.engine.reason.default")
            } else {
                fill("status.engine.status.failed", &[("why", why)])
            }
        }
        // ready ("running"/"idle") or unrecognized
        _ => String::new(),
    }
}

/// Lifetime / remaining duration; leading+trailing zero units dropped. Usage remaining:
/// [`duration_live_no_seconds`].
pub fn duration_live(secs: f64) -> String {
    let total = secs.round().max(0.0) as i64;
    let d = total / 86400;
    let h = (total % 86400) / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let d_s = d.to_string();
    let h_s = h.to_string();
    let h2 = format!("{h:02}");
    let m_s = m.to_string();
    let m2 = format!("{m:02}");
    let s2 = format!("{s:02}");
    if d > 0 {
        if h == 0 && m == 0 && s == 0 {
            fill("status.stats.duration_live.days_only", &[("d", &d_s)])
        } else if m == 0 && s == 0 {
            fill(
                "status.stats.duration_live.days_hours",
                &[("d", &d_s), ("h", &h2)],
            )
        } else if s == 0 {
            fill(
                "status.stats.duration_live.days_hours_minutes",
                &[("d", &d_s), ("h", &h2), ("m", &m2)],
            )
        } else {
            fill(
                "status.stats.duration_live.days",
                &[("d", &d_s), ("h", &h2), ("m", &m2), ("s", &s2)],
            )
        }
    } else if h > 0 {
        if m == 0 && s == 0 {
            fill("status.stats.duration_live.hours_only", &[("h", &h_s)])
        } else if s == 0 {
            fill(
                "status.stats.duration_live.hours_minutes",
                &[("h", &h_s), ("m", &m2)],
            )
        } else {
            fill(
                "status.stats.duration_live.hours",
                &[("h", &h_s), ("m", &m2), ("s", &s2)],
            )
        }
    } else if m > 0 {
        if s == 0 {
            fill("status.stats.duration_live.minutes_only", &[("m", &m_s)])
        } else {
            fill(
                "status.stats.duration_live.minutes",
                &[("m", &m_s), ("s", &s2)],
            )
        }
    } else {
        fill(
            "status.stats.duration_live.seconds",
            &[("s", &s.to_string())],
        )
    }
}

/// Usage remaining (`2d 05h` …); no "Resets in" prefix; minute is finest unit.
pub fn usage_resets_in(resets_at_unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let remaining = (resets_at_unix - now).max(0) as f64;
    duration_live_no_seconds(remaining)
}

/// [`duration_live`] without seconds (sub-minute → `0m`).
fn duration_live_no_seconds(secs: f64) -> String {
    let total = secs.round().max(0.0) as i64;
    let d = total / 86400;
    let h = (total % 86400) / 3600;
    let m = (total % 3600) / 60;
    let d_s = d.to_string();
    let h_s = h.to_string();
    let h2 = format!("{h:02}");
    let m_s = m.to_string();
    let m2 = format!("{m:02}");
    if d > 0 {
        if h == 0 && m == 0 {
            fill("status.stats.duration_live.days_only", &[("d", &d_s)])
        } else if m == 0 {
            fill(
                "status.stats.duration_live.days_hours",
                &[("d", &d_s), ("h", &h2)],
            )
        } else {
            fill(
                "status.stats.duration_live.days_hours_minutes",
                &[("d", &d_s), ("h", &h2), ("m", &m2)],
            )
        }
    } else if h > 0 {
        if m == 0 {
            fill("status.stats.duration_live.hours_only", &[("h", &h_s)])
        } else {
            fill(
                "status.stats.duration_live.hours_minutes",
                &[("h", &h_s), ("m", &m2)],
            )
        }
    } else {
        fill(
            "status.stats.duration_live.minutes_only",
            &[("m", &m_s)],
        )
    }
}

/// Runtime label for a provider token: `ane` → Core ML/ANE; `coreml`/`cuda`/`cpu` →
/// ORT label; else verbatim. ONE mapping (was duplicated in Swift + C#).
pub fn runtime_label(provider: &str) -> String {
    use ds_config::Provider;
    let key = if provider == Provider::Ane.as_str() {
        "status.engine.coreml_ane"
    } else if provider == Provider::OrtCoreMl.as_str() {
        "status.engine.coreml"
    } else if provider == Provider::OrtCuda.as_str() {
        "status.engine.cuda"
    } else if provider == Provider::OrtCpu.as_str() {
        "status.engine.cpu"
    } else {
        return provider.to_string();
    };
    ds_i18n::t(key)
}

/// Stat range: `"avg{unit}  ·  lo–hi"`. `precision` = decimals; `unit_key` = catalog
/// unit after the average. Complete string (replaces per-platform builders).
///
/// `precision` comes over FFI ([`crate::ffi::ds_stats_range`]) and is clamped:
/// `format!("{:.precision$}")` panics for `precision >= 65536`.
pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: usize, unit_key: &str) -> String {
    let precision = precision.min(17);
    let unit = ds_i18n::t(unit_key);
    format!("{avg:.precision$}{unit}  ·  {lo:.precision$}–{hi:.precision$}")
}

/// Count + audio duration: `"<count>  <audio_secs> s"`.
pub fn stats_count(count: u64, audio_secs: f64) -> String {
    let secs = format!("{:.0}", audio_secs.max(0.0));
    format!(
        "{count}  {}",
        fill("status.stats.audio_secs", &[("secs", &secs)])
    )
}

/// Human-readable file size — decimal SI (÷1000), matching Apple's file-size convention:
/// "1.4 GB", "325 MB", "12 KB", "512 B". Replaces three drifted host formatters (WinUI
/// used binary ÷1024). `.` decimal matches `stats_range`.
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    // Roll over before rounding would show the lower unit as "1000".
    if b >= 999_950_000.0 {
        format!("{:.1} GB", b / 1_000_000_000.0)
    } else if b >= 999_950.0 {
        format!("{:.1} MB", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0} KB", b / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

/// Format a JSON number without trailing ".0" when whole (2.0 → "2").
fn num(v: f64) -> String {
    if v == v.round() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Constraint qualifier for one `ds_tools::catalog_ui` param: enum → "one of: …",
/// else min–max range, else "". Pre-built into `ds_tools_json` so hosts don't re-derive
/// (was Swift `toToolParam` + C# `ParamDetail`; omitted on Linux).
pub fn tool_param_detail(param: &serde_json::Value) -> String {
    if let Some(vals) = param
        .get("enum")
        .and_then(|e| e.as_array())
        .filter(|v| !v.is_empty())
    {
        let joined = vals
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return fill("tools.param.one_of", &[("values", &joined)]);
    }
    match (
        param.get("minimum").and_then(|m| m.as_f64()),
        param.get("maximum").and_then(|m| m.as_f64()),
    ) {
        (Some(lo), Some(hi)) => format!("{}–{}", num(lo), num(hi)),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_live_drops_leading_and_trailing_zero_units() {
        assert_eq!(duration_live(45.0), "45s");
        assert_eq!(duration_live(0.0), "0s");
        assert_eq!(duration_live(12.0 * 60.0 + 4.0), "12m 04s");
        assert_eq!(duration_live(12.0 * 60.0), "12m");
        assert_eq!(duration_live(5.0 * 3600.0), "5h");
        assert_eq!(duration_live(5.0 * 3600.0 + 11.0 * 60.0), "5h 11m");
        assert_eq!(duration_live(86400.0), "1d");
        assert_eq!(duration_live(86400.0 + 5.0 * 3600.0), "1d 05h");
        assert_eq!(
            duration_live(86400.0 + 2.0 * 3600.0 + 3.0 * 60.0 + 4.0),
            "1d 02h 03m 04s"
        );
        // Never a leading 0d when under a day.
        assert!(!duration_live(5.0 * 3600.0).contains('d'));
    }

    #[test]
    fn duration_live_no_seconds_omits_second_unit() {
        assert_eq!(duration_live_no_seconds(45.0), "0m");
        assert_eq!(duration_live_no_seconds(0.0), "0m");
        assert_eq!(duration_live_no_seconds(12.0 * 60.0 + 4.0), "12m");
        assert_eq!(duration_live_no_seconds(12.0 * 60.0), "12m");
        assert_eq!(duration_live_no_seconds(5.0 * 3600.0), "5h");
        assert_eq!(duration_live_no_seconds(5.0 * 3600.0 + 11.0 * 60.0), "5h 11m");
        assert_eq!(
            duration_live_no_seconds(5.0 * 3600.0 + 11.0 * 60.0 + 30.0),
            "5h 11m"
        );
        assert_eq!(duration_live_no_seconds(86400.0), "1d");
        assert_eq!(duration_live_no_seconds(86400.0 + 5.0 * 3600.0), "1d 05h");
        assert_eq!(
            duration_live_no_seconds(86400.0 + 2.0 * 3600.0 + 3.0 * 60.0 + 4.0),
            "1d 02h 03m"
        );
        assert!(!duration_live_no_seconds(90.0).contains('s'));
    }

    #[test]
    fn engine_state_word_handles_each_state() {
        assert_eq!(engine_state_word("running", 0.0, ""), "");
        assert_eq!(engine_state_word("idle", 0.0, ""), "");
        // Overall byte-weighted % — wording must match every host.
        assert_eq!(engine_state_word("downloading", 0.5, ""), "Downloading 50%");
        assert_eq!(
            engine_state_word("downloading", 5.0 / 22.0, ""),
            "Downloading 23%"
        );
        // Zero progress → indeterminate, not "0%".
        assert_eq!(engine_state_word("downloading", 0.0, ""), "Downloading…");
        assert_eq!(engine_state_word("failed", 0.0, ""), "Failed to start");
        assert_eq!(
            engine_state_word("failed", 0.0, "no model"),
            "Failed — no model"
        );
    }

    #[test]
    fn human_size_uses_decimal_units_shared_by_every_platform() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12_000), "12 KB");
        assert_eq!(human_size(325_000_000), "325.0 MB");
        assert_eq!(human_size(1_400_000_000), "1.4 GB");
        // Exactly 1000 → "1 KB", not "1000 B".
        assert_eq!(human_size(1_000), "1 KB");
        assert_eq!(human_size(999_999), "1.0 MB");
        assert_eq!(human_size(999_999_999), "1.0 GB");
    }

    #[test]
    fn runtime_label_maps_known_providers() {
        assert_eq!(runtime_label("cpu"), "ORT CPU");
        assert_eq!(runtime_label("cuda"), "ORT CUDA");
        assert_eq!(runtime_label("coreml"), "ORT Core ML");
        assert_eq!(runtime_label("ane"), "FluidAudio ANE");
        assert_eq!(runtime_label("whatever"), "whatever");
    }

    #[test]
    fn stats_range_and_count_format() {
        assert_eq!(
            stats_range(1.0, 1.23, 1.5, 2, "status.stats.unit.times"),
            "1.23×  ·  1.00–1.50"
        );
        // Catalog seconds unit has a leading space.
        assert_eq!(
            stats_range(0.3, 0.5, 0.8, 1, "status.stats.unit.seconds"),
            "0.5 s  ·  0.3–0.8"
        );
        assert_eq!(stats_count(12, 45.4), "12  45 s");
    }

    #[test]
    fn tool_param_detail_qualifiers() {
        use serde_json::json;
        assert_eq!(
            tool_param_detail(&json!({"enum": ["list", "enroll", "forget"]})),
            "one of: list, enroll, forget"
        );
        assert_eq!(
            tool_param_detail(&json!({"minimum": 0.5, "maximum": 0.9})),
            "0.5–0.9"
        );
        assert_eq!(
            tool_param_detail(&json!({"minimum": 1.0, "maximum": 10.0})),
            "1–10"
        );
        // Enum wins if both present.
        assert_eq!(
            tool_param_detail(&json!({"enum": ["a"], "minimum": 1.0, "maximum": 2.0})),
            "one of: a"
        );
        assert_eq!(tool_param_detail(&json!({"type": "string"})), "");
        assert_eq!(tool_param_detail(&json!({"enum": []})), "");
        assert_eq!(tool_param_detail(&json!({"minimum": 1.0})), "");
    }
}
