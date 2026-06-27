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
/// engine isn't ready. `state` is the raw model-status string ("warming"|"downloading"|"failed"|
/// "blocked"|"missing"); `progress` is the 0..1 download fraction (only used for "downloading");
/// `why` is the failure reason (only used for "failed"; empty → the generic default reason). The
/// ready states ("running"|"idle") — and anything unrecognized — have no note and return "".
pub fn engine_state_word(state: &str, progress: f64, why: &str) -> String {
    engine_state_word_files(state, progress, why, 0, 0)
}

/// As [`engine_state_word`], but with the in-flight download's `file_index`/`file_count` so a
/// multi-file model set reads "<index>/<count> · Downloading <pct>%" — the user sees WHICH file
/// of how many, and that file's own percent (not a confusing cross-file aggregate). `0`/`0`
/// (single file / unknown) falls back to the plain percent.
pub fn engine_state_word_files(
    state: &str,
    progress: f64,
    why: &str,
    file_index: i64,
    file_count: i64,
) -> String {
    match state {
        "missing" => ds_i18n::t("status.engine.status.missing"),
        "warming" => ds_i18n::t("status.engine.status.warming"),
        "blocked" => ds_i18n::t("status.engine.status.blocked"),
        "downloading" => {
            let pct = (progress * 100.0).round() as i64;
            let base = if pct <= 0 {
                // Unknown/zero progress — FluidAudio's ANE/Core ML fetches don't report a
                // fraction — so show an indeterminate label instead of a misleading "0%".
                ds_i18n::t("status.engine.status.downloading_indeterminate")
            } else {
                fill("status.engine.status.downloading", &[("pct", &pct.to_string())])
            };
            if file_count > 1 && file_index > 0 {
                format!("{file_index}/{file_count} · {base}")
            } else {
                base
            }
        }
        "failed" => {
            if why.is_empty() {
                ds_i18n::t("status.engine.reason.default")
            } else {
                fill("status.engine.status.failed", &[("why", why)])
            }
        }
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
        fill("status.stats.duration_live.seconds", &[("s", &s.to_string())])
    }
}

/// The localized RUNTIME label for a resolved execution-provider token:
/// `ane` → Core ML/ANE, `ort_coreml`/`ort_cuda`/`ort_cpu` → the matching ORT label;
/// anything else passes through verbatim. The engine-runtime detail shown under the
/// TTS/STT rows — was hand-duplicated in the Swift + C# hosts; now ONE mapping for all three.
pub fn runtime_label(provider: &str) -> String {
    let key = match provider {
        "ane" => "status.engine.coreml_ane",
        "ort_coreml" => "status.engine.coreml",
        "ort_cuda" => "status.engine.cuda",
        "ort_cpu" => "status.engine.cpu",
        other => return other.to_string(),
    };
    ds_i18n::t(key)
}

/// A stat RANGE — "avg<unit>  ·  lo–hi" (e.g. "1.23×  ·  1.00–1.50", "0.5 s  ·  0.3–0.8").
/// `precision` = decimal places; `unit_key` = the catalog key for the unit shown after the
/// average ("status.stats.unit.times" → "×", "status.stats.unit.seconds" → " s"). Returns the
/// COMPLETE string. Replaces the per-platform `Range()` (C#) / `StatRange` (Swift) builders.
pub fn stats_range(lo: f64, avg: f64, hi: f64, precision: usize, unit_key: &str) -> String {
    let unit = ds_i18n::t(unit_key);
    format!("{avg:.precision$}{unit}  ·  {lo:.precision$}–{hi:.precision$}")
}

/// A COUNT + audio-duration stat — "<count>  <audio_secs> s" (e.g. "12  45 s"), using the
/// "status.stats.audio_secs" template for the seconds part. Replaces the per-platform
/// `CountText()` (C#) / `countRow()` (Swift) builders.
pub fn stats_count(count: u64, audio_secs: f64) -> String {
    let secs = format!("{:.0}", audio_secs.max(0.0));
    format!(
        "{count}  {}",
        fill("status.stats.audio_secs", &[("secs", &secs)])
    )
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
        assert_eq!(engine_state_word("downloading", 0.5, ""), "Downloading 50%");
        // Zero/unknown progress (FluidAudio ANE fetch) → indeterminate, not a stuck "0%".
        assert_eq!(engine_state_word("downloading", 0.0, ""), "Downloading…");
        // Multi-file set → "<index>/<count> · " prefix with the CURRENT file's percent.
        assert_eq!(
            engine_state_word_files("downloading", 0.5, "", 3, 22),
            "3/22 · Downloading 50%"
        );
        // Single file (count ≤ 1) → no prefix.
        assert_eq!(engine_state_word_files("downloading", 0.5, "", 1, 1), "Downloading 50%");
        assert_eq!(engine_state_word("failed", 0.0, ""), "Failed to start");
        assert_eq!(
            engine_state_word("failed", 0.0, "no model"),
            "Failed — no model"
        );
    }

    #[test]
    fn runtime_label_maps_known_providers() {
        assert_eq!(runtime_label("ort_cpu"), "ORT CPU");
        assert_eq!(runtime_label("ort_cuda"), "ORT CUDA");
        assert_eq!(runtime_label("ort_coreml"), "ORT Core ML");
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
}
