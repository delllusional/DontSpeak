//! The per-message narration accumulation core — "given a streamed `MessageDisplay` batch,
//! which top-level blockquote runs become speakable now". PURE of IO (no socket, no disk) so
//! it is exhaustively unit-testable; [`crate::hook_narrate::message_display`] wraps it with the
//! per-session state file + lock and the engine forward.
//!
//! Claude Code streams an assistant message as many `MessageDisplay` batches (a `delta` chunk
//! keyed by content-block `index`, plus a sticky `final` flag), and — because "all matching
//! hooks run in parallel" — the per-batch hook processes can arrive out of order or overlap.
//! [`Accum`] makes reconstruction INDEPENDENT of arrival order (parts keyed by index) and emits
//! each completed blockquote run EXACTLY ONCE, in document order (a high-water mark, not a
//! one-shot latch), so a later run — e.g. a closing question — is still voiced and a re-fed or
//! duplicate batch emits nothing.

use std::collections::BTreeMap;

/// Per-message accumulation: feed batches, get back the blockquote runs that became newly
/// speakable. `hook_narrate` persists one of these per session (serialized to a file under a
/// lock, so the overlapping per-batch hook processes take turns).
#[derive(Default, Clone, Debug, PartialEq)]
pub(crate) struct Accum {
    /// Each batch's chunk keyed by content-block `index` → cumulative text reconstructs in
    /// index order regardless of the order calls arrive.
    pub parts: BTreeMap<u64, String>,
    /// Sticky: once ANY batch carried `final=true`, the message is final — even if that
    /// batch arrived before the one holding the blockquote.
    pub seen_final: bool,
    /// High-water mark: how many blockquote runs have already been forwarded to TTS, in
    /// document order. Each run is emitted EXACTLY ONCE — the moment it is provably complete —
    /// so re-fed and duplicate batches advance nothing.
    pub emitted: usize,
    /// Latch for the "shorts" fallback: a final message with NO blockquote is voiced WHOLE
    /// exactly once (set true the moment we consider it), so a late duplicate batch can't
    /// re-speak it.
    pub short_done: bool,
}

impl Accum {
    /// Feed one streamed batch; return the blockquote runs that became newly speakable on THIS
    /// batch, in document order, each returned exactly once over the message's lifetime (usually
    /// empty mid-run, sometimes several at once). `displayed_text` (cumulative) wins over `delta`
    /// when a CC version sends it; otherwise the per-index `delta` parts are reconstructed.
    pub fn feed(
        &mut self,
        index: u64,
        delta: &str,
        displayed_text: Option<&str>,
        is_final: bool,
        messages_on: bool,
        short_on: bool,
    ) -> Vec<String> {
        self.seen_final |= is_final;

        let cumulative = match displayed_text {
            Some(dt) if !dt.trim().is_empty() => dt.to_string(),
            _ => {
                self.parts.insert(index, delta.to_string());
                self.parts.values().map(String::as_str).collect::<String>()
            }
        };

        // Every top-level blockquote run so far, in document order, each flagged `complete`.
        // The speakable prefix is the leading stretch of complete runs (only the LAST run can
        // still be open). Emit those beyond the high-water mark, in order, and advance it —
        // so each run is voiced exactly once and a later run (e.g. a closing question) is
        // still caught. The high-water mark advances regardless of `messages_on`, so the
        // short fallback below can tell "no blockquote at all" from "blockquotes, but muted".
        let runs = ds_config::all_blockquotes_state(&cumulative, self.seen_final);
        let total = runs.len();
        let speakable = runs.iter().take_while(|(_, complete)| *complete).count();
        let mut spoken = Vec::new();
        if messages_on {
            for (text, _) in runs.into_iter().take(speakable).skip(self.emitted) {
                let text = text.trim().to_string();
                if !text.is_empty() {
                    spoken.push(text);
                }
            }
        }
        self.emitted = speakable.max(self.emitted);

        // SHORT fallback: a FINAL message with NO blockquote AT ALL is voiced whole, once —
        // if it's short and plain enough to read aloud cleanly (no code fence / path / URL).
        // Latched so a late duplicate batch never re-speaks it.
        if short_on && self.seen_final && total == 0 && !self.short_done {
            self.short_done = true;
            if let Some(utt) = short_reply_utterance(&cumulative) {
                spoken.push(utt);
            }
        }

        // Once the message is final and every run has been voiced, free the buffered text —
        // the high-water mark stays, so any late duplicate batch still emits nothing.
        if self.seen_final && self.emitted >= total {
            self.parts.clear();
        }
        spoken
    }
}

/// Turn a blockquote-less reply into a spoken utterance — read WHOLE, so no information is
/// lost. A reply WITHOUT a `>` digest is the "short" case and is voiced in full; the only
/// thing dropped is inline Markdown markers (`` ` `` `* _ #`) and collapsed whitespace, for
/// cleaner speech (no words removed). Returns `None` only for empty / markers-only text.
///
/// (No length/code/URL/slash guards: they silently swallowed readable replies — e.g. a
/// slashed word like "pause/resume" muted a whole answer. We read everything for now and can
/// special-case genuinely-unspeakable content later.)
pub(crate) fn short_reply_utterance(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let mut s = String::with_capacity(t.len());
    for ch in t.chars() {
        match ch {
            '`' | '*' | '_' | '#' => {} // drop inline Markdown emphasis/code/heading markers
            '\n' | '\r' | '\t' => s.push(' '),
            other => s.push(other),
        }
    }
    let cleaned = s.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Exhaustive coverage for `short_reply_utterance` (the blockquote-less "shorts"
    // path). The rule is now: read a blockquote-less reply WHOLE — no info lost. Only
    // empty / markers-only text is dropped. (It used to over-filter and silently swallow
    // readable replies — e.g. a slashed word like "pause/resume" muted a whole answer.)

    #[test]
    fn short_utt_plain_text_passes_trimmed() {
        assert_eq!(
            short_reply_utterance("  Hello there.  ").as_deref(),
            Some("Hello there.")
        );
    }

    #[test]
    fn short_utt_empty_or_whitespace_is_none() {
        assert_eq!(short_reply_utterance(""), None);
        assert_eq!(short_reply_utterance("   \n\t "), None);
    }

    #[test]
    fn short_utt_markers_only_becomes_none() {
        // After dropping the markdown markers nothing remains → not spoken.
        assert_eq!(short_reply_utterance("***"), None);
        assert_eq!(short_reply_utterance("###  _ "), None);
    }

    #[test]
    fn short_utt_long_text_is_read_whole() {
        // No length cap any more — a long blockquote-less reply is still read.
        let long = "word ".repeat(120); // ~600 chars
        let spoken = short_reply_utterance(&long).expect("long text is read");
        assert!(spoken.starts_with("word word"));
        assert!(spoken.chars().count() > 320, "not truncated/silenced");
    }

    #[test]
    fn short_utt_slashed_word_is_read_whole() {
        // The regression: a slashed word must NOT silence the reply — it's read.
        assert_eq!(
            short_reply_utterance("The pause/resume toggle.").as_deref(),
            Some("The pause/resume toggle.")
        );
        assert_eq!(
            short_reply_utterance("Edit src/main and rebuild.").as_deref(),
            Some("Edit src/main and rebuild.")
        );
    }

    #[test]
    fn short_utt_code_and_url_are_read_whole() {
        // Read everything for now: code-fence backticks are stripped as markers; a URL is
        // kept (no info lost). Genuinely-unspeakable cases can be special-cased later.
        assert_eq!(
            short_reply_utterance("Run ```cargo build``` now").as_deref(),
            Some("Run cargo build now")
        );
        assert_eq!(
            short_reply_utterance("See https://example.com for more").as_deref(),
            Some("See https://example.com for more")
        );
    }

    #[test]
    fn short_utt_strips_markdown_and_collapses_whitespace() {
        assert_eq!(
            short_reply_utterance("Yes, `that` is the **default**.").as_deref(),
            Some("Yes, that is the default.")
        );
        assert_eq!(
            short_reply_utterance("line one\n\n  line   two\ttab").as_deref(),
            Some("line one line two tab")
        );
        assert_eq!(
            short_reply_utterance("# Heading _emph_").as_deref(),
            Some("Heading emph")
        );
    }

    #[test]
    fn speaks_the_leading_line_once_when_complete() {
        let mut a = Accum::default();
        assert!(
            a.feed(0, "Preamble prose.", None, false, true, false)
                .is_empty()
        );
        assert!(
            a.feed(1, "\n\n> The spoken line.", None, false, true, false)
                .is_empty()
        );
        assert_eq!(
            a.feed(2, "\n\nBody.", None, true, true, false),
            vec!["The spoken line."]
        );
        // Any later/duplicate batch is a no-op (the run is past the high-water mark).
        assert!(a.feed(3, " more body.", None, true, true, false).is_empty());
    }

    #[test]
    fn speaks_every_blockquote_in_order_each_once() {
        // The core multi-emit guarantee: a reply with several blockquotes voices ALL of them,
        // in document order, each exactly once, as each run closes.
        let mut a = Accum::default();
        // First run closes when the second `>` block's blank line separates them.
        assert_eq!(
            a.feed(
                0,
                "> One.\n\nbody one.\n\n> Two.\n\n",
                None,
                false,
                true,
                false
            ),
            vec!["One.", "Two."]
        );
        // A later run streams in and closes on the final batch.
        assert!(
            a.feed(1, "more.\n\n> Three.", None, false, true, false)
                .is_empty()
        ); // Three still open
        assert_eq!(
            a.feed(2, "\n\ntail.", None, true, true, false),
            vec!["Three."]
        );
        // Fully drained: nothing re-emits.
        assert!(a.feed(3, " extra.", None, true, true, false).is_empty());
    }

    #[test]
    fn whole_reply_as_one_final_batch_emits_all_blockquotes() {
        // The NON-STREAMING (OpenAI Codex `Stop`) path: the entire reply arrives as ONE final
        // batch (index 0, is_final = true). The same core must emit every top-level blockquote,
        // in order, each once — exactly what the streamed path would, just in one feed.
        let reply = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.\n\n> Closing ask?";
        let mut a = Accum::default();
        assert_eq!(
            a.feed(0, reply, None, true, true, false),
            vec!["First point.", "Second point.", "Closing ask?"]
        );
        // Idempotent: a duplicate final batch re-speaks nothing.
        assert!(a.feed(0, reply, None, true, true, false).is_empty());
    }

    #[test]
    fn whole_blockquoteless_reply_voiced_whole_under_short() {
        // Codex `Stop` with the `short` mode on and NO blockquote: a brief plain reply is
        // voiced whole, once. With `short` OFF it stays silent (messages-only).
        let reply = "Done — all three tests pass.";
        let mut a = Accum::default();
        assert_eq!(
            a.feed(0, reply, None, true, /*messages*/ false, /*short*/ true),
            vec!["Done — all three tests pass."]
        );
        let mut b = Accum::default();
        assert!(b.feed(0, reply, None, true, false, false).is_empty(), "messages-only ⇒ silent");
    }

    #[test]
    fn out_of_order_batches_assemble_correctly() {
        let mut a = Accum::default();
        // Deliver indices 2 (body, final), 0 (preamble), 1 (quote) — reversed.
        assert!(
            a.feed(2, "\n\nBody after.", None, true, true, false)
                .is_empty()
        );
        assert!(
            a.feed(0, "Preamble first.", None, false, true, false)
                .is_empty()
        );
        assert_eq!(
            a.feed(
                1,
                "\n\n> Spoken even out of order.",
                None,
                false,
                true,
                false
            ),
            vec!["Spoken even out of order."]
        );
    }

    #[test]
    fn no_blockquote_is_silent() {
        let mut a = Accum::default();
        assert!(
            a.feed(0, "Just prose, ", None, false, true, false)
                .is_empty()
        );
        assert!(
            a.feed(1, "no spoken line at all.", None, true, true, false)
                .is_empty()
        );
    }

    #[test]
    fn short_mode_speaks_a_blockquoteless_final_reply_once() {
        // With "shorts" on, a final message that has NO blockquote is voiced whole (cleaned),
        // exactly once.
        let mut a = Accum::default();
        assert!(a.feed(0, "Yes, ", None, false, true, true).is_empty()); // not final yet
        assert_eq!(
            a.feed(1, "that's the `default`.", None, true, true, true),
            vec!["Yes, that's the default."] // backticks stripped, whitespace joined
        );
        // Latched: a late duplicate batch never re-speaks it.
        assert!(a.feed(2, " dup", None, true, true, true).is_empty());
    }

    #[test]
    fn short_mode_reads_code_paths_and_long_text_whole() {
        // No guards now: code, paths, and long blockquote-less text are all READ (no info
        // lost) — only the markdown markers are cleaned off.
        assert_eq!(
            Accum::default().feed(0, "Run ```cargo build```", None, true, true, true),
            vec!["Run cargo build"],
            "code fence → read (backticks stripped)"
        );
        assert_eq!(
            Accum::default().feed(0, "See rust/crates/lib.rs now", None, true, true, true),
            vec!["See rust/crates/lib.rs now"],
            "path → read whole"
        );
        let long = "word ".repeat(80); // ~400 chars
        assert_eq!(
            Accum::default().feed(0, &long, None, true, true, true).len(),
            1,
            "long text → read, not silenced"
        );
        // A reply WITH a blockquote is digests-territory: with digests OFF + short ON it
        // still stays silent (shorts only fires when there is NO blockquote at all).
        assert!(
            Accum::default()
                .feed(0, "> Spoken.\n\nbody.", None, true, false, true)
                .is_empty()
        );
    }

    #[test]
    fn cumulative_displayed_text_mode_speaks() {
        let mut a = Accum::default();
        assert!(
            a.feed(0, "", Some("> Spoken."), false, true, false)
                .is_empty()
        );
        assert_eq!(
            a.feed(1, "", Some("> Spoken.\n\nBody."), false, true, false),
            vec!["Spoken."]
        );
    }

    #[test]
    fn final_drains_buffer_but_keeps_high_water_mark() {
        // After the message is final and every run voiced, the buffered parts are freed but a
        // late duplicate batch still emits nothing (the high-water mark persists).
        let mut a = Accum::default();
        assert_eq!(
            a.feed(0, "> Once.\n\nbody", None, true, true, false),
            vec!["Once."]
        );
        assert!(a.parts.is_empty(), "buffer freed once final + drained");
        assert!(
            a.feed(1, " duplicate tail", None, true, true, false)
                .is_empty()
        );
    }
}
