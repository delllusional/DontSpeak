//! Spoken-summary extraction and the prompt that requests those summaries.

/// Join one blockquote run into a spoken line; drop empties. `complete` = run is finished.
fn push_blockquote_run(cur: &mut Vec<String>, out: &mut Vec<(String, bool)>, complete: bool) {
    if cur.is_empty() {
        return;
    }
    let text = cur
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    cur.clear();
    if !text.is_empty() {
        out.push((text, complete));
    }
}

/// Top-level blockquotes of `msg` in document order as `(text, complete)`.
/// Body prose between runs is skipped — narration speaks these VERBATIM, one utterance per run.
///
/// A run is contiguous top-level `>` lines; ends at non-quote, nested `>>`, or EOF.
/// Every run except possibly the last is `complete`. Last is complete only after a terminator
/// or `is_final` — so streaming never voices a half line. No `>` ⇒ empty (silent).
pub fn all_blockquotes_state(msg: &str, is_final: bool) -> Vec<(String, bool)> {
    let is_quote = |l: &str| l.trim_start().starts_with('>');
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut in_run = false;
    for l in msg.lines() {
        if is_quote(l) {
            let t = l.trim_start();
            let inner = t.strip_prefix('>').unwrap_or(t);
            let inner = inner.strip_prefix(' ').unwrap_or(inner);
            // Nested `>>` ends the top-level run and is skipped.
            if inner.trim_start().starts_with('>') {
                push_blockquote_run(&mut cur, &mut out, true);
                in_run = false;
                continue;
            }
            cur.push(inner.to_string());
            in_run = true;
        } else if in_run {
            push_blockquote_run(&mut cur, &mut out, true);
            in_run = false;
        }
    }
    if in_run {
        // Trailing run: complete only on final batch.
        push_blockquote_run(&mut cur, &mut out, is_final);
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
            vec!["First part second part"]
        );
        assert_eq!(all_blockquotes("\n> hello\nbody"), vec!["hello"]);
    }

    #[test]
    fn nested_quotes_are_skipped_only_top_level_spoken() {
        // Nested ends current run; following top-level starts a new voiced run.
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
    fn streaming_run_incomplete_until_body_or_final() {
        let runs = all_blockquotes_state("> partial spoken line", false);
        assert_eq!(runs, vec![("partial spoken line".to_string(), false)]);
        let runs = all_blockquotes_state("> partial spoken line", true);
        assert_eq!(runs, vec![("partial spoken line".to_string(), true)]);
        assert!(texts("just prose, no spoken line", true).is_empty());
    }

    #[test]
    fn earlier_runs_complete_once_a_later_line_terminates_them() {
        let runs = all_blockquotes_state("> One.\n\nbody.\n\n> Two still open", false);
        assert_eq!(
            runs,
            vec![
                ("One.".to_string(), true),
                ("Two still open".to_string(), false)
            ]
        );
    }
}
