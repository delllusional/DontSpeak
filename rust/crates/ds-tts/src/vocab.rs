//! Kokoro phoneme→token vocabulary + tokenizer (port of the Kokoro vocab + tokenizer).
//!
//! `KOKORO_VOCAB` is the kokoro-onnx `config.json "vocab"` (n_token = 178),
//! copied VERBATIM from the kokoro-onnx config: id 0 is the pad token reused as BOS/EOS,
//! there is no ASCII `g` (only ɡ U+0261), no apostrophe, the combining tilde is
//! U+0303, primary stress ˈ = 156, etc.
//!
//! [`tokenize`] maps a phoneme string to token ids, silently dropping any char
//! the vocab doesn't contain (like kokoro-onnx `tokenizer.py`): anything
//! `voice-g2p` emits that Kokoro can't say is dropped, never a panic. Batching a
//! long phoneme string at sentence marks under [`MAX_PHONEME_LENGTH`] (port of
//! `splitPhonemes`) lives in [`crate::batch`].

/// Max phonemes per synthesis batch (kokoro-onnx model context).
pub const MAX_PHONEME_LENGTH: usize = 510;
/// Kokoro output sample rate (24 kHz mono).
pub const SAMPLE_RATE: u32 = 24_000;

/// The phoneme→token id table, ported char-for-char from the Kokoro vocab.
/// Lookup is a linear scan over this 114-entry `const` slice, once per phoneme;
/// no map allocation, no `phf` dep.
pub const KOKORO_VOCAB: &[(char, i64)] = &[
    (';', 1),
    (':', 2),
    (',', 3),
    ('.', 4),
    ('!', 5),
    ('?', 6),
    ('—', 9),
    ('…', 10),
    ('"', 11),
    ('(', 12),
    (')', 13),
    ('“', 14),
    ('”', 15),
    (' ', 16),
    ('\u{0303}', 17), // combining tilde
    ('ʣ', 18),
    ('ʥ', 19),
    ('ʦ', 20),
    ('ʨ', 21),
    ('ᵝ', 22),
    ('ꭧ', 23),
    ('A', 24),
    ('I', 25),
    ('O', 31),
    ('Q', 33),
    ('S', 35),
    ('T', 36),
    ('W', 39),
    ('Y', 41),
    ('ᵊ', 42),
    ('a', 43),
    ('b', 44),
    ('c', 45),
    ('d', 46),
    ('e', 47),
    ('f', 48),
    ('h', 50),
    ('i', 51),
    ('j', 52),
    ('k', 53),
    ('l', 54),
    ('m', 55),
    ('n', 56),
    ('o', 57),
    ('p', 58),
    ('q', 59),
    ('r', 60),
    ('s', 61),
    ('t', 62),
    ('u', 63),
    ('v', 64),
    ('w', 65),
    ('x', 66),
    ('y', 67),
    ('z', 68),
    ('ɑ', 69),
    ('ɐ', 70),
    ('ɒ', 71),
    ('æ', 72),
    ('β', 75),
    ('ɔ', 76),
    ('ɕ', 77),
    ('ç', 78),
    ('ɖ', 80),
    ('ð', 81),
    ('ʤ', 82),
    ('ə', 83),
    ('ɚ', 85),
    ('ɛ', 86),
    ('ɜ', 87),
    ('ɟ', 90),
    ('ɡ', 92), // U+0261 LATIN SMALL LETTER SCRIPT G (not ASCII 'g')
    ('ɥ', 99),
    ('ɨ', 101),
    ('ɪ', 102),
    ('ʝ', 103),
    ('ɯ', 110),
    ('ɰ', 111),
    ('ŋ', 112),
    ('ɳ', 113),
    ('ɲ', 114),
    ('ɴ', 115),
    ('ø', 116),
    ('ɸ', 118),
    ('θ', 119),
    ('œ', 120),
    ('ɹ', 123),
    ('ɾ', 125),
    ('ɻ', 126),
    ('ʁ', 128),
    ('ɽ', 129),
    ('ʂ', 130),
    ('ʃ', 131),
    ('ʈ', 132),
    ('ʧ', 133),
    ('ʊ', 135),
    ('ʋ', 136),
    ('ʌ', 138),
    ('ɣ', 139),
    ('ɤ', 140),
    ('χ', 142),
    ('ʎ', 143),
    ('ʒ', 147),
    ('ʔ', 148),
    ('ˈ', 156),
    ('ˌ', 157),
    ('ː', 158),
    ('ʰ', 162),
    ('ʲ', 164),
    ('↓', 169),
    ('→', 171),
    ('↗', 172),
    ('↘', 173),
    ('ᵻ', 177),
];

/// The token id for `c`, or `None` if Kokoro can't say it (drop it).
#[inline]
pub fn vocab_id(c: char) -> Option<i64> {
    KOKORO_VOCAB
        .iter()
        .find_map(|&(k, v)| if k == c { Some(v) } else { None })
}

/// Phoneme string → token ids; UNKNOWN chars are skipped silently (port of
/// `Tokenizer.kt::tokenize` / kokoro-onnx `tokenizer.py`). Caller is responsible
/// for keeping `phonemes` under [`MAX_PHONEME_LENGTH`] (via
/// [`crate::batch::split_phonemes`]); to stay panic-free regardless, we truncate
/// by char to the cap.
pub fn tokenize(phonemes: &str) -> Vec<i64> {
    phonemes
        .chars()
        .take(MAX_PHONEME_LENGTH)
        .filter_map(vocab_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_has_exactly_114_mapped_entries_and_key_entries() {
        // kokoro-onnx's config.json declares n_token = 178 (the ID SPACE size,
        // max id 177 + 1), but only 114 chars are actually MAPPED — exactly the
        // 114 `put(...)` entries in the Kokoro `KOKORO_VOCAB`, which we port
        // verbatim. (Unmapped ids are reserved/unused phoneme slots.)
        assert_eq!(KOKORO_VOCAB.len(), 114);
        // Spot-check the load-bearing, easy-to-mistranscribe entries.
        assert_eq!(vocab_id(' '), Some(16));
        assert_eq!(vocab_id('\u{0303}'), Some(17)); // combining tilde
        assert_eq!(vocab_id('ˈ'), Some(156)); // primary stress
        assert_eq!(vocab_id('ˌ'), Some(157)); // secondary stress
        assert_eq!(vocab_id('ɡ'), Some(92)); // U+0261 script g
        assert_eq!(vocab_id('.'), Some(4));
        // ASCII 'g' is NOT in the vocab (only the U+0261 variant).
        assert_eq!(vocab_id('g'), None);
        // No apostrophe.
        assert_eq!(vocab_id('\''), None);
        // All ids are distinct (no two phonemes share a token id).
        let mut ids: Vec<i64> = KOKORO_VOCAB.iter().map(|&(_, v)| v).collect();
        ids.sort_unstable();
        let distinct = {
            let mut d = ids.clone();
            d.dedup();
            d.len()
        };
        assert_eq!(distinct, KOKORO_VOCAB.len(), "ids must be unique");
    }

    #[test]
    fn tokenize_maps_known_and_drops_unknown() {
        // "həlˈO wˈɜɹld" — voice-g2p's "Hello world"; every char is in the vocab.
        let ids = tokenize("həlˈO wˈɜɹld");
        assert!(!ids.is_empty(), "expected a non-empty id list");
        // Manual expected from KOKORO_VOCAB: h ə l ˈ O ' ' w ˈ ɜ ɹ l d
        assert_eq!(ids, vec![50, 83, 54, 156, 31, 16, 65, 156, 87, 123, 54, 46]);
    }

    #[test]
    fn tokenize_drops_unmapped_chars_without_panicking() {
        // 'g' (ASCII) and '@' are not in the vocab; they are silently dropped,
        // leaving only the mappable chars.
        let ids = tokenize("a@g.b");
        assert_eq!(
            ids,
            vec![
                vocab_id('a').unwrap(),
                vocab_id('.').unwrap(),
                vocab_id('b').unwrap()
            ]
        );
    }

    #[test]
    fn tokenize_empty_is_empty() {
        assert!(tokenize("").is_empty());
    }
}
