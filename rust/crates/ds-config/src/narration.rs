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

/// Shared cleanup for text about to be spoken — used for both digest blockquotes and the
/// shorts fallback, so the two paths can't drift. The narration spec asks the model not to
/// put code/paths/hashes in spoken lines, but that's a prompt request, not an enforced
/// contract; this is the code-level backstop.
///
/// Strips Markdown marker characters (`` ` * _ # ``, content kept — e.g. `` `path` `` reads
/// as `path`) and drops standalone hash-like tokens (7-40 hex chars with at least one a-f
/// letter, so plain decimal numbers like line counts or years are untouched). Deliberately
/// does NOT touch slashes or file extensions: an earlier length/code/URL/slash guard here
/// swallowed readable replies (e.g. "pause/resume") and was removed. Collapses whitespace;
/// returns `None` if nothing speakable remains.
pub fn clean_for_speech(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let mut s = String::with_capacity(t.len());
    for ch in t.chars() {
        match ch {
            '`' | '*' | '_' | '#' => {}
            '\n' | '\r' | '\t' => s.push(' '),
            other => s.push(other),
        }
    }
    let cleaned = s
        .split_whitespace()
        .filter(|tok| !is_hash_like(tok))
        .collect::<Vec<_>>()
        .join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

/// A standalone token that reads as a hash/commit-id rather than a word or number: all
/// hex digits, 7-40 chars long, with at least one `a`-`f` letter (excludes plain decimal
/// numbers, which are common and fine to speak).
fn is_hash_like(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let len = core.chars().count();
    if !(7..=40).contains(&len) {
        return false;
    }
    let mut has_hex_letter = false;
    for ch in core.chars() {
        if !ch.is_ascii_hexdigit() {
            return false;
        }
        has_hex_letter |= ch.is_ascii_alphabetic();
    }
    has_hex_letter
}

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
    fn clean_for_speech_strips_markers_keeps_content() {
        assert_eq!(
            clean_for_speech("Yes, `that` is the **default**.").as_deref(),
            Some("Yes, that is the default.")
        );
        assert_eq!(
            clean_for_speech("`.github/workflows/release.yml`").as_deref(),
            Some(".github/workflows/release.yml"),
            "path kept, only backtick markers dropped"
        );
    }

    #[test]
    fn clean_for_speech_drops_hash_like_tokens_only() {
        assert_eq!(
            clean_for_speech("It fast forwarded from commit eedfc57 to main.").as_deref(),
            Some("It fast forwarded from commit to main.")
        );
        // Plain numbers, short hex-ish words, and non-hex identifiers are untouched.
        assert_eq!(
            clean_for_speech("Line 1234567 of 2026 tests pass.").as_deref(),
            Some("Line 1234567 of 2026 tests pass.")
        );
        assert_eq!(
            clean_for_speech("Edit src/main and rebuild.").as_deref(),
            Some("Edit src/main and rebuild."),
            "regression: slash must not silence or mangle the reply"
        );
    }

    #[test]
    fn clean_for_speech_empty_or_markers_only_is_none() {
        assert_eq!(clean_for_speech(""), None);
        assert_eq!(clean_for_speech("***"), None);
        assert_eq!(clean_for_speech("###  _ "), None);
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
