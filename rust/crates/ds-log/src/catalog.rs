//! Logs-tab pure rules: combined-log JSON parse, free-text filter, first-appearance sources.
//! Single place for every host (was reimplemented per platform).

use serde_json::Value;

/// One wire line from `ds_logs_json` / `ds_logs_wait`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLine {
    pub source: String,
    pub level: String,
    pub text: String,
}

/// Malformed / non-array → empty. Missing fields → `""` (partial lines kept).
pub fn parse_logs_json(json: &str) -> Vec<LogLine> {
    if json.trim().is_empty() {
        return Vec::new();
    }
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|v| {
            let obj = v.as_object()?;
            Some(LogLine {
                source: str_field(obj, "source"),
                level: str_field(obj, "level"),
                text: str_field(obj, "text"),
            })
        })
        .collect()
}

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Distinct sources, first-appearance order (palette index = position mod length).
/// Empty sources skipped.
pub fn distinct_sources(lines: &[LogLine]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for l in lines {
        if l.source.is_empty() || !seen.insert(l.source.clone()) {
            continue;
        }
        ordered.push(l.source.clone());
    }
    ordered
}

/// Case-insensitive substring on text/source/level. Blank query → all lines.
/// Returns `(original_index, line)` for stable row ids.
pub fn filter_logs<'a>(lines: &'a [LogLine], query: &str) -> Vec<(usize, &'a LogLine)> {
    let q = query.trim().to_ascii_lowercase();
    let all = lines.iter().enumerate();
    if q.is_empty() {
        return all.collect();
    }
    all.filter(|(_, l)| line_matches(l, &q)).collect()
}

/// `query` must already be trimmed + lowercased (see [`filter_logs`]).
fn line_matches(line: &LogLine, query_lower: &str) -> bool {
    contains_ci(&line.text, query_lower)
        || contains_ci(&line.source, query_lower)
        || contains_ci(&line.level, query_lower)
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle_lower)
}

/// `"[source] text"` lines (Linux plain-text log). Empty source omits brackets.
pub fn flatten_log_lines(lines: &[LogLine]) -> String {
    lines
        .iter()
        .map(|l| {
            if l.source.is_empty() {
                l.text.clone()
            } else {
                format!("[{}] {}", l.source, l.text)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(source: &str, level: &str, text: &str) -> LogLine {
        LogLine {
            source: source.into(),
            level: level.into(),
            text: text.into(),
        }
    }

    #[test]
    fn empty_or_malformed_json_is_empty_list() {
        for j in ["", "   ", "not json", "{}", "[1,2,3]"] {
            assert!(parse_logs_json(j).is_empty(), "{j:?}");
        }
    }

    #[test]
    fn well_formed_payload_maps_in_order() {
        let lines = parse_logs_json(
            r#"[
                {"source":"dontspeakd","level":"INFO","text":"engine started"},
                {"source":"ds-helper","level":"ERROR","text":"tts spawn failed"}
            ]"#,
        );
        assert_eq!(
            lines,
            [
                line("dontspeakd", "INFO", "engine started"),
                line("ds-helper", "ERROR", "tts spawn failed"),
            ]
        );
    }

    #[test]
    fn missing_fields_default_to_empty() {
        let lines = parse_logs_json(r#"[{"text":"no source or level"}]"#);
        assert_eq!(lines, [line("", "", "no source or level")]);
    }

    #[test]
    fn distinct_sources_preserve_first_appearance() {
        let lines = [
            line("engine", "INFO", "a"),
            line("tts", "INFO", "b"),
            line("engine", "WARN", "c"),
            line("caps", "INFO", "d"),
            line("", "INFO", "skip"),
        ];
        assert_eq!(
            distinct_sources(&lines),
            ["engine", "tts", "caps"]
        );
    }

    #[test]
    fn filter_blank_keeps_all_with_indices() {
        let sample = [
            line("tts", "INFO", "spoke a sentence"),
            line("stt", "ERROR", "mic blocked"),
            line("caps", "WARN", "held too long"),
        ];
        let r = filter_logs(&sample, "");
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].0, 1);
        assert_eq!(filter_logs(&sample, "   \t ").len(), 3);
    }

    #[test]
    fn filter_matches_message_source_or_level_case_insensitively() {
        let sample = [
            line("tts", "INFO", "spoke a sentence"),
            line("stt", "ERROR", "mic blocked"),
            line("caps", "WARN", "held too long"),
        ];
        assert_eq!(
            filter_logs(&sample, "BLOCKED")
                .into_iter()
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(
            filter_logs(&sample, "caps")
                .into_iter()
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            filter_logs(&sample, "error")
                .into_iter()
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [1]
        );
        assert_eq!(
            filter_logs(&sample, "  stt  ")
                .into_iter()
                .map(|(i, _)| i)
                .collect::<Vec<_>>(),
            [1]
        );
        assert!(filter_logs(&sample, "zzz").is_empty());
    }

    #[test]
    fn filter_indexed_keeps_original_indices() {
        let sample = [
            line("tts", "INFO", "spoke a sentence"),
            line("stt", "ERROR", "mic blocked"),
            line("caps", "WARN", "held too long"),
        ];
        // "n" matches seNtence (0) + loNg (2)
        let r = filter_logs(&sample, "n");
        assert_eq!(
            r.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            [0, 2]
        );
    }

    #[test]
    fn flatten_prefixes_source() {
        let lines = [
            line("engine", "INFO", "up"),
            line("", "INFO", "orphan"),
        ];
        assert_eq!(flatten_log_lines(&lines), "[engine] up\norphan");
    }
}
