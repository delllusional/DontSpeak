//! Model-bounded phoneme batching for Kokoro synthesis.
//!
//! [`split_phonemes`](crate::batch::split_phonemes) packs a long phoneme string at sentence
//! marks under `MAX_PHONEME_LENGTH` (port of `splitPhonemes`);
//! [`stream_batches`](crate::batch::stream_batches) is the ramped variant used by the shared
//! frontend. The helper now prepares its complete sequence transactionally before playback;
//! the API name records the earlier concurrent-playback design. Both share `pack_batches`.

use crate::vocab::MAX_PHONEME_LENGTH;

/// The sentence/clause marks batches may break at — the split set from
/// `Tokenizer.kt::splitPhonemes` / kokoro-onnx `_split_phonemes`.
const SPLIT_CHARS: &[char] = &['.', ',', '!', '?', ';'];

/// Split a phoneme string at `.,!?;` into interleaved `[text, mark, text, …]`
/// atomic parts — `re.split(r"([.,!?;])", s)`. A "lone mark" part glues to its
/// preceding chunk during batching; this never breaks mid-clause.
fn atomic_parts(phonemes: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in phonemes.chars() {
        if SPLIT_CHARS.contains(&ch) {
            parts.push(std::mem::take(&mut current));
            parts.push(ch.to_string());
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}

pub fn split_phonemes(phonemes: &str) -> Vec<String> {
    // Constant model cap (no ramp), and DON'T break early at every sentence — pack
    // greedily so multiple sentences share a batch, preserving the inter-sentence
    // pauses (each batch is trimmed only at its ends). A forced break at the cap
    // still backtracks to the last sentence boundary, and a short trailing
    // remainder is still folded back — the same squeak guard as the stream path.
    //
    // `pack_batches` only ever forces a break BETWEEN `.,!?;`-delimited atomic
    // parts (see `atomic_parts`); a run with NO such mark at all (e.g. a long
    // digit string expanded by `numbers::expand_numbers` into an unpunctuated
    // word run) is a single atomic part and survives packing oversized. Guard
    // with the same hard word/char-split fallback the streaming path uses, so no
    // batch handed to the ONNX/Core ML engines can ever exceed the cap that
    // gets it silently truncated (ONNX) or dropped whole (Core ML/ANE).
    pack_batches(
        phonemes,
        MAX_PHONEME_LENGTH,
        MAX_PHONEME_LENGTH,
        MIN_PHONEME_LENGTH,
        |b| b, // never grows past the cap it starts at
        false, // no early strong-boundary flush (keep packing to the cap)
    )
    .into_iter()
    .flat_map(|b| hard_split_words(&b, MAX_PHONEME_LENGTH, MIN_PHONEME_LENGTH))
    .collect()
}

/// Last-resort split of an over-cap chunk/batch at WORD boundaries (spaces), so nothing
/// exceeds `cap` even when it has no `.,!?;` to break on. A single word longer than `cap`
/// is split at a char boundary — degraded but never dropped/truncated. Split results also stay
/// above `floor`; an in-cap whole reply remains unchanged even when it is shorter than the floor.
fn hard_split_words(s: &str, cap: usize, floor: usize) -> Vec<String> {
    if s.chars().count() <= cap {
        return vec![s.to_string()];
    }
    debug_assert!(floor > 0 && floor <= cap.div_ceil(2));

    // Match the previous word packer's whitespace normalization before choosing
    // boundaries. A space used as a batch boundary is omitted as before.
    let normalized = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars = normalized.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Vec::new();
    }
    if chars.len() <= cap {
        return vec![normalized];
    }

    let mut out = Vec::new();
    let mut start = 0;
    while chars.len() - start > cap {
        let remaining = chars.len() - start;
        let split_at = if remaining <= cap.saturating_mul(2) {
            // On the final split, choose a balanced word boundary when possible.
            // Otherwise bisect a long word so neither side becomes a short tail.
            let midpoint = remaining / 2;
            (floor..remaining)
                .filter(|&at| chars[start + at] == ' ')
                .filter(|&at| {
                    let right = remaining - at - 1;
                    at <= cap && (floor..=cap).contains(&right)
                })
                .min_by_key(|&at| at.abs_diff(midpoint))
                .unwrap_or(midpoint)
        } else {
            // More than two chunks remain, so pack to the last usable word boundary.
            (floor..=cap)
                .rev()
                .find(|&at| chars[start + at] == ' ')
                .unwrap_or(cap)
        };

        let end = start + split_at;
        out.push(chars[start..end].iter().collect());
        start = end + usize::from(chars[end] == ' ');
    }
    out.push(chars[start..].iter().collect());
    out
}

/// Shared batching core for [`split_phonemes`] and [`stream_batches`]. Packs
/// `.,!?;`-delimited parts into batches whose length stays under a `budget` that
/// starts at `budget0` and is advanced by `grow` after each flush. Always
/// applies the floor (never flush below [`MIN_PHONEME_LENGTH`]), backtracks a
/// forced (cap) break to the last sentence boundary when that head clears the
/// floor, and folds a short trailing remainder into the previous batch. With
/// `break_at_strong`, it ALSO ends a batch early at a sentence-final mark once it
/// clears the floor (whole-sentence batches for the streaming ramp); without it,
/// batches grow to the cap (packing the non-streaming path).
fn pack_batches(
    phonemes: &str,
    budget0: usize,
    hard_cap: usize,
    floor: usize,
    grow: impl Fn(usize) -> usize,
    break_at_strong: bool,
) -> Vec<String> {
    let parts = atomic_parts(phonemes);

    let mut batched: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut budget = budget0;
    // Byte offset in `current` just past the last sentence-final mark (`.!?`), or
    // `None` if there isn't one yet — the point a forced break prefers to cut at.
    let mut strong_at: Option<usize> = None;

    for raw_part in parts {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        // Length comparisons use char counts (the source uses UTF-16 units, but
        // the cap is a coarse safety bound; char count is the closest portable
        // analog and never under-batches dangerously).
        let part_len = part.chars().count();
        let cur_len = current.chars().count();
        // Hard cap is always the model context; the ramp only ever lowers it.
        let cap = budget.min(hard_cap);
        let is_lone_mark = part_len == 1 && SPLIT_CHARS.contains(&part.chars().next().unwrap());

        // FORCED break: appending would exceed the cap. Gated on the floor
        // (`cur_len >= MIN`) so we never flush a too-short batch — a sub-floor
        // `current` keeps accumulating past the cap instead of emitting a fragment.
        // A lone mark NEVER forces a break: it's one token that belongs to the
        // preceding clause (splitting it off would orphan the sentence's period).
        if !is_lone_mark && !current.is_empty() && cur_len >= floor && cur_len + part_len + 1 >= cap
        {
            // Prefer to cut at the last sentence boundary (Kokoro `waterfall_last`),
            // carrying the trailing clause forward — but only if the head is itself
            // ≥ floor, else a tiny first sentence would become a squeaky fragment.
            // With no usable strong boundary, take the (≥ floor) weak break.
            match strong_at {
                Some(idx)
                    if idx < current.len() && current[..idx].trim().chars().count() >= floor =>
                {
                    batched.push(current[..idx].trim().to_string());
                    current = current[idx..].trim().to_string();
                }
                _ => {
                    batched.push(current.trim().to_string());
                    current.clear();
                }
            }
            strong_at = None;
            budget = grow(budget);
            // Fall through to append `part` to the (possibly carried) `current`.
        }

        // A lone split-mark glues with no space; an empty `current` takes the
        // part as-is; otherwise prefix a single space.
        if !is_lone_mark && !current.is_empty() {
            current.push(' ');
        }
        current.push_str(part);
        // Record a sentence-final boundary at the end of `current`.
        if is_lone_mark && STRONG_MARKS.contains(&part.chars().next().unwrap()) {
            strong_at = Some(current.len());
        }

        // PREFERRED break (streaming only): once the batch has reached the floor,
        // end it at a sentence-final mark (favoring `.!?` over `,;`), so batches
        // are whole sentences rather than mid-clause comma fragments.
        if break_at_strong && current.chars().count() >= floor && ends_at_strong_boundary(&current)
        {
            batched.push(current.trim().to_string());
            current.clear();
            strong_at = None;
            budget = grow(budget);
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        batched.push(trimmed.to_string());
    }

    // Fold a short trailing remainder into the previous batch so the LAST words —
    // the most audible — are never a tiny, high-pitched fragment. The flush rules
    // above already keep every earlier batch ≥ floor, so only the tail can be
    // short. Skip if the merge would overflow the model context (rare).
    if batched.len() >= 2 {
        let last_len = batched[batched.len() - 1].chars().count();
        let prev_len = batched[batched.len() - 2].chars().count();
        if last_len < floor && prev_len + 1 + last_len <= hard_cap {
            let tail = batched.pop().unwrap();
            let prev = batched.last_mut().unwrap();
            prev.push(' ');
            prev.push_str(&tail);
        }
    }
    batched
}

/// First ramped batch budget (phonemes). Subsequent synthesis units grow geometrically up to
/// `MAX_PHONEME_LENGTH`; retaining these boundaries avoids changing established style-row and
/// short-tail behavior while the helper's playback commit is transactional.
pub const STREAM_FIRST_BUDGET: usize = 80;
/// Per-batch growth factor retained from the original streaming sequence.
const STREAM_GROWTH_NUM: usize = 7; // 1.4 = 7/5
const STREAM_GROWTH_DEN: usize = 5;

/// Floor on a streaming batch's length (phonemes). A batch shorter than this —
/// especially a trailing fragment like "Got it." — makes Kokoro select a
/// short-utterance style row (indexed by token count; see `synth::style_row`),
/// which compresses durations and renders the words high-pitched / "choked". The
/// reference Kokoro pipelines avoid this structurally by packing chunks to the
/// model cap and never emitting tiny ones; our low-latency ramp can, so we (a)
/// never FLUSH a batch below this floor and (b) fold a short trailing remainder
/// back into the previous batch. Kept ≤ [`STREAM_FIRST_BUDGET`] so the first ramped
/// unit can still reach the floor. Tuned by ear.
const MIN_PHONEME_LENGTH: usize = 64;
const _: () = assert!(
    MIN_PHONEME_LENGTH <= STREAM_FIRST_BUDGET,
    "the min-batch floor must not exceed the first-batch budget"
);

/// Sentence-final marks — the PREFERRED batch boundaries (Kokoro `waterfall_last`
/// breaks on these before the weaker `,;`). A batch ending here is a complete
/// clause/sentence and renders with natural prosody; a batch that ends mid-clause
/// at a comma does not. (`,` and `;` from [`SPLIT_CHARS`] are the weak fallbacks,
/// used only when a run has no strong mark before the cap.)
const STRONG_MARKS: &[char] = &['.', '!', '?'];

/// Whether `s` ends at a sentence-final mark (ignoring trailing whitespace).
fn ends_at_strong_boundary(s: &str) -> bool {
    s.trim_end()
        .chars()
        .next_back()
        .is_some_and(|c| STRONG_MARKS.contains(&c))
}

/// Split a phoneme string into the established ramped sequence: a small first batch growing
/// geometrically to `MAX_PHONEME_LENGTH`. It uses the same `.,!?;` boundaries as
/// [`split_phonemes`] and never breaks mid-clause. The helper stages the returned sequence before
/// playback; callers should still pass the whole reply so the short-tail protections see every
/// boundary.
pub fn stream_batches(phonemes: &str) -> Vec<String> {
    // Ramped cap (small first batch, growing geometrically) AND early
    // strong-boundary flushing, so each batch is a whole sentence delivered fast.
    //
    // Same hard-cap guarantee as `split_phonemes`: an unpunctuated run longer than
    // the cap is one atomic part and would otherwise survive packing oversized, so
    // run every batch through the same hard word/char-split fallback before it can
    // reach either synthesis engine.
    pack_batches(
        phonemes,
        STREAM_FIRST_BUDGET,
        MAX_PHONEME_LENGTH,
        MIN_PHONEME_LENGTH,
        |b| (b * STREAM_GROWTH_NUM / STREAM_GROWTH_DEN).min(MAX_PHONEME_LENGTH),
        true,
    )
    .into_iter()
    .flat_map(|b| hard_split_words(&b, MAX_PHONEME_LENGTH, MIN_PHONEME_LENGTH))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_phonemes_keeps_short_string_as_one_batch() {
        let batches = split_phonemes("həlˈO wˈɜɹld");
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], "həlˈO wˈɜɹld");
    }

    #[test]
    fn split_phonemes_glues_marks_to_preceding_chunk() {
        // "a. b" → mark '.' glues to "a", then "b" joins with a space.
        let batches = split_phonemes("a. b");
        assert_eq!(batches, vec!["a. b".to_string()]);
        // A trailing question mark glues too.
        let q = split_phonemes("hi? there");
        assert_eq!(q, vec!["hi? there".to_string()]);
    }

    #[test]
    fn split_phonemes_empty_yields_no_batches() {
        assert!(split_phonemes("").is_empty());
        assert!(split_phonemes("   ").is_empty());
        // Only marks → trimmed away (each becomes a lone glued mark then trimmed).
        let only_marks = split_phonemes("...");
        // "..." → three '.' parts each glued; result is a single "..." batch.
        assert_eq!(only_marks, vec!["...".to_string()]);
    }

    #[test]
    fn split_phonemes_hard_splits_a_long_run_with_no_punctuation() {
        // THE failure mode a punctuation-only packer can't catch: one long unpunctuated
        // run (e.g. digits expanded by `numbers::expand_numbers` into a word run with no
        // `.,!?;`) is a single atomic part and would survive `pack_batches` oversized —
        // silently truncated on ONNX or dropped whole on Core ML/ANE. The hard word-split
        // fallback must still bound every batch.
        let run = "wʌn tu θɹiː foːɹ faɪv sɪks sɛvn eɪt naɪn tɛn ".repeat(20); // ~460 chars, no marks
        assert!(run.chars().count() > MAX_PHONEME_LENGTH);
        let batches = split_phonemes(&run);
        assert!(
            batches.len() >= 2,
            "an over-cap unpunctuated run must still split"
        );
        for b in &batches {
            assert!(
                b.chars().count() <= MAX_PHONEME_LENGTH,
                "unpunctuated batch over cap: {}",
                b.chars().count()
            );
        }
        assert_eq!(
            batches.join(" ").split_whitespace().collect::<Vec<_>>(),
            run.split_whitespace().collect::<Vec<_>>(),
            "no phonemes lost in the hard split"
        );
    }

    #[test]
    fn stream_batches_hard_splits_a_long_run_with_no_punctuation() {
        // Same failure mode, streaming path: the ramped budget still packs an unpunctuated
        // run into one oversized atomic part without the hard-split fallback.
        let run = "wʌn tu θɹiː foːɹ faɪv sɪks sɛvn eɪt naɪn tɛn ".repeat(20);
        assert!(run.chars().count() > MAX_PHONEME_LENGTH);
        let batches = stream_batches(&run);
        assert!(
            batches.len() >= 2,
            "an over-cap unpunctuated run must still split"
        );
        for b in &batches {
            assert!(
                b.chars().count() <= MAX_PHONEME_LENGTH,
                "unpunctuated batch over cap: {}",
                b.chars().count()
            );
        }
    }

    #[test]
    fn exact_model_context_is_split_below_the_missing_style_row() {
        // Kokoro advertises a 510-phoneme context, but the copied voice pack has rows 0..=509
        // indexed by token count. A 510-token input therefore has no style row and must split
        // without leaving a one-token tail that selects degenerate short-utterance prosody.
        let exact_context = "ə".repeat(510);
        for batches in [
            split_phonemes(&exact_context),
            stream_batches(&exact_context),
        ] {
            assert_eq!(
                batches
                    .iter()
                    .map(|batch| batch.chars().count())
                    .collect::<Vec<_>>(),
                vec![255, 255]
            );
            assert!(batches.iter().all(|batch| {
                let len = batch.chars().count();
                (MIN_PHONEME_LENGTH..=MAX_PHONEME_LENGTH).contains(&len)
            }));
            assert_eq!(batches.concat(), exact_context);
        }
    }

    #[test]
    fn hard_split_rebalances_a_short_final_word() {
        // The final word cannot fold into the preceding cap-sized chunk, so both sides
        // must be rebalanced instead of leaving the final phoneme alone.
        let phonemes = format!("{} {} c", "a".repeat(250), "b".repeat(258));
        assert_eq!(phonemes.chars().count(), MAX_PHONEME_LENGTH + 2);

        let batches = hard_split_words(&phonemes, MAX_PHONEME_LENGTH, MIN_PHONEME_LENGTH);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.chars().count())
                .collect::<Vec<_>>(),
            vec![250, 260]
        );
        assert!(batches.iter().all(|batch| {
            let len = batch.chars().count();
            (MIN_PHONEME_LENGTH..=MAX_PHONEME_LENGTH).contains(&len)
        }));
        assert_eq!(
            batches.join(" ").split_whitespace().collect::<Vec<_>>(),
            phonemes.split_whitespace().collect::<Vec<_>>()
        );
    }

    #[test]
    fn split_phonemes_breaks_a_very_long_run() {
        // Build a phoneme string well over MAX_PHONEME_LENGTH with sentence marks.
        let sentence = "ə".repeat(200);
        let long = format!("{sentence}. {sentence}. {sentence}.");
        let batches = split_phonemes(&long);
        assert!(batches.len() >= 2, "expected the long run to split");
        for b in &batches {
            assert!(
                b.chars().count() < MAX_PHONEME_LENGTH,
                "every batch must be under the cap"
            );
        }
    }

    #[test]
    fn split_phonemes_packs_multiple_sentences_and_merges_short_tail() {
        // Several whole sentences plus a tiny closer. Unlike the streaming path,
        // split_phonemes does NOT break early at every sentence — it packs them
        // (preserving inter-sentence pauses), so the count stays small...
        let sentence = "ə".repeat(40);
        let body = std::iter::repeat_n(sentence, 6)
            .collect::<Vec<_>>()
            .join(". ");
        let one = stream_batches(&body); // streaming splits per sentence
        let packed = split_phonemes(&body); // packing keeps them together
        assert!(
            packed.len() < one.len(),
            "split_phonemes should pack more per batch than the streaming ramp: \
             packed={} stream={}",
            packed.len(),
            one.len()
        );
        // ...and a short trailing sentence is folded in, never left to squeak.
        let with_tail = format!("{body}. ɡɑt ɪt.");
        let batches = split_phonemes(&with_tail);
        for b in &batches {
            assert!(
                b.chars().count() >= MIN_PHONEME_LENGTH,
                "no split_phonemes batch may be below the floor; got {} ({b:?})",
                b.chars().count()
            );
        }
        assert!(
            batches.last().unwrap().ends_with("ɡɑt ɪt."),
            "the short tail must survive, merged into the final batch"
        );
    }

    #[test]
    fn stream_batches_short_input_is_one_batch() {
        let b = stream_batches("hɛˈloʊ wˈɜːld.");
        assert_eq!(b.len(), 1, "a short reply needs no streaming split");
    }

    #[test]
    fn stream_batches_ramps_small_first_then_grows_under_cap() {
        // COMMA-separated clauses (weak boundaries) so the budget ramp — not the
        // strong-boundary preference — drives the splits, exercising the growth.
        let clause = "ə".repeat(40);
        let long = std::iter::repeat_n(clause, 40)
            .collect::<Vec<_>>()
            .join(", ");
        let batches = stream_batches(&long);
        assert!(batches.len() >= 3, "expected a ramped multi-batch split");
        // First batch stays within the ramp's first-budget plus one atomic part.
        assert!(
            batches[0].chars().count() <= STREAM_FIRST_BUDGET + 41,
            "first batch must stay within the ramp budget, got {}",
            batches[0].chars().count()
        );
        // Batches grow (each ≥ the previous) up to the cap; the FINAL batch is
        // just the remainder and may be shorter, so exclude it from the ramp check.
        let n = batches.len();
        for w in batches[..n - 1].windows(2) {
            assert!(
                w[1].chars().count() + 1 >= w[0].chars().count(),
                "ramp must be non-decreasing until the cap"
            );
        }
        for b in &batches {
            assert!(b.chars().count() < MAX_PHONEME_LENGTH);
        }
    }

    #[test]
    fn stream_batches_never_emits_a_below_floor_batch_except_a_lone_whole_reply() {
        // A long body of sentences plus a TINY final sentence ("Got it."-sized).
        let body = std::iter::repeat_n("ə".repeat(50), 12)
            .collect::<Vec<_>>()
            .join(". ");
        let long = format!("{body}. ɡɑt ɪt.");
        let batches = stream_batches(&long);
        assert!(
            batches.len() >= 2,
            "the long body must split into many batches"
        );
        // EVERY batch is at or above the floor — the tiny "ɡɑt ɪt." tail was folded
        // into the previous batch, not left to squeak on its own.
        for b in &batches {
            assert!(
                b.chars().count() >= MIN_PHONEME_LENGTH,
                "no batch may be below the floor; got {} ({b:?})",
                b.chars().count()
            );
        }
        // The folded tail still ends the LAST batch (nothing dropped).
        assert!(
            batches.last().unwrap().ends_with("ɡɑt ɪt."),
            "the short tail must survive, merged into the final batch"
        );
    }

    #[test]
    fn stream_batches_prefers_strong_boundaries_over_commas() {
        // Sentences LONGER than the floor, each with internal commas. Because each
        // sentence alone clears the floor, every batch can — and must — end at the
        // sentence-final `.`, never mid-clause at one of the internal commas.
        // (When a sentence is shorter than the floor, a clean strong break isn't
        // reachable under the first-batch cap; that case falls back to a ≥ floor
        // weak break, covered by the floor test below.)
        let clause = "ə".repeat(30);
        let sentence = format!("{clause}, {clause}, {clause}."); // ~94 phonemes > floor
        let long = std::iter::repeat_n(sentence, 8)
            .collect::<Vec<_>>()
            .join(" ");
        let batches = stream_batches(&long);
        assert!(batches.len() >= 2, "expected multiple sentence batches");
        for b in &batches {
            assert!(
                ends_at_strong_boundary(b),
                "batch should end at a sentence boundary, not a comma: {b:?}"
            );
        }
    }

    #[test]
    fn stream_batches_short_reply_stays_one_batch_even_below_floor() {
        // A whole reply shorter than the floor is a complete utterance, not a
        // fragment — it stays as one batch (there's no previous batch to fold into),
        // which is exactly the pre-streaming "whole reply" behavior that sounded fine.
        let b = stream_batches("ɡɑt ɪt.");
        assert_eq!(b, vec!["ɡɑt ɪt.".to_string()]);
        assert!(b[0].chars().count() < MIN_PHONEME_LENGTH);
    }
}
