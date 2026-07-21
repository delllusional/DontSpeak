//! Per-message accumulation: which top-level blockquote runs become speakable now.
//! Pure of IO; [`crate::deliver_batch`] owns state file + lock.
//!
//! Out-of-order batches (parallel hooks): reconstruct by content-block `index`, emit
//! each completed run once in document order (high-water mark). Cumulative
//! `displayed_text` covers Qwen / Codex final snapshots.

use std::collections::BTreeMap;

/// Max bytes of detection corpus retained at selection / on the wire. Equality with the
/// engine's per-item speak cap is asserted at compile time in `dontspeakd::ttsq`.
pub const DETECTION_TEXT_MAX_BYTES: usize = 10 * 1024;

/// Prefix-truncate to `max` bytes on a char boundary (same semantics as the speak limit).
pub fn cap_detection_text(s: String) -> String {
    if s.len() <= DETECTION_TEXT_MAX_BYTES {
        return s;
    }
    let mut end = DETECTION_TEXT_MAX_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// One newly speakable run plus the reconstructed message-so-far for language detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedUtterance {
    pub text: String,
    /// Turn text so far for language detection, capped at [`DETECTION_TEXT_MAX_BYTES`].
    pub detection_text: String,
}

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
    ) -> Vec<SelectedUtterance> {
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
        // Cap once per feed so pending state and IPC never carry multi-MB corpora.
        let detection_corpus = cap_detection_text(cumulative.clone());
        if messages_on {
            for (text, _) in runs.into_iter().take(speakable).skip(self.emitted) {
                spoken.push(SelectedUtterance {
                    text,
                    detection_text: detection_corpus.clone(),
                });
            }
        }
        self.emitted = speakable.max(self.emitted);

        // Final + no blockquote at all → voice whole once (latched).
        if short_on && self.seen_final && total == 0 && !self.short_done {
            self.short_done = true;
            if !cumulative.trim().is_empty() {
                spoken.push(SelectedUtterance {
                    text: cumulative,
                    detection_text: detection_corpus,
                });
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

    fn texts(selected: &[SelectedUtterance]) -> Vec<&str> {
        selected.iter().map(|u| u.text.as_str()).collect()
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
        let out = a.feed(2, "\n\nBody.", None, true, true, false);
        assert_eq!(texts(&out), ["The spoken line."]);
        assert_eq!(
            out[0].detection_text,
            "Preamble prose.\n\n> The spoken line.\n\nBody."
        );
        // Past high-water mark → no-op.
        assert!(a.feed(3, " more body.", None, true, true, false).is_empty());
    }

    #[test]
    fn speaks_every_blockquote_in_order_each_once() {
        // Multi-emit: every blockquote, document order, each once as it closes.
        let mut a = Accum::default();
        let first = a.feed(
            0,
            "> One.\n\nbody one.\n\n> Two.\n\n",
            None,
            false,
            true,
            false,
        );
        assert_eq!(texts(&first), ["One.", "Two."]);
        assert_eq!(first[0].detection_text, "> One.\n\nbody one.\n\n> Two.\n\n");
        assert_eq!(first[1].detection_text, first[0].detection_text);
        assert!(
            a.feed(1, "more.\n\n> Three.", None, false, true, false)
                .is_empty()
        ); // Three still open
        let third = a.feed(2, "\n\ntail.", None, true, true, false);
        assert_eq!(texts(&third), ["Three."]);
        assert!(
            third[0]
                .detection_text
                .contains("> One.\n\nbody one.\n\n> Two.\n\nmore.\n\n> Three.\n\ntail.")
        );
        assert!(a.feed(3, " extra.", None, true, true, false).is_empty());
    }

    #[test]
    fn detection_text_is_capped_at_selection() {
        let mut a = Accum::default();
        let huge = format!(
            "{}{}",
            "E".repeat(DETECTION_TEXT_MAX_BYTES + 500),
            "\n\n> Quote."
        );
        let out = a.feed(0, &huge, None, true, true, false);
        assert_eq!(texts(&out), ["Quote."]);
        assert!(out[0].detection_text.len() <= DETECTION_TEXT_MAX_BYTES);
        assert!(
            out[0]
                .detection_text
                .is_char_boundary(out[0].detection_text.len())
        );
    }

    #[test]
    fn detection_text_grows_monotonically_across_quotes() {
        let mut a = Accum::default();
        let first = a.feed(
            0,
            "English preamble with enough body to classify.\n\n> Short digest one.",
            None,
            false,
            true,
            false,
        );
        assert!(first.is_empty(), "open quote waits for close");
        let first = a.feed(
            1,
            "\n\nMore English body for context.\n\n",
            None,
            false,
            true,
            false,
        );
        assert_eq!(texts(&first), ["Short digest one."]);
        let first_det = first[0].detection_text.clone();
        let second = a.feed(
            2,
            "> Short digest two.\n\nClosing English prose for the turn.",
            None,
            true,
            true,
            false,
        );
        assert_eq!(texts(&second), ["Short digest two."]);
        assert!(
            second[0].detection_text.starts_with(&first_det)
                || second[0].detection_text.len() >= first_det.len(),
            "second quote's detection corpus is fuller so-far"
        );
        assert!(second[0].detection_text.contains("Short digest two."));
        assert!(second[0].detection_text.contains("Closing English prose"));
    }

    #[test]
    fn whole_reply_as_one_final_batch_emits_all_blockquotes() {
        // Non-streaming (`Stop`): one final batch must match the streamed multi-emit.
        let reply = "> First point.\n\nDetail.\n\n> Second point.\n\nMore.\n\n> Closing ask?";
        let mut a = Accum::default();
        let out = a.feed(0, reply, None, true, true, false);
        assert_eq!(
            texts(&out),
            ["First point.", "Second point.", "Closing ask?"]
        );
        assert!(out.iter().all(|u| u.detection_text == reply));
        assert!(a.feed(0, reply, None, true, true, false).is_empty());
    }

    #[test]
    fn whole_blockquoteless_reply_voiced_whole_under_short() {
        // Short on + no blockquote → whole once; short off → silent.
        let reply = "Done — all three tests pass.";
        let mut a = Accum::default();
        let out = a.feed(
            0, reply, None, true, /*messages*/ false, /*short*/ true,
        );
        assert_eq!(texts(&out), [reply]);
        assert_eq!(out[0].detection_text, reply);
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
        let out = a.feed(
            1,
            "\n\n> Spoken even out of order.",
            None,
            false,
            true,
            false,
        );
        assert_eq!(texts(&out), ["Spoken even out of order."]);
        // Early final batch frees `parts` when no quotes yet, so later assembly only
        // has chunks that arrived after that free — detection still tracks so-far.
        assert!(out[0].detection_text.contains("Spoken even out of order."));
        assert!(!out[0].detection_text.is_empty());
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
        let out = a.feed(1, "that's the `default`.", None, true, true, true);
        assert_eq!(texts(&out), ["Yes, that's the `default`."]);
        assert_eq!(out[0].detection_text, "Yes, that's the `default`.");
        // Latched against late duplicates.
        assert!(a.feed(2, " dup", None, true, true, true).is_empty());
    }

    #[test]
    fn short_mode_reads_code_paths_and_long_text_whole() {
        // Selection preserves content; the single TTS frontend owns prose cleanup.
        let code = Accum::default().feed(0, "Run ```cargo build```", None, true, true, true);
        assert_eq!(texts(&code), ["Run ```cargo build```"]);
        let path = Accum::default().feed(0, "See rust/crates/lib.rs now", None, true, true, true);
        assert_eq!(texts(&path), ["See rust/crates/lib.rs now"]);
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
    fn digest_and_short_paths_forward_identical_text() {
        let raw = "Fixed `MainWindow.swift` at commit eedfc57.";

        let digest_out = Accum::default().feed(0, &format!("> {raw}"), None, true, true, false);
        let short_out = Accum::default().feed(0, raw, None, true, true, true);

        assert_eq!(texts(&digest_out), texts(&short_out));
        assert_eq!(texts(&digest_out), [raw]);
    }

    #[test]
    fn cumulative_displayed_text_mode_speaks() {
        let mut a = Accum::default();
        assert!(
            a.feed(0, "", Some("> Spoken."), false, true, false)
                .is_empty()
        );
        let out = a.feed(1, "", Some("> Spoken.\n\nBody."), false, true, false);
        assert_eq!(texts(&out), ["Spoken."]);
        assert_eq!(out[0].detection_text, "> Spoken.\n\nBody.");
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
        let out = a.feed(0, "> Once.\n\nbody", None, true, true, false);
        assert_eq!(texts(&out), ["Once."]);
        assert!(a.parts.is_empty(), "buffer freed once final + drained");
        assert!(
            a.feed(1, " duplicate tail", None, true, true, false)
                .is_empty()
        );
    }
}
