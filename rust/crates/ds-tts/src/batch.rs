//! Phoneme batching / streaming for gapless Kokoro synthesis.
//!
//! [`split_phonemes`](crate::batch::split_phonemes) packs a long phoneme string at sentence
//! marks under `MAX_PHONEME_LENGTH` (port of `splitPhonemes`);
//! [`stream_batches`](crate::batch::stream_batches) is the
//! ramped variant for low-latency streaming. Both share `pack_batches`.

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
    // Constant 510 cap (no ramp), and DON'T break early at every sentence — pack
    // greedily so multiple sentences share a batch, preserving the inter-sentence
    // pauses (each batch is trimmed only at its ends). A forced break at the cap
    // still backtracks to the last sentence boundary, and a short trailing
    // remainder is still folded back — the same squeak guard as the stream path.
    pack_batches(
        phonemes,
        MAX_PHONEME_LENGTH,
        MAX_PHONEME_LENGTH,
        MIN_PHONEME_LENGTH,
        |b| b, // never grows past the cap it starts at
        false, // no early strong-boundary flush (keep packing to the cap)
    )
}

/// Max characters of TEXT per synthesis chunk — the SHARED bound both TTS engines pack to
/// (the ONNX path then ramps phoneme batches within a chunk; the Core ML path synthesizes the
/// chunk whole). Conservative so a chunk's phonemes stay safely under EITHER engine's input
/// cap: the Core ML (FluidAudio) chain has its own fixed phoneme limit and DROPS a whole
/// utterance that overflows it (`phonemeSequenceTooLong`), so the splitter is what guarantees
/// no spoken line is ever lost. ~1.3 phonemes/char worst case → ~340 phonemes, well under the
/// model context.
pub const TEXT_CHUNK_CHARS: usize = 260;
/// Floor on a TEXT chunk (chars) — same role as the phoneme floor: don't emit a tiny chunk
/// (which renders high-pitched) unless it's the only content; a short tail folds back.
const TEXT_CHUNK_FLOOR: usize = 48;

/// Split arbitrary TEXT into synthesis chunks, each GUARANTEED ≤ [`TEXT_CHUNK_CHARS`]. The
/// SINGLE splitter every spoken request flows through — narration AND replies, ONNX AND Core
/// ML — so no path can overflow an engine and silently drop audio. Greedy sentence packing
/// (reuses `pack_batches`) at `.,!?;` boundaries: multiple short sentences share a chunk
/// (natural pauses preserved), a long sentence is cut at the last sentence/clause boundary, and
/// a short trailing remainder folds back. A run with NO `.,!?;` at all (one long clause)
/// would survive `pack_batches` oversized, so we additionally HARD-SPLIT any over-cap chunk at
/// word boundaries — the bound is then unconditional, which is the whole point: a spoken line
/// must never be lost to a phoneme-cap overflow.
pub fn chunk_text(text: &str) -> Vec<String> {
    pack_batches(
        text,
        TEXT_CHUNK_CHARS,
        TEXT_CHUNK_CHARS,
        TEXT_CHUNK_FLOOR,
        |b| b, // constant budget — no streaming ramp at the text level
        false,
    )
    .into_iter()
    .flat_map(|c| hard_split_words(&c, TEXT_CHUNK_CHARS))
    .collect()
}

/// Last-resort split of an over-cap chunk at WORD boundaries (spaces), so no chunk exceeds
/// `cap` even when it has no `.,!?;` to break on. A single word longer than `cap` (e.g. a long
/// URL) is split at a char boundary — degraded but never dropped. In/under cap → unchanged.
fn hard_split_words(s: &str, cap: usize) -> Vec<String> {
    if s.chars().count() <= cap {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let wlen = word.chars().count();
        // A word longer than the cap on its own: flush, then emit it in char-sized pieces.
        if wlen > cap {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            let mut piece = String::new();
            for ch in word.chars() {
                if piece.chars().count() >= cap {
                    out.push(std::mem::take(&mut piece));
                }
                piece.push(ch);
            }
            if !piece.is_empty() {
                out.push(piece);
            }
            continue;
        }
        // +1 for the joining space (only when `cur` is non-empty).
        let need = if cur.is_empty() {
            wlen
        } else {
            cur.chars().count() + 1 + wlen
        };
        if need > cap && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
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

/// First streaming batch budget (phonemes). Small so the first audio starts
/// fast (~3 s at our measured synth speed) instead of waiting for the whole
/// reply. Subsequent batches grow geometrically up to `MAX_PHONEME_LENGTH`.
pub const STREAM_FIRST_BUDGET: usize = 80;
/// Per-batch growth factor. Kokoro synth runs ~0.55× real-time here, so audio
/// drains slower than the next batch synthesizes as long as each batch is at
/// most ~1/0.55 ≈ 1.8× the previous one. We use 1.4× for margin against synth
/// jitter and the fixed per-call overhead — that keeps the rodio queue from
/// ever underrunning, so playback is gapless with NO artificial cushion.
const STREAM_GROWTH_NUM: usize = 7; // 1.4 = 7/5
const STREAM_GROWTH_DEN: usize = 5;

/// Floor on a streaming batch's length (phonemes). A batch shorter than this —
/// especially a trailing fragment like "Got it." — makes Kokoro select a
/// short-utterance style row (indexed by token count; see `synth::style_row`),
/// which compresses durations and renders the words high-pitched / "choked". The
/// reference Kokoro pipelines avoid this structurally by packing chunks to the
/// 510 cap and never emitting tiny ones; our low-latency ramp can, so we (a)
/// never FLUSH a batch below this floor and (b) fold a short trailing remainder
/// back into the previous batch. Kept ≤ [`STREAM_FIRST_BUDGET`] so the first
/// batch still flushes early for fast first-audio. Tuned by ear.
const MIN_PHONEME_LENGTH: usize = 64;
const _: () = assert!(
    MIN_PHONEME_LENGTH <= STREAM_FIRST_BUDGET,
    "the min-batch floor must not exceed the first-batch budget, or first-audio latency regresses"
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

/// Split a phoneme string into a RAMPED sequence of batches for gapless
/// streaming: a small first batch (fast first-audio) growing geometrically to
/// `MAX_PHONEME_LENGTH`. Same `.,!?;` boundaries as [`split_phonemes`] (never
/// breaks mid-clause), but the budget grows per batch so the producer (synth)
/// stays ahead of the consumer (playback) after the first batch. Pass the whole
/// reply's phonemes — do NOT pre-split into sentences (that caused the tiny-group
/// underruns this replaces).
pub fn stream_batches(phonemes: &str) -> Vec<String> {
    // Ramped cap (small first batch, growing geometrically) AND early
    // strong-boundary flushing, so each batch is a whole sentence delivered fast.
    pack_batches(
        phonemes,
        STREAM_FIRST_BUDGET,
        MAX_PHONEME_LENGTH,
        MIN_PHONEME_LENGTH,
        |b| (b * STREAM_GROWTH_NUM / STREAM_GROWTH_DEN).min(MAX_PHONEME_LENGTH),
        true,
    )
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
        // First batch is small (fast first-audio), within the first-budget + one part.
        assert!(
            batches[0].chars().count() <= STREAM_FIRST_BUDGET + 41,
            "first batch must stay small for low first-audio latency, got {}",
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

    // ── chunk_text: the SHARED text splitter that guards BOTH TTS engines ──────────────
    // The regression these pin: a long narration line was sent whole to the Core ML engine,
    // overflowed its phoneme cap (`phonemeSequenceTooLong`), and the WHOLE line was dropped.
    // The invariants below — every chunk ≤ cap, and NO words lost — are what make that
    // impossible on either engine.

    /// Rejoin chunks and compare the word sequence to the source: the splitter must never
    /// drop or reorder content, only insert boundaries.
    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn chunk_text_short_text_is_one_chunk() {
        assert_eq!(
            chunk_text("A short spoken line."),
            vec!["A short spoken line."]
        );
    }

    #[test]
    fn chunk_text_empty_is_no_chunks() {
        assert!(chunk_text("").is_empty());
        assert!(chunk_text("   \n  ").is_empty());
    }

    #[test]
    fn chunk_text_every_chunk_is_within_cap_and_loses_nothing() {
        // A long, comma/period-punctuated line (the real narration shape that overflowed) —
        // comfortably past the cap so it MUST split.
        let line = "That quote block at the top is my spoken digest, the part that gets read \
            aloud, and yes it doubles as visible text so it can look heavy and a bit redundant \
            with the detail below, which is why splitting it cleanly across chunks matters so \
            much here, because otherwise the whole spoken line overflows the model and is \
            dropped, leaving you with silence instead of the words you expected to hear today.";
        assert!(
            line.chars().count() > TEXT_CHUNK_CHARS,
            "test fixture must exceed the cap"
        );
        let chunks = chunk_text(line);
        assert!(
            chunks.len() >= 2,
            "a long line must split into multiple chunks"
        );
        for c in &chunks {
            assert!(
                c.chars().count() <= TEXT_CHUNK_CHARS,
                "chunk over cap: {} chars",
                c.chars().count()
            );
        }
        assert_eq!(
            words(&chunks.join(" ")),
            words(line),
            "no words may be lost or reordered"
        );
    }

    #[test]
    fn chunk_text_hard_splits_a_long_run_with_no_punctuation() {
        // THE failure mode the punctuation-only packer can't catch: one long clause with NO
        // `.,!?;` at all. The hard word-split guarantee must still bound every chunk.
        let run =
            "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima ".repeat(6); // ~390 chars, zero sentence marks
        let chunks = chunk_text(&run);
        assert!(
            chunks.len() >= 2,
            "an over-cap unpunctuated run must still split"
        );
        for c in &chunks {
            assert!(
                c.chars().count() <= TEXT_CHUNK_CHARS,
                "unpunctuated chunk over cap: {}",
                c.chars().count()
            );
        }
        assert_eq!(
            words(&chunks.join(" ")),
            words(&run),
            "no words lost in the hard split"
        );
    }

    #[test]
    fn chunk_text_splits_a_single_oversize_word_at_char_boundary() {
        // Degenerate: a single "word" longer than the cap (e.g. a URL). It must be emitted in
        // ≤ cap pieces rather than dropped or left oversized.
        let word = "x".repeat(TEXT_CHUNK_CHARS * 2 + 5);
        let chunks = chunk_text(&word);
        assert!(chunks.len() >= 3);
        for c in &chunks {
            assert!(c.chars().count() <= TEXT_CHUNK_CHARS);
        }
        assert_eq!(
            chunks.concat(),
            word,
            "every character survives the char-split"
        );
    }

    #[test]
    fn chunk_text_packs_several_short_sentences_together() {
        // Short sentences share a chunk (so inter-sentence pauses are preserved) rather than
        // each becoming its own tiny, high-pitched utterance.
        let chunks = chunk_text("One. Two. Three.");
        assert_eq!(chunks, vec!["One. Two. Three."]);
    }
}
