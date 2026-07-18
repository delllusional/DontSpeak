//! Per-message accumulation: which top-level blockquote runs become speakable now.
//! PURE of IO (unit-testable); [`crate::deliver_batch`] adds the state file + lock.
//!
//! Claude Code can deliver batches out of order (hooks run in parallel). [`Accum`]
//! reconstructs by content-block `index` and emits each completed run EXACTLY ONCE in
//! document order (high-water mark, not a one-shot latch). Cumulative `displayed_text`
//! mode covers Qwen / Codex final snapshots.

use std::collections::BTreeMap;

/// Feed batches → newly speakable blockquote runs. One per session under a lock.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Accum {
    /// Chunks by content-block `index` — reconstructs regardless of arrival order.
    pub parts: BTreeMap<u64, String>,
    /// Sticky: any `final=true` makes the message final (even if that batch arrived early).
    pub seen_final: bool,
    /// High-water mark of runs already forwarded; re-fed batches advance nothing.
    pub emitted: usize,
    /// "Shorts" latch: blockquote-less final reply voiced whole, once.
    pub short_done: bool,
}

impl Accum {
    /// Newly speakable runs this batch (usually empty mid-run). Cumulative
    /// `displayed_text` wins over delta reconstruction when present.
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
            Some(dt) if !dt.trim().is_empty() => {
                // Codex ends deltas with one authoritative final snapshot. Earlier
                // cumulative would leave `parts` stale if another delta followed.
                debug_assert!(
                    self.parts.is_empty() || is_final,
                    "a non-final cumulative payload must not follow delta payloads"
                );
                dt.to_string()
            }
            _ => {
                self.parts.insert(index, delta.to_string());
                self.parts.values().map(String::as_str).collect::<String>()
            }
        };

        // Speakable prefix = leading complete runs (only the last can still be open).
        // Advance the high-water mark even when `messages_on` is false so shorts can
        // tell "no blockquote" from "blockquotes, but muted".
        let runs = ds_config::all_blockquotes_state(&cumulative, self.seen_final);
        let total = runs.len();
        let speakable = runs.iter().take_while(|(_, complete)| *complete).count();
        let mut spoken = Vec::new();
        if messages_on {
            for (text, _) in runs.into_iter().take(speakable).skip(self.emitted) {
                if let Some(text) = ds_config::clean_for_speech(&text) {
                    spoken.push(text);
                }
            }
        }
        self.emitted = speakable.max(self.emitted);

        // Final + no blockquote at all → voice whole once (latched).
        if short_on && self.seen_final && total == 0 && !self.short_done {
            self.short_done = true;
            if let Some(utt) = ds_config::clean_for_speech(&cumulative) {
                spoken.push(utt);
            }
        }

        // Free buffer when done; high-water mark stays so late duplicates stay silent.
        if self.seen_final && self.emitted >= total {
            self.parts.clear();
        }
        spoken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shorts-path fixtures for `ds_config::clean_for_speech`: read WHOLE, only
    // empty/markers-only dropped. (Hash-token and backtick-path cases live in
    // `ds_config::narration`'s own tests, next to the function.)

    #[test]
    fn short_utt_plain_text_passes_trimmed() {
        assert_eq!(
            ds_config::clean_for_speech("  Hello there.  ").as_deref(),
            Some("Hello there.")
        );
    }

    #[test]
    fn short_utt_empty_or_whitespace_is_none() {
        assert_eq!(ds_config::clean_for_speech(""), None);
        assert_eq!(ds_config::clean_for_speech("   \n\t "), None);
    }

    #[test]
    fn short_utt_markers_only_becomes_none() {
        assert_eq!(ds_config::clean_for_speech("***"), None);
        assert_eq!(ds_config::clean_for_speech("###  _ "), None);
    }

    #[test]
    fn short_utt_long_text_is_read_whole() {
        // No length cap.
        let long = "word ".repeat(120); // ~600 chars
        let spoken = ds_config::clean_for_speech(&long).expect("long text is read");
        assert!(spoken.starts_with("word word"));
        assert!(spoken.chars().count() > 320, "not truncated/silenced");
    }

    #[test]
    fn short_utt_slashed_word_is_read_whole() {
        // Regression: slash must not silence the reply.
        assert_eq!(
            ds_config::clean_for_speech("The pause/resume toggle.").as_deref(),
            Some("The pause/resume toggle.")
        );
        assert_eq!(
            ds_config::clean_for_speech("Edit src/main and rebuild.").as_deref(),
            Some("Edit src/main and rebuild.")
        );
    }

    #[test]
    fn short_utt_code_and_url_are_read_whole() {
        // Backticks stripped as markers; URL kept.
        assert_eq!(
            ds_config::clean_for_speech("Run ```cargo build``` now").as_deref(),
            Some("Run cargo build now")
        );
        assert_eq!(
            ds_config::clean_for_speech("See https://example.com for more").as_deref(),
            Some("See https://example.com for more")
        );
    }

    #[test]
    fn short_utt_strips_markdown_and_collapses_whitespace() {
        assert_eq!(
            ds_config::clean_for_speech("Yes, `that` is the **default**.").as_deref(),
            Some("Yes, that is the default.")
        );
        assert_eq!(
            ds_config::clean_for_speech("line one\n\n  line   two\ttab").as_deref(),
            Some("line one line two tab")
        );
        assert_eq!(
            ds_config::clean_for_speech("# Heading _emph_").as_deref(),
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
        // Past high-water mark → no-op.
        assert!(a.feed(3, " more body.", None, true, true, false).is_empty());
    }

    #[test]
    fn speaks_every_blockquote_in_order_each_once() {
        // Multi-emit: every blockquote, document order, each once as it closes.
        let mut a = Accum::default();
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
        assert!(
            a.feed(1, "more.\n\n> Three.", None, false, true, false)
                .is_empty()
        ); // Three still open
        assert_eq!(
            a.feed(2, "\n\ntail.", None, true, true, false),
            vec!["Three."]
        );
        assert!(a.feed(3, " extra.", None, true, true, false).is_empty());
    }

    #[test]
    fn whole_reply_as_one_final_batch_emits_all_blockquotes() {
        // Non-streaming (`Stop`): one final batch must match the streamed multi-emit.
        let reply = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.\n\n> Closing ask?";
        let mut a = Accum::default();
        assert_eq!(
            a.feed(0, reply, None, true, true, false),
            vec!["First point.", "Second point.", "Closing ask?"]
        );
        assert!(a.feed(0, reply, None, true, true, false).is_empty());
    }

    #[test]
    fn whole_blockquoteless_reply_voiced_whole_under_short() {
        // Short on + no blockquote → whole once; short off → silent.
        let reply = "Done — all three tests pass.";
        let mut a = Accum::default();
        assert_eq!(
            a.feed(
                0, reply, None, true, /*messages*/ false, /*short*/ true
            ),
            vec!["Done — all three tests pass."]
        );
        let mut b = Accum::default();
        assert!(
            b.feed(0, reply, None, true, false, false).is_empty(),
            "messages-only ⇒ silent"
        );
    }

    #[test]
    fn out_of_order_batches_assemble_correctly() {
        let mut a = Accum::default();
        // Indices 2, 0, 1 — reversed arrival.
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
    fn feed_prose_only_emits_nothing() {
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
        let mut a = Accum::default();
        assert!(a.feed(0, "Yes, ", None, false, true, true).is_empty()); // not final yet
        assert_eq!(
            a.feed(1, "that's the `default`.", None, true, true, true),
            vec!["Yes, that's the default."]
        );
        // Latched against late duplicates.
        assert!(a.feed(2, " dup", None, true, true, true).is_empty());
    }

    #[test]
    fn short_mode_reads_code_paths_and_long_text_whole() {
        // No content guards — only markdown markers cleaned.
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
            Accum::default()
                .feed(0, &long, None, true, true, true)
                .len(),
            1,
            "long text → read, not silenced"
        );
        // Shorts only when there is NO blockquote at all.
        assert!(
            Accum::default()
                .feed(0, "> Spoken.\n\nbody.", None, true, false, true)
                .is_empty()
        );
    }

    #[test]
    fn digest_and_short_paths_apply_identical_cleanup() {
        // Regression: the digest path used to be `.trim()`-only while shorts stripped
        // markers + hash-like tokens — the two cleanups could drift. Both now delegate to
        // `ds_config::clean_for_speech`; assert that delegation holds for the same content
        // reached via either path (as a blockquote vs. blockquote-less final reply).
        let raw = "Fixed `MainWindow.swift` at commit eedfc57.";

        let digest_out = Accum::default().feed(0, &format!("> {raw}"), None, true, true, false);
        let short_out = Accum::default().feed(0, raw, None, true, true, true);

        assert_eq!(digest_out, short_out);
        // Trailing "." was attached to the dropped hash token ("eedfc57."), so it goes too.
        assert_eq!(digest_out, vec!["Fixed MainWindow.swift at commit"]);
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
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a non-final cumulative payload must not follow delta payloads")]
    fn non_final_cumulative_payload_after_deltas_violates_debug_invariant() {
        let mut a = Accum::default();
        assert!(a.feed(0, "> Partial", None, false, true, false).is_empty());
        let _ = a.feed(0, "", Some("> Partial\n\nBody."), false, true, false);
    }

    #[test]
    fn final_drains_buffer_but_keeps_high_water_mark() {
        // Free buffer when drained; high-water mark still silences late duplicates.
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
