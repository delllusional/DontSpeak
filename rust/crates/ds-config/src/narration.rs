//! Spoken-summary extraction and the prompt that requests those summaries.

/// Top-level blockquote lines of `msg` in document order as `(text, complete)`.
/// Body prose between them is skipped — narration speaks these VERBATIM, one utterance
/// per `>` line. The spec asks for one point per line and language is classified per
/// utterance, so joining adjacent lines would put a single language verdict on a digest
/// that switches language between points (garbling the minority-language line).
///
/// Nested `>>` lines are never spoken. The last line is `complete` once its newline
/// arrives or on `is_final` — so streaming never voices a half line. No `>` ⇒ empty
/// (silent).
pub fn all_blockquotes_state(msg: &str, is_final: bool) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut last_line_spoken = false;
    for l in msg.lines() {
        last_line_spoken = false;
        let Some(inner) = l.trim_start().strip_prefix('>') else {
            continue;
        };
        let inner = inner.strip_prefix(' ').unwrap_or(inner);
        if inner.trim_start().starts_with('>') {
            continue;
        }
        let text = inner.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        out.push((text, true));
        last_line_spoken = true;
    }
    if last_line_spoken
        && !is_final
        && !msg.ends_with('\n')
        && let Some(last) = out.last_mut()
    {
        last.1 = false;
    }
    out
}

/// All top-level blockquotes as plain lines (message treated complete). Stop-path helper.
pub fn all_blockquotes(msg: &str) -> Vec<String> {
    all_blockquotes_state(msg, true)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

/// Built-in narration spec injected by `UserPromptSubmit` when `narrate` includes `digests`.
/// Extraction contract: [`all_blockquotes`].
pub const DEFAULT_NARRATION_SPEC: &str = r#"# Narrate
Start every reply with a concise spoken summary of the full response. Write each point on its own `>` line in plain text, without other Markdown, code, URLs, or paths.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(msg: &str, is_final: bool) -> Vec<String> {
        all_blockquotes_state(msg, is_final)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    #[test]
    fn extracts_every_blockquote_in_document_order() {
        assert_eq!(
            all_blockquotes(
                "> First point.\n\nbody one.\n\n> Second point.\n\nbody two.\n\n> Closing ask?"
            ),
            vec!["First point.", "Second point.", "Closing ask?"]
        );
        assert_eq!(
            all_blockquotes("> First part\n> second part\n\nbody"),
            vec!["First part", "second part"]
        );
        assert_eq!(all_blockquotes("\n> hello\nbody"), vec!["hello"]);
    }

    #[test]
    fn adjacent_lines_stay_separate_utterances() {
        // Language is classified per utterance: a digest that switches language
        // between points must not merge into one verdict (ru line spoken under `en`
        // comes out garbled).
        assert_eq!(
            all_blockquotes("> Русская строка о результате.\n> English line about the fix."),
            vec![
                "Русская строка о результате.",
                "English line about the fix."
            ]
        );
    }

    #[test]
    fn nested_quotes_are_skipped_only_top_level_spoken() {
        // Nested lines are skipped; surrounding top-level lines still speak.
        assert_eq!(
            all_blockquotes("> the spoken line\n> > a nested quote\n> a new top-level run"),
            vec!["the spoken line", "a new top-level run"]
        );
        assert_eq!(all_blockquotes("> top\n>> deep"), vec!["top"]);
        assert!(all_blockquotes(">> only nested\nbody").is_empty());
    }

    #[test]
    fn all_blockquotes_empty_on_prose_only() {
        assert!(all_blockquotes("just prose").is_empty());
        assert!(all_blockquotes("line one\nline two\n").is_empty());
        assert!(all_blockquotes("").is_empty());
    }

    #[test]
    fn streaming_line_incomplete_until_newline_or_final() {
        let lines = all_blockquotes_state("> partial spoken line", false);
        assert_eq!(lines, vec![("partial spoken line".to_string(), false)]);
        let lines = all_blockquotes_state("> partial spoken line", true);
        assert_eq!(lines, vec![("partial spoken line".to_string(), true)]);
        // A newline terminates the line — speakable before the final batch.
        let lines = all_blockquotes_state("> done line\n", false);
        assert_eq!(lines, vec![("done line".to_string(), true)]);
        let lines = all_blockquotes_state("> done line\n> next partial", false);
        assert_eq!(
            lines,
            vec![
                ("done line".to_string(), true),
                ("next partial".to_string(), false)
            ]
        );
        assert!(texts("just prose, no spoken line", true).is_empty());
    }

    #[test]
    fn a_bare_marker_tail_never_retracts_or_speaks() {
        // A trailing `> ` (marker typed, text not yet streamed) adds no entry and
        // must not reopen the previous line's completeness.
        assert_eq!(
            all_blockquotes_state("> a\n> ", false),
            vec![("a".to_string(), true)]
        );
        assert!(all_blockquotes_state(">", false).is_empty());
    }

    #[test]
    fn crlf_lines_split_and_complete_like_lf() {
        assert_eq!(
            all_blockquotes_state("> one\r\n> two\r\n", false),
            vec![("one".to_string(), true), ("two".to_string(), true)]
        );
    }

    #[test]
    fn earlier_lines_complete_once_a_later_line_terminates_them() {
        let lines = all_blockquotes_state("> One.\n\nbody.\n\n> Two still open", false);
        assert_eq!(
            lines,
            vec![
                ("One.".to_string(), true),
                ("Two still open".to_string(), false)
            ]
        );
    }
}
