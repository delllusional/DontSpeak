//! Chatterbox text frontend: markdown → spoken prose → sentence-bounded text chunks.
//!
//! Same GFM + English number-expansion as Kokoro, then plain-text chunks (no G2P/IPA).
//! Deterministic so batch indices stay stable across resume (`skip`).
//! Chunk schedule mirrors Kokoro (floor / overshoot / short-tail fold).

/// Hard chunk budget in chars. Ramped low-latency schedule grows from
/// `batch::STREAM_FIRST_BUDGET` to this model-safe cap.
pub const MAX_CHUNK_CHARS: usize = 300;

/// Dense non-ASCII prose: stay under the AR model's 1,024-token (~41 s) budget.
const DENSE_SCRIPT_MAX_CHARS: usize = 150;

/// Markdown → prose → chunks. Empty / unspeakable → zero chunks.
/// Number expansion is English-only; other languages keep digits.
pub fn text_chunks(text: &str, language: &str) -> Vec<String> {
    let prose = crate::spoken::SpokenText::from_markdown(text).into_string();
    let prose = if language == "en" {
        crate::numbers::expand_numbers(&prose)
    } else {
        prose
    };
    chunk_prose(&prose)
}

pub fn chunk_prose(prose: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_limit = MAX_CHUNK_CHARS;
    let mut budget = crate::batch::STREAM_FIRST_BUDGET;
    for sentence in split_sentences(prose) {
        let mut pending = sentence.trim().to_string();
        if pending.is_empty() {
            continue;
        }

        loop {
            let pending_limit = chunk_limit(&pending);
            let cap = budget.min(current_limit).min(pending_limit);
            let current_len = current.chars().count();
            let pending_len = pending.chars().count();
            let need = current_len + usize::from(!current.is_empty()) + pending_len;

            if !current.is_empty() && need > cap {
                if current_len >= crate::batch::STREAM_MIN_BUDGET {
                    push_speakable(&mut chunks, std::mem::take(&mut current));
                    current_limit = MAX_CHUNK_CHARS;
                    budget = crate::batch::grow_stream_budget(budget, MAX_CHUNK_CHARS);
                    continue;
                }

                // Join under-floor head to next sentence before splitting.
                pending = format!("{current} {pending}");
                current.clear();
                current_limit = current_limit.min(pending_limit);
                continue;
            }

            if current.is_empty() && pending_len > cap {
                // Modest overshoot beats a sub-floor tail; else split at ramp budget.
                if pending_len > cap + crate::batch::STREAM_MIN_BUDGET
                    || pending_len > pending_limit
                {
                    let (head, tail) = split_prefix(&pending, cap);
                    push_speakable(&mut chunks, head);
                    budget = crate::batch::grow_stream_budget(budget, MAX_CHUNK_CHARS);
                    pending = tail;
                    current_limit = MAX_CHUNK_CHARS;
                    continue;
                }
            }

            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(&pending);
            current_limit = current_limit.min(pending_limit);
            break;
        }

        if current.chars().count() >= crate::batch::STREAM_MIN_BUDGET {
            push_speakable(&mut chunks, std::mem::take(&mut current));
            current_limit = MAX_CHUNK_CHARS;
            budget = crate::batch::grow_stream_budget(budget, MAX_CHUNK_CHARS);
        }
    }
    push_speakable(&mut chunks, current);

    // Fold short tail only when joined batch stays within both scripts' caps.
    if chunks.len() >= 2 {
        let tail_len = chunks.last().map_or(0, |chunk| chunk.chars().count());
        let previous = &chunks[chunks.len() - 2];
        let joined_len = previous.chars().count() + 1 + tail_len;
        let joined_cap = chunk_limit(previous).min(chunk_limit(chunks.last().unwrap()));
        if tail_len < crate::batch::STREAM_MIN_BUDGET && joined_len <= joined_cap {
            let tail = chunks.pop().unwrap();
            let previous = chunks.last_mut().unwrap();
            previous.push(' ');
            previous.push_str(&tail);
        }
    }
    chunks
}

fn chunk_limit(text: &str) -> usize {
    let whitespace_free = !text.chars().any(char::is_whitespace);
    let has_non_ascii_letter_or_number = text
        .chars()
        .any(|character| !character.is_ascii() && character.is_alphanumeric());
    if whitespace_free && has_non_ascii_letter_or_number {
        DENSE_SCRIPT_MAX_CHARS
    } else {
        MAX_CHUNK_CHARS
    }
}

/// Keep only chunks with something pronounceable (≥1 alphanumeric char).
fn push_speakable(chunks: &mut Vec<String>, chunk: String) {
    if chunk.chars().any(char::is_alphanumeric) {
        chunks.push(chunk);
    }
}

/// Split at sentence enders and newlines. ASCII enders (`.` `!` `?` `…`) require
/// following whitespace/end so dotted identifiers stay whole; fullwidth/CJK `。！？．`,
/// Arabic `؟`, and Hindi `।` end sentences unconditionally (those scripts don't put a
/// space after punctuation).
fn split_sentences(prose: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = prose.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
        let boundary = match c {
            '。' | '！' | '？' | '．' | '؟' | '।' => true,
            '.' | '!' | '?' | '…' => chars.peek().is_none_or(|n| n.is_whitespace()),
            _ => false,
        };
        if boundary {
            out.push(std::mem::take(&mut cur));
        }
    }
    out.push(cur);
    out
}

/// Split one overlong pending run at the current ramp budget, preferring a word boundary.
fn split_prefix(text: &str, cap: usize) -> (String, String) {
    debug_assert!(text.chars().count() > cap);
    let cut = text
        .char_indices()
        .nth(cap)
        .map_or(text.len(), |(index, _)| index);
    let window = &text[..cut];
    let split_at = window
        .rfind(char::is_whitespace)
        .filter(|&index| index > 0)
        .unwrap_or(cut);
    (
        text[..split_at].trim().to_string(),
        text[split_at..].trim_start().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_unspeakable_inputs_produce_zero_chunks() {
        assert!(text_chunks("", "en").is_empty());
        assert!(text_chunks("   \n\t", "en").is_empty());
        assert!(text_chunks("🎉🎉🎉", "en").is_empty());
        assert!(text_chunks("!!! ... ???", "en").is_empty());
    }

    #[test]
    fn short_prose_is_one_chunk_and_markdown_is_stripped() {
        let chunks = text_chunks("Build **57** shipped.", "en");
        assert_eq!(chunks, vec!["Build fifty-seven shipped.".to_string()]);
    }

    #[test]
    fn number_expansion_is_english_only() {
        // Non-English (and OmniVoice's "auto") keep digits — the English word form
        // would be spoken verbatim in the wrong language.
        assert_eq!(text_chunks("**57**", "ru"), vec!["57".to_string()]);
        assert_eq!(text_chunks("57", "auto"), vec!["57".to_string()]);
        assert_eq!(text_chunks("**57**", "en"), vec!["fifty-seven".to_string()]);
    }

    #[test]
    fn sentences_pack_up_to_the_budget_then_split() {
        let sentence = "This sentence is about forty characters.";
        let n = 12;
        let text = vec![sentence; n].join(" ");
        let chunks = chunk_prose(&text);
        assert!(chunks.len() > 1, "must split: {chunks:?}");
        for c in &chunks {
            assert!(c.chars().count() <= MAX_CHUNK_CHARS, "{}", c.len());
            assert!(c.ends_with('.'), "chunks end on sentence bounds: {c:?}");
        }
        // Nothing dropped: rejoining covers every sentence.
        let total: usize = chunks.iter().map(|c| c.matches(sentence).count()).sum();
        assert_eq!(total, n);
    }

    #[test]
    fn pathological_unbroken_run_hard_splits_on_the_char_budget() {
        let run = "x".repeat(MAX_CHUNK_CHARS * 2 + 9);
        let chunks = chunk_prose(&run);
        assert!(chunks.len() >= 3);
        assert_eq!(chunks[0].chars().count(), crate::batch::STREAM_FIRST_BUDGET);
        assert!(chunks.iter().all(|c| c.chars().count() <= MAX_CHUNK_CHARS));
        assert_eq!(
            chunks.iter().map(|c| c.chars().count()).sum::<usize>(),
            MAX_CHUNK_CHARS * 2 + 9
        );
    }

    #[test]
    fn enderless_cjk_splits_on_the_dense_script_budget() {
        let text = "你".repeat(MAX_CHUNK_CHARS);
        let chunks = chunk_prose(&text);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].chars().count(), crate::batch::STREAM_FIRST_BUDGET);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DENSE_SCRIPT_MAX_CHARS)
        );
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn overlong_sentence_prefers_a_whitespace_split() {
        let words = vec!["word"; 100].join(" "); // 499 chars
        let chunks = chunk_prose(&words);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.chars().count() <= MAX_CHUNK_CHARS);
            assert!(!c.starts_with(' ') && !c.ends_with(' '));
            assert!(c.split(' ').all(|w| w == "word"), "no mid-word cut: {c:?}");
        }
    }

    #[test]
    fn max_sized_input_uses_the_shared_ramped_first_batch() {
        let exact = "y".repeat(MAX_CHUNK_CHARS);
        let exact_chunks = chunk_prose(&exact);
        assert_eq!(
            exact_chunks[0].chars().count(),
            crate::batch::STREAM_FIRST_BUDGET
        );
        assert_eq!(exact_chunks.concat(), exact);

        let over = format!("{exact}z");
        let chunks = chunk_prose(&over);
        assert_eq!(chunks.concat(), over);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= MAX_CHUNK_CHARS)
        );
    }

    #[test]
    fn chunking_is_deterministic() {
        let text = "One. Two! Three? Four… Five.";
        assert_eq!(chunk_prose(text), chunk_prose(text));
        assert_eq!(chunk_prose(text), vec![text.to_string()]);
    }

    fn sentences(prose: &str) -> Vec<String> {
        split_sentences(prose)
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    #[test]
    fn nonlatin_enders_split_without_following_whitespace() {
        // CJK prose has no space after 。！？． — each ender still ends a sentence.
        assert_eq!(
            sentences("第一句。第二句！第三句？第四句．"),
            vec!["第一句。", "第二句！", "第三句？", "第四句．"]
        );
        // Arabic ؟ and Hindi । likewise.
        assert_eq!(sentences("هل تعمل؟نعم."), vec!["هل تعمل؟", "نعم."]);
        assert_eq!(
            sentences("यह पहला वाक्य है।यह दूसरा है।"),
            vec!["यह पहला वाक्य है।", "यह दूसरा है।"]
        );
        // ASCII enders keep the whitespace requirement: dotted identifiers stay whole.
        assert_eq!(
            sentences("Use serve.rs today. Ship it."),
            vec!["Use serve.rs today.", "Ship it."]
        );
    }
}
