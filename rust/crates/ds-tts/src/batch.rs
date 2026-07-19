//! Model-bounded phoneme batching for Kokoro.
//!
//! [`split_phonemes`](crate::batch::split_phonemes) packs at sentence marks under `MAX_PHONEME_LENGTH` (port of
//! `splitPhonemes`). [`stream_batches`](crate::batch::stream_batches) is the ramped, floor-protected variant for the
//! shared frontend. Batch commit is the consumer's job (helper `prepare`). Both use
//! `pack_batches`.

use crate::vocab::MAX_PHONEME_LENGTH;

/// Clause marks from `Tokenizer.kt::splitPhonemes` / kokoro-onnx `_split_phonemes`.
const SPLIT_CHARS: &[char] = &['.', ',', '!', '?', ';'];

/// Split at `.,!?;` into interleaved `[text, mark, …]` (`re.split(r"([.,!?;])", s)`).
/// Lone marks glue to the preceding chunk — never mid-clause.
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
    // Constant cap, greedy across sentences (preserve inter-sentence pause).
    // Unpunctuated runs can stay oversized after pack — hard_split_words so
    // engines never see over-cap (ONNX truncates silently; Core ML drops whole).
    pack_batches(
        phonemes,
        MAX_PHONEME_LENGTH,
        MAX_PHONEME_LENGTH,
        MIN_PHONEME_LENGTH,
        |b| b,
        false, // pack to cap; no early strong-boundary flush
    )
    .into_iter()
    .flat_map(|b| hard_split_words(&b, MAX_PHONEME_LENGTH, MIN_PHONEME_LENGTH))
    .collect()
}

/// Last-resort over-cap split at word (space) boundaries. No `.,!?;` needed.
/// Single word > cap → char split (degraded, never dropped). Respects `floor`.
fn hard_split_words(s: &str, cap: usize, floor: usize) -> Vec<String> {
    if s.chars().count() <= cap {
        return vec![s.to_string()];
    }
    debug_assert!(floor > 0 && floor <= cap.div_ceil(2));

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
            // Last split: balance at a word boundary when both sides clear floor/cap.
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

/// Shared packer for [`split_phonemes`] / [`stream_batches`]. Budget starts at
/// `budget0`, advances via `grow` after each flush; hard cap is model context.
/// Never flushes below `floor`; forced (cap) breaks prefer last `.!?` when the
/// head clears floor; short tail folds into previous batch. `break_at_strong`
/// ends early at sentence marks once past floor (streaming); else packs to cap.
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
    // Byte offset past last `.!?` in `current` — preferred forced-break cut.
    let mut strong_at: Option<usize> = None;

    for raw_part in parts {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        // Cap is coarse; char count (vs Kotlin UTF-16) never under-batches dangerously.
        let part_len = part.chars().count();
        let cur_len = current.chars().count();
        let cap = budget.min(hard_cap);
        let is_lone_mark = part_len == 1 && SPLIT_CHARS.contains(&part.chars().next().unwrap());

        // Forced break at cap, floor-gated. Lone mark stays with preceding clause.
        if !is_lone_mark && !current.is_empty() && cur_len >= floor && cur_len + part_len + 1 >= cap
        {
            // Prefer last sentence boundary (Kokoro `waterfall_last`) when head ≥ floor.
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
        }

        if !is_lone_mark && !current.is_empty() {
            current.push(' ');
        }
        current.push_str(part);
        if is_lone_mark && STRONG_MARKS.contains(&part.chars().next().unwrap()) {
            strong_at = Some(current.len());
        }

        // Streaming preferred break: whole sentence once past floor.
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

    // Fold short trailing remainder — only the tail can be sub-floor after flush rules.
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

/// First ramped batch budget (phonemes). Grows geometrically to `MAX_PHONEME_LENGTH`.
pub const STREAM_FIRST_BUDGET: usize = 80;
/// Growth factor 1.4 = 7/5.
const STREAM_GROWTH_NUM: usize = 7;
const STREAM_GROWTH_DEN: usize = 5;

/// Floor (phonemes). Sub-floor batches pick a short-utterance style row
/// (`synth::style_row` by token count) → compressed, high-pitched prosody.
/// Never flush below floor; fold short tail into previous. ≤ [`STREAM_FIRST_BUDGET`].
const MIN_PHONEME_LENGTH: usize = 64;
const _: () = assert!(
    MIN_PHONEME_LENGTH <= STREAM_FIRST_BUDGET,
    "the min-batch floor must not exceed the first-batch budget"
);

/// Preferred boundaries (Kokoro `waterfall_last`); `,;` are weak fallbacks only.
const STRONG_MARKS: &[char] = &['.', '!', '?'];

fn ends_at_strong_boundary(s: &str) -> bool {
    s.trim_end()
        .chars()
        .next_back()
        .is_some_and(|c| STRONG_MARKS.contains(&c))
}

/// Ramped stream packs: small first batch → geometric growth to cap, same `.,!?;`
/// rules as [`split_phonemes`]. Pass the whole reply so short-tail fold sees every boundary.
pub fn stream_batches(phonemes: &str) -> Vec<String> {
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
        let batches = split_phonemes("a. b");
        assert_eq!(batches, vec!["a. b".to_string()]);
        let q = split_phonemes("hi? there");
        assert_eq!(q, vec!["hi? there".to_string()]);
    }

    #[test]
    fn split_phonemes_empty_yields_no_batches() {
        assert!(split_phonemes("").is_empty());
        assert!(split_phonemes("   ").is_empty());
        let only_marks = split_phonemes("...");
        assert_eq!(only_marks, vec!["...".to_string()]);
    }

    #[test]
    fn split_phonemes_hard_splits_a_long_run_with_no_punctuation() {
        // Unpunctuated run = one atomic part; hard-split must still bound (ONNX truncates,
        // Core ML drops). Expanded digit strings hit this path.
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
        // Streaming path: same hard-split guarantee as split_phonemes.
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
        // Voice pack rows 0..=509 by token count; 510-token input has no style row.
        // Split must avoid a one-token tail (degenerate short-utterance prosody).
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
        // Final word can't fold into prior cap chunk — rebalance both sides.
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
        // Packs across sentences (vs stream's per-sentence breaks); folds short tail.
        let sentence = "ə".repeat(40);
        let body = std::iter::repeat_n(sentence, 6)
            .collect::<Vec<_>>()
            .join(". ");
        let one = stream_batches(&body);
        let packed = split_phonemes(&body);
        assert!(
            packed.len() < one.len(),
            "split_phonemes should pack more per batch than the streaming ramp: \
             packed={} stream={}",
            packed.len(),
            one.len()
        );
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
        // Weak (comma) boundaries only — exercises budget ramp, not strong-break preference.
        let clause = "ə".repeat(40);
        let long = std::iter::repeat_n(clause, 40)
            .collect::<Vec<_>>()
            .join(", ");
        let batches = stream_batches(&long);
        assert!(batches.len() >= 3, "expected a ramped multi-batch split");
        assert!(
            batches[0].chars().count() <= STREAM_FIRST_BUDGET + 41,
            "first batch must stay within the ramp budget, got {}",
            batches[0].chars().count()
        );
        // Final batch is remainder (may be shorter); exclude from ramp check.
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
        // Long body + tiny closer: tail folds into previous so nothing squeaks.
        let body = std::iter::repeat_n("ə".repeat(50), 12)
            .collect::<Vec<_>>()
            .join(". ");
        let long = format!("{body}. ɡɑt ɪt.");
        let batches = stream_batches(&long);
        assert!(
            batches.len() >= 2,
            "the long body must split into many batches"
        );
        for b in &batches {
            assert!(
                b.chars().count() >= MIN_PHONEME_LENGTH,
                "no batch may be below the floor; got {} ({b:?})",
                b.chars().count()
            );
        }
        assert!(
            batches.last().unwrap().ends_with("ɡɑt ɪt."),
            "the short tail must survive, merged into the final batch"
        );
    }

    #[test]
    fn stream_batches_prefers_strong_boundaries_over_commas() {
        // Sentences > floor with internal commas must end at `.`, not mid-clause.
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
        // Whole reply below floor: one batch (no prior batch to fold into).
        let b = stream_batches("ɡɑt ɪt.");
        assert_eq!(b, vec!["ɡɑt ɪt.".to_string()]);
        assert!(b[0].chars().count() < MIN_PHONEME_LENGTH);
    }
}
