//! Shared status-panel presentation logic, rendered identically by every native
//! UI (macOS SwiftUI, Windows WinUI, Linux GTK) through the C ABI (Linux, being
//! Rust, calls these directly). These are the small formatters that used to be
//! duplicated per-platform — the engine-state word, the live duration string,
//! the engine RUNTIME label, and the RTF/first-audio RANGE + count stat strings.
//!
//! NOTE: the stat formatters now return the COMPLETE string (number formatting
//! included) so all three hosts render byte-identically and there is ONE copy of
//! the assembly + catalog-key choice. The number formatting is `.`-decimal and
//! not yet locale-aware — a deliberate "de-dup first, prettify later" tradeoff
//! (was previously hand-formatted natively per platform).

/// Fill a catalog string's `%{name}` placeholders. Mirrors rust-i18n's own
/// substitution so we don't need a JSON round-trip for the simple, NUL-free args
/// these formatters use.
fn fill(key: &str, pairs: &[(&str, &str)]) -> String {
    let mut s = ds_i18n::t(key);
    for (name, value) in pairs {
        s = s.replace(&format!("%{{{name}}}"), value);
    }
    s
}

/// The engine's status NOTE — the one-line message shown in a row's expanded section when the
/// engine isn't ready. `state` is the model-status token, classified through
/// [`ds_status::EngineState`] (a note is shown for `Missing`/`Warming`/`Blocked`/`Downloading`/
/// `Failed`); `progress` is the 0..1 download fraction (only used for "downloading");
/// `why` is the failure reason (only used for "failed"; empty → the generic default reason). The
/// ready states ("running"|"idle") — and anything unrecognized — have no note and return "".
///
/// This is the ONE cross-platform formatter for a row's status word: macOS SwiftUI, Linux GTK
/// and Windows WinUI ALL call it over the C ABI ([`crate::ffi::ds_engine_state_word`]), so the
/// download wording ("Downloading `<pct>`%") can NEVER diverge between platforms. `progress` is a
/// single OVERALL byte-weighted percent across the whole model set (see
/// `ds_model::coreml_repo::ensure_coreml_repos`), NOT a per-file percent.
pub fn engine_state_word(state: &str, progress: f64, why: &str) -> String {
    use ds_status::EngineState;
    match EngineState::parse(state) {
        Some(EngineState::Missing) => ds_i18n::t("status.engine.status.missing"),
        Some(EngineState::Warming) => ds_i18n::t("status.engine.status.warming"),
        Some(EngineState::Blocked) => ds_i18n::t("status.engine.status.blocked"),
        Some(EngineState::Downloading) => {
            let pct = (progress * 100.0).round() as i64;
            if pct <= 0 {
                // Unknown/zero progress — FluidAudio's ANE/Core ML fetches don't report a
                // fraction — so show an indeterminate label instead of a misleading "0%".
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
        // The ready states ("running"/"idle") — and any unrecognized token — have no note.
        _ => String::new(),
    }
}

/// Localized lifetime duration, shown DOWN TO SECONDS so the running totals visibly
/// tick up. Leading zero units are dropped: "5h 11m 23s", "12m 04s", "45s", and
/// "1d 02h 03m 04s" past a day.
pub fn duration_live(secs: f64) -> String {
    let total = secs.round().max(0.0) as i64;
    let d = total / 86400;
    let h = (total % 86400) / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    let (hh, mm, ss) = (format!("{h:02}"), format!("{m:02}"), format!("{s:02}"));
    if d > 0 {
        fill(
            "status.stats.duration_live.days",
            &[("d", &d.to_string()), ("h", &hh), ("m", &mm), ("s", &ss)],
        )
    } else if h > 0 {
        fill(
            "status.stats.duration_live.hours",
            &[("h", &h.to_string()), ("m", &mm), ("s", &ss)],
        )
    } else if m > 0 {
        fill(
            "status.stats.duration_live.minutes",
            &[("m", &m.to_string()), ("s", &ss)],
        )
    } else {
        fill(
            "status.stats.duration_live.seconds",
            &[("s", &s.to_string())],
        )
    }
}

/// The localized RUNTIME label for a resolved execution-provider token:
/// `ane` → Core ML/ANE, `coreml`/`cuda`/`cpu` → the matching ORT label;
/// anything else passes through verbatim. The engine-runtime detail shown under the
/// TTS/STT rows — was hand-duplicated in the Swift + C# hosts; now ONE mapping for all three.
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

/// A stat RANGE — "avg`<unit>`  ·  lo–hi" (e.g. "1.23×  ·  1.00–1.50", "0.5 s  ·  0.3–0.8").
/// `precision` = decimal places; `unit_key` = the catalog key for the unit shown after the
/// average ("status.stats.unit.times" → "×", "status.stats.unit.seconds" → " s"). Returns the
/// COMPLETE string. Replaces the per-platform `Range()` (C#) / `StatRange` (Swift) builders.
pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: usize, unit_key: &str) -> String {
    let unit = ds_i18n::t(unit_key);
    format!("{avg:.precision$}{unit}  ·  {lo:.precision$}–{hi:.precision$}")
}

/// A COUNT + audio-duration stat — "`<count>`  <audio_secs> s" (e.g. "12  45 s"), using the
/// "status.stats.audio_secs" template for the seconds part. Replaces the per-platform
/// `CountText()` (C#) / `countRow()` (Swift) builders.
pub fn stats_count(count: u64, audio_secs: f64) -> String {
    let secs = format!("{:.0}", audio_secs.max(0.0));
    format!(
        "{count}  {}",
        fill("status.stats.audio_secs", &[("secs", &secs)])
    )
}

/// A human-readable file SIZE — decimal (SI, ÷1000) units to match the "file size" convention
/// (Apple's `ByteCountFormatter(.file)` is decimal too): "1.4 GB", "325 MB", "12 KB", "512 B".
/// GB/MB carry one decimal, KB none. Replaces the three drifted per-platform size formatters
/// (Swift `humanSize`, GTK `human_size`, WinUI `HumanSize` — which used binary ÷1024, disagreeing
/// with the other two) so every host's Libraries/Credits tab shows the SAME size for the SAME
/// `size_bytes` from `ds_libraries_json`. The `.`-decimal separator matches `stats_range`.
pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_000_000_000.0 {
        format!("{:.1} GB", b / 1_000_000_000.0)
    } else if b >= 1_000_000.0 {
        format!("{:.1} MB", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0} KB", b / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

/// Format a JSON number without a trailing ".0" when it's whole (2.0 → "2", 0.7 → "0.7").
fn num(v: f64) -> String {
    if v == v.round() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// The localized CONSTRAINT qualifier for one UI-catalog param: an enum's allowed values
/// ("one of: a, b, c" via `tools.param.one_of`), else a numeric `minimum`–`maximum` range
/// ("lo–hi"), else "" when the param carries no constraint. Built from a param object of the
/// `ds_tools::catalog_ui` shape (`enum`/`minimum`/`maximum` keys). Was hand-duplicated as the
/// Swift `toToolParam` + C# `ParamDetail` builders (and omitted entirely on Linux); now ONE
/// mapping every host reads pre-built from `ds_tools_json`.
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
    fn duration_live_drops_leading_zero_units() {
        // English fallback catalog values, p<minute → just seconds.
        assert_eq!(duration_live(45.0), "45s");
        assert_eq!(duration_live(0.0), "0s");
        // 12m 04s
        assert_eq!(duration_live(12.0 * 60.0 + 4.0), "12m 04s");
    }

    #[test]
    fn engine_state_word_handles_each_state() {
        // Ready states have no note.
        assert_eq!(engine_state_word("running", 0.0, ""), "");
        assert_eq!(engine_state_word("idle", 0.0, ""), "");
        // ONE overall byte-weighted percent across the whole model set — no per-file prefix. This
        // is the single formatter every platform (macOS/Linux/Windows) renders, so the wording
        // is identical everywhere.
        assert_eq!(engine_state_word("downloading", 0.5, ""), "Downloading 50%");
        // Rounds the global fraction (e.g. 5/22 of the bytes ≈ 23%).
        assert_eq!(
            engine_state_word("downloading", 5.0 / 22.0, ""),
            "Downloading 23%"
        );
        // Zero/unknown progress (FluidAudio ANE fetch) → indeterminate, not a stuck "0%".
        assert_eq!(engine_state_word("downloading", 0.0, ""), "Downloading…");
        assert_eq!(engine_state_word("failed", 0.0, ""), "Failed to start");
        assert_eq!(
            engine_state_word("failed", 0.0, "no model"),
            "Failed — no model"
        );
    }

    #[test]
    fn human_size_uses_decimal_units_shared_by_every_platform() {
        // Decimal (÷1000) base — one formatter, so macOS/Linux/Windows agree byte-for-byte.
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12_000), "12 KB");
        assert_eq!(human_size(325_000_000), "325.0 MB");
        assert_eq!(human_size(1_400_000_000), "1.4 GB");
        // Boundary: exactly 1000 rolls over to KB (not "1000 B").
        assert_eq!(human_size(1_000), "1 KB");
    }

    #[test]
    fn runtime_label_maps_known_providers() {
        assert_eq!(runtime_label("cpu"), "ORT CPU");
        assert_eq!(runtime_label("cuda"), "ORT CUDA");
        assert_eq!(runtime_label("coreml"), "ORT Core ML");
        assert_eq!(runtime_label("ane"), "FluidAudio ANE");
        // Unknown passes through verbatim.
        assert_eq!(runtime_label("whatever"), "whatever");
    }

    #[test]
    fn stats_range_and_count_format() {
        // RTF: 2-dp, "×" unit after the average only.
        assert_eq!(
            stats_range(1.0, 1.23, 1.5, 2, "status.stats.unit.times"),
            "1.23×  ·  1.00–1.50"
        );
        // First-audio: 1-dp, " s" unit (the catalog value has a leading space).
        assert_eq!(
            stats_range(0.3, 0.5, 0.8, 1, "status.stats.unit.seconds"),
            "0.5 s  ·  0.3–0.8"
        );
        // Count + audio seconds (rounded).
        assert_eq!(stats_count(12, 45.4), "12  45 s");
    }

    #[test]
    fn tool_param_detail_qualifiers() {
        use serde_json::json;
        // An enum → localized "one of: …" (authored order preserved).
        assert_eq!(
            tool_param_detail(&json!({"enum": ["list", "enroll", "forget"]})),
            "one of: list, enroll, forget"
        );
        // A numeric range → "lo–hi", whole numbers without a trailing ".0".
        assert_eq!(
            tool_param_detail(&json!({"minimum": 0.5, "maximum": 0.9})),
            "0.5–0.9"
        );
        assert_eq!(
            tool_param_detail(&json!({"minimum": 1.0, "maximum": 10.0})),
            "1–10"
        );
        // Enum wins over a range if both are (somehow) present.
        assert_eq!(
            tool_param_detail(&json!({"enum": ["a"], "minimum": 1.0, "maximum": 2.0})),
            "one of: a"
        );
        // No constraint, an empty enum, or a lone bound → no qualifier.
        assert_eq!(tool_param_detail(&json!({"type": "string"})), "");
        assert_eq!(tool_param_detail(&json!({"enum": []})), "");
        assert_eq!(tool_param_detail(&json!({"minimum": 1.0})), "");
    }
}
