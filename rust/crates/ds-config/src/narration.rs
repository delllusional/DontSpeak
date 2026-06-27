//! Narration mode (§G.5) — extracting the spoken blockquote digests Claude leads
//! each section with, plus the default narration spec.

/// Finalize one accumulated blockquote run into the output list. Joins the run's lines
/// into one spoken line, collapses whitespace, and drops empties. `complete` records
/// whether the run is provably finished (a terminating line followed, or end-of-final).
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

/// Extract EVERY top-level blockquote of a message, in document order — the per-block
/// spoken digest Claude is instructed (by the narration spec) to lead each section with.
/// Returns one `(text, complete)` pair per blockquote run, markers stripped and lines
/// joined; the body prose between runs is skipped (we never read raw replies). This is
/// what the narration paths speak VERBATIM, one utterance per run, in order.
///
/// A "run" is a maximal block of contiguous top-level `>` lines; it ends at the first
/// non-quote line (a blank or the answer body), a nested quote (`>>`), or end-of-input.
/// Every run except possibly the LAST is provably `complete` (a terminating line followed
/// it). The last run is `complete` only once a terminating line appears OR `is_final` is
/// set — so a streaming caller never voices a half-finished line. A message with no `>`
/// line yields an empty list (mid-stream: the digest may still arrive; on the final batch:
/// this reply has no spoken line → stay silent).
pub fn all_blockquotes_state(msg: &str, is_final: bool) -> Vec<(String, bool)> {
    let is_quote = |l: &str| l.trim_start().starts_with('>');
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut in_run = false;
    for l in msg.lines() {
        if is_quote(l) {
            // Strip leading whitespace, the '>', and one optional following space.
            let t = l.trim_start();
            let inner = t.strip_prefix('>').unwrap_or(t);
            let inner = inner.strip_prefix(' ').unwrap_or(inner);
            // A NESTED quote (`>>` / `> >`) ends the current top-level run and is itself
            // skipped — we narrate only top-level blockquotes, never quotes nested inside.
            if inner.trim_start().starts_with('>') {
                push_blockquote_run(&mut cur, &mut out, true);
                in_run = false;
                continue;
            }
            cur.push(inner.to_string());
            in_run = true;
        } else if in_run {
            // A blank line or the answer body terminates the run (standard Markdown).
            push_blockquote_run(&mut cur, &mut out, true);
            in_run = false;
        }
    }
    if in_run {
        // Trailing run at end-of-input: complete only when this is the message's last batch.
        push_blockquote_run(&mut cur, &mut out, is_final);
    }
    out
}

/// Every top-level blockquote of `msg` as plain spoken lines, in order (treating the
/// message as complete). Used by the final-reply (Stop) path, which has the whole text.
pub fn all_blockquotes(msg: &str) -> Vec<String> {
    all_blockquotes_state(msg, true)
        .into_iter()
        .map(|(t, _)| t)
        .collect()
}

/// The BUILT-IN narration spec — injected into Claude every turn by the `UserPromptSubmit`
/// `provide` hook (when `narrate` includes `digests`). It lives in the binary and is used
/// directly; nothing is written to disk at install. A [`crate::Paths::narration_spec`] file is
/// an OPTIONAL OVERRIDE — create/edit `narration-spec.md` to reshape the spoken voice; an
/// absent or empty file falls back to this. It instructs Claude to lead each reply with a
/// spoken-line blockquote digest; the narrator reads EVERY top-level blockquote aloud, in order
/// (see [`all_blockquotes`]).
pub const DEFAULT_NARRATION_SPEC: &str = r#"# Narrate
Only `>` lines are spoken (rest silent). Lead every reply with a `>` digest: one line per point, plain speech, no markdown/code/URLs/paths, say IDs as words. Any pick-one options → speak each as the LAST `>` lines, for voice reply.
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: every blockquote's text, dropping the completeness flag.
    fn texts(msg: &str, is_final: bool) -> Vec<String> {
        all_blockquotes_state(msg, is_final)
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    #[test]
    fn extracts_every_blockquote_in_document_order() {
        // Several top-level runs separated by body prose — all spoken, in order, each a run.
        assert_eq!(
            all_blockquotes(
                "> First point.\n\nbody one.\n\n> Second point.\n\nbody two.\n\n> Closing ask?"
            ),
            vec!["First point.", "Second point.", "Closing ask?"]
        );
        // A multi-line run joins into one spoken line.
        assert_eq!(
            all_blockquotes("> First part\n> second part\n\nbody"),
            vec!["First part second part"]
        );
        // A blank line before the quote is tolerated.
        assert_eq!(all_blockquotes("\n> hello\nbody"), vec!["hello"]);
    }

    #[test]
    fn nested_quotes_are_skipped_only_top_level_spoken() {
        // A nested quote (`>>` / `> >`) is never voiced and ENDS the current run — so a
        // top-level `>` line after it starts a NEW run (which IS voiced).
        assert_eq!(
            all_blockquotes("> the spoken line\n> > a nested quote\n> a new top-level run"),
            vec!["the spoken line", "a new top-level run"]
        );
        // Nested with nothing top-level after it → just the first run.
        assert_eq!(all_blockquotes("> top\n>> deep"), vec!["top"]);
        // A nested quote on the very first line → no top-level spoken content.
        assert!(all_blockquotes(">> only nested\nbody").is_empty());
    }

    #[test]
    fn no_blockquote_is_silent() {
        assert!(all_blockquotes("just prose").is_empty());
        assert!(all_blockquotes("line one\nline two\n").is_empty());
        assert!(all_blockquotes("").is_empty());
    }

    #[test]
    fn streaming_run_incomplete_until_body_or_final() {
        // Mid-stream: only the quote so far, no following line yet → known but not complete.
        let runs = all_blockquotes_state("> partial spoken line", false);
        assert_eq!(runs, vec![("partial spoken line".to_string(), false)]);
        // Same text, but final batch → complete (flush it).
        let runs = all_blockquotes_state("> partial spoken line", true);
        assert_eq!(runs, vec![("partial spoken line".to_string(), true)]);
        // Prose-only final batch → definitively silent.
        assert!(texts("just prose, no spoken line", true).is_empty());
    }

    #[test]
    fn earlier_runs_complete_once_a_later_line_terminates_them() {
        // A closed first run plus a still-open last run, mid-stream: the first is complete,
        // the last is not until a body line or the final batch arrives.
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
