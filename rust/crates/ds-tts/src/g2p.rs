//! English G2P wrapper around `voice-g2p` (misaki dict, pure Rust) that NEVER
//! invokes espeak-ng and NEVER aborts the utterance on an out-of-vocab word.
//!
//! WHY a wrapper. `voice-g2p`'s only public path, `english_to_phonemes`, runs
//! the misaki dictionary and, for words it can't resolve, calls an EXTERNAL
//! `espeak-ng` binary (`G2PConfig.espeak_path`, default `"espeak-ng"`). The
//! owner wants espeak OUT. In voice-g2p 0.2.2 that fallback is GRACEFUL by
//! construction — `EspeakFallback::convert_word` returns `Option` and yields
//! `None` when the binary is absent, so the word degrades to empty phonemes and
//! `english_to_phonemes` still returns `Ok` (it never constructs the
//! `EspeakNotFound`/`EspeakFailed` error variants on the convert path). We still
//! build a resilient wrapper for defense in depth and forward-compat:
//!   1. FAST PATH: try `english_to_phonemes(text)` whole; on `Ok` use it.
//!   2. RESILIENT PATH: on any `Err`, split into words and phonemize each on its
//!      own, dropping (skipping) any word that errors, and pass punctuation
//!      through so the downstream `split_phonemes` batching still sees `.,!?;`.
//!
//! Bridging to the Kokoro vocab is then done by `vocab::tokenize`,
//! which drops any char Kokoro can't say. voice-g2p (misaki) and Kokoro share
//! the same phoneme inventory, so the bridge is near-lossless — the
//! `g2p_output_is_fully_covered_by_kokoro_vocab_for_common_and_edge_words` test
//! pins this by asserting EVERY char voice-g2p emits for a spread of common +
//! tricky words is in `KOKORO_VOCAB` (a regression guard, not a vibe).
//!
//! NOTE (owner-accepted parity caveat): misaki ≠ espeak, so these phonemes — and
//! thus the tokens — are NOT byte-identical to an espeak path. Functional
//! English; OOV/technical words may pronounce worse. The dict is 90k gold + 93k
//! silver entries, so common prose is well covered.

/// Phonemize already-normalized English `text` to a Kokoro-compatible phoneme
/// string. Never shells espeak; never returns an error. An OOV word the misaki
/// dict can't resolve is recovered by a tiered ESPEAK-FREE fallback (name lexicon
/// → neural predictor → letter-by-letter spelling; see [`phonemize_word`]) so it
/// is pronounced/approximated audibly, NEVER silent — the cross-platform parity
/// fix for the macOS neural-G2P path. (Without this, an OOV proper noun like
/// "Nicole" degrades to empty phonemes and vanishes whenever espeak-ng is absent.)
pub fn phonemize(text: &str) -> String {
    // Expand digits to English words first — `voice-g2p` (misaki) has no number
    // frontend, so a bare "12" would drop at the vocab layer (silent). English
    // only: non-English voices reach espeak (which expands in-language) and never
    // call this. See [`crate::numbers`].
    let text = crate::numbers::expand_numbers(text);
    // FAST PATH: the whole-string conversion is POS-aware (homographs, cross-word
    // reductions), so keep it UNCHANGED whenever every word resolves. Only when a
    // word is OOV (would go silent) do we rebuild per-word with the fallback — so
    // common prose pays nothing and never loses context.
    match voice_g2p::english_to_phonemes(&text) {
        Ok(whole) if !text.split_whitespace().any(word_is_oov) => whole,
        _ => phonemize_per_word(&text),
    }
}

/// A whitespace token is OOV iff it has a pronounceable (alphabetic) core that
/// voice-g2p leaves empty — i.e. the dict missed it and (no espeak) it would be
/// silent. Punctuation-only tokens and already-expanded numbers are never "OOV".
fn word_is_oov(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_alphanumeric());
    if !core.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    voice_g2p::english_to_phonemes(core)
        .map(|p| p.trim().is_empty())
        .unwrap_or(true)
}

/// Per-word phonemization with an ESPEAK-FREE OOV fallback: dict first, then the
/// name lexicon, then letter-by-letter spelling — so an OOV word is approximated
/// audibly, never silent. Trailing sentence punctuation is preserved between words
/// so the downstream `split_phonemes` batching still sees `.,!?;`.
fn phonemize_per_word(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        let (core, trailing) = split_trailing_punct(word);
        if !core.is_empty() {
            let ph = phonemize_word(core);
            if !ph.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&ph);
            }
        }
        if !trailing.is_empty() {
            out.push_str(trailing);
        }
    }
    out
}

/// One word → phonemes, NEVER silent for a real word: misaki dict, else the name
/// lexicon, else letter-by-letter spelling. `core` has trailing sentence punctuation
/// already peeled by the caller; we strip any remaining edge punctuation for the
/// dict/lexicon lookup but spell over the alphabetic letters only.
fn phonemize_word(core: &str) -> String {
    if let Ok(ph) = voice_g2p::english_to_phonemes(core) {
        let ph = ph.trim();
        if !ph.is_empty() {
            return ph.to_string();
        }
    }
    // OOV (dict miss + no espeak). Tiered, espeak-free, all in-vocab:
    //   1. name lexicon — authoritative pronunciation for known proper nouns (the model
    //      mis-stresses some names, e.g. "Nicole" → NIH-kul, so the lexicon wins);
    //   2. neural predictor — real pronunciation for arbitrary OOV (parity with macOS);
    //   3. letter-by-letter spelling — the never-silent floor when neither applies.
    let bare = core.trim_matches(|c: char| !c.is_alphanumeric());
    if let Some(lex) = name_lexicon(bare) {
        return lex.to_string();
    }
    if let Some(neural) = neural_phonemes(bare) {
        return neural;
    }
    spell_out(bare)
}

/// Hand-authored Kokoro phonemes for common OOV proper nouns — the Kokoro voice names the
/// misaki dict misses (`Nicole`, `Aoede`, `Eric`, `Fenrir`, `Santa`). Lets them pronounce
/// CORRECTLY without espeak instead of being spelled out. Every string is built from phoneme
/// chars that voice-g2p itself emits for in-dict words, so they bridge to Kokoro losslessly;
/// `oov_lexicon_is_vocab_safe` pins that. Match on the lowercased bare word.
fn name_lexicon(word: &str) -> Option<&'static str> {
    Some(match word.to_ascii_lowercase().as_str() {
        "nicole" => "nɪkˈOl",
        "aoede" => "Aˈidi",
        "eric" => "ˈɛɹɪk",
        "fenrir" => "fˈɛnɹɪɹ",
        "santa" => "sˈæntə",
        _ => return None,
    })
}

/// Spell an OOV word letter-by-letter using each letter's NAME phonemes — the never-silent
/// floor for an unknown word with no lexicon entry. Non-letters are skipped (digits are
/// expanded upstream). Letter phonemes are the validated voice-g2p outputs for the spoken
/// letter names ("en" → ˈɛn, "eye" → ˈI, …), so they're guaranteed in-vocab.
fn spell_out(word: &str) -> String {
    word.chars()
        .filter_map(letter_phonemes)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One ASCII letter → the Kokoro phonemes for its spoken NAME, or `None` for a non-letter.
/// Sourced from voice-g2p's own output for the letter-name words (validated in-vocab); `a`/`e`
/// use the bare vowel since "ay"/"ee" aren't dict words.
fn letter_phonemes(c: char) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase() {
        'a' => "ˈA",
        'b' => "bˈi",
        'c' => "sˈi",
        'd' => "dˈi",
        'e' => "ˈi",
        'f' => "ˈɛf",
        'g' => "ʤˈi",
        'h' => "ˈAʧ",
        'i' => "ˈI",
        'j' => "ʤˈA",
        'k' => "kˈA",
        'l' => "ˌɛl",
        'm' => "ˈɛm",
        'n' => "ˈɛn",
        'o' => "ˈO",
        'p' => "pˈi",
        'q' => "kjˈu",
        'r' => "ɑɹ",
        's' => "ˈɛs",
        't' => "tˈi",
        'u' => "ju",
        'v' => "vˈi",
        'w' => "dˈʌbᵊl ju",
        'x' => "ˈɛks",
        'y' => "wˌI",
        'z' => "zˈi",
        _ => return None,
    })
}

/// The bundled pure-Rust NEURAL OOV predictor (a g2p.py port), loaded ONCE. `Model` is immutable
/// after load (only `ndarray` weights + lookup maps, no interior mutability ⇒ `Sync`), so a
/// `&'static` is shared across threads; predictions return `&'static str`. `None` if the model
/// can't load (then callers spell the word out). Lazy: the first OOV word pays the ~one-time load.
fn neural_oov_model() -> Option<&'static grapheme_to_phoneme::Model> {
    static MODEL: std::sync::OnceLock<Option<grapheme_to_phoneme::Model>> =
        std::sync::OnceLock::new();
    MODEL
        .get_or_init(|| grapheme_to_phoneme::Model::load_in_memory().ok())
        .as_ref()
}

/// Real (approximate) pronunciation for an OOV word via the neural predictor, bridged to Kokoro
/// IPA. This is the parity-with-macOS fallback (macOS uses FluidAudio's neural G2P): a dict miss
/// like "Yanchenko" is PRONOUNCED, not spelled. `None` (→ caller spells it) when the model is
/// unavailable, the word isn't bare ASCII letters (the predictor has no number/punct frontend),
/// or it predicts nothing.
fn neural_phonemes(word: &str) -> Option<String> {
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let arpa = neural_oov_model()?.predict_phonemes_strs(word).ok()?;
    let ipa = arpabet_to_kokoro(&arpa);
    (!ipa.trim().is_empty()).then_some(ipa)
}

/// Bridge ARPABET (from [`neural_phonemes`]) into the Kokoro/misaki IPA inventory. A vowel's
/// stress digit (0/1/2) becomes a stress mark placed IMMEDIATELY BEFORE the vowel — exactly
/// misaki's convention (e.g. ARPABET `N IH0 K OW1 L` → `nɪkˈOl`), so neural OOV phonemes match
/// the dict's style and bridge to Kokoro losslessly. Every mapping target is a phoneme the dict
/// itself emits (validated against `KOKORO_VOCAB` by `arpabet_bridge_is_vocab_safe`).
/// Diphthongs use Kokoro's single-char shorthands (eɪ→A, aɪ→I, oʊ→O, ɔɪ→Y, aʊ→W).
fn arpabet_to_kokoro(tokens: &[&str]) -> String {
    let mut out = String::new();
    for tok in tokens {
        // Peel a trailing stress digit (vowels carry 0/1/2; consonants carry none).
        let (base, stress) = match tok.as_bytes().last() {
            Some(b'0') => (&tok[..tok.len() - 1], 0u8),
            Some(b'1') => (&tok[..tok.len() - 1], 1u8),
            Some(b'2') => (&tok[..tok.len() - 1], 2u8),
            _ => (*tok, 9u8), // consonant — no stress
        };
        let ipa: &str = match base {
            "AA" => "ɑ",
            "AE" => "æ",
            // Reduced (AH0 → schwa) vs full (AH1/AH2 → ʌ).
            "AH" => {
                if stress == 0 {
                    "ə"
                } else {
                    "ʌ"
                }
            }
            "AO" => "ɔ",
            "AW" => "W",
            "AY" => "I",
            "EH" => "ɛ",
            // R-coloured: stressed NURSE vowel (ɜɹ) vs reduced lettER (əɹ).
            "ER" => {
                if stress == 1 || stress == 2 {
                    "ɜɹ"
                } else {
                    "əɹ"
                }
            }
            "EY" => "A",
            "IH" => "ɪ",
            "IY" => "i",
            "OW" => "O",
            "OY" => "Y",
            "UH" => "ʊ",
            "UW" => "u",
            "B" => "b",
            "CH" => "ʧ",
            "D" => "d",
            "DH" => "ð",
            "F" => "f",
            "G" => "ɡ",
            "HH" => "h",
            "JH" => "ʤ",
            "K" => "k",
            "L" => "l",
            "M" => "m",
            "N" => "n",
            "NG" => "ŋ",
            "P" => "p",
            "R" => "ɹ",
            "S" => "s",
            "SH" => "ʃ",
            "T" => "t",
            "TH" => "θ",
            "V" => "v",
            "W" => "w",
            "Y" => "j",
            "Z" => "z",
            "ZH" => "ʒ",
            _ => continue, // unknown ARPABET token: skip rather than emit garbage
        };
        // Stress mark precedes the VOWEL only (consonants have stress == 9 → no mark).
        out.push_str(match stress {
            1 => "ˈ",
            2 => "ˌ",
            _ => "",
        });
        out.push_str(ipa);
    }
    out
}

/// Split a token into (core, trailing-sentence-punctuation). Only the
/// `split_phonemes` break chars are peeled so batching keeps its boundaries.
fn split_trailing_punct(word: &str) -> (&str, &str) {
    const BREAKS: &[char] = &['.', ',', '!', '?', ';', ':'];
    let trimmed_end = word.trim_end_matches(|c| BREAKS.contains(&c));
    let trailing = &word[trimmed_end.len()..];
    (trimmed_end, trailing)
}

// ── Multilingual G2P via external espeak-ng (non-English Kokoro voices) ───────
//
// Kokoro's non-English voices (es/fr/it/pt/hi, plus ja/zh) were trained on
// espeak-ng phonemes (via misaki). English stays on the pure-Rust `voice-g2p`
// path above (no espeak); other languages shell out to a USER-INSTALLED
// `espeak-ng` (GPL — kept at arm's length as a separate process, never linked)
// and bridge its IPA into Kokoro's phoneme set with `voice_g2p::espeak_ipa_to_kokoro`.

/// Is an external `espeak-ng` binary available on PATH?
///
/// NOT cached: it's a cheap PATH lookup (`espeak-ng --version`) on the non-English
/// path only (which then shells out to espeak-ng anyway, so the relative cost is
/// negligible). A process-lifetime `OnceLock` cache meant the long-lived warm
/// `ds-helper --serve` never noticed an espeak-ng install made MID-SESSION —
/// the helper is restarted only on a provider / full-duplex / stt-engine preference
/// change (see `dontspeakd::tts`), NOT on an espeak install — so a stale cached miss
/// would persist until the next unrelated restart. Re-probing each call fixes that.
pub fn espeak_available() -> bool {
    std::process::Command::new("espeak-ng")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Map a Kokoro language subtag (from `enumerate::kokoro_language`) to the
/// matching `espeak-ng -v` voice code. `None` for English (handled by the pure
/// path) and for families espeak can't cover.
pub fn kokoro_lang_to_espeak(subtag: &str) -> Option<&'static str> {
    match subtag {
        "es" => Some("es"),
        "fr" => Some("fr-fr"),
        "it" => Some("it"),
        "pt" => Some("pt-br"), // Kokoro's `p` family is Brazilian Portuguese
        "hi" => Some("hi"),
        "ja" => Some("ja"),
        "zh" => Some("cmn"), // Mandarin
        _ => None,
    }
}

/// Phonemize `text` for a specific Kokoro `voice`: English uses the pure-Rust
/// path; other languages use external espeak-ng when available, falling back to
/// the English path if espeak is missing/unmapped (callers gate non-English on
/// `espeak_available()`, so that fallback is a defensive last resort).
pub fn phonemize_for(text: &str, voice: &str) -> String {
    let lang = crate::enumerate::kokoro_language(voice);
    // English never shells out; only a non-English mapped voice needs the espeak probe.
    let espeak_ok = needs_espeak(voice) && espeak_available();
    phonemize_lang(text, lang, espeak_ok)
}

/// Like [`phonemize_for`] but with a PRE-PROBED espeak availability. Hot callers that split
/// ONE utterance into many chunks must probe [`espeak_available`] once (gated on
/// [`needs_espeak`]) and pass the result here, rather than re-spawning `espeak-ng --version`
/// per chunk. `espeak_ok` is ignored for English (the pure path).
pub fn phonemize_for_with(text: &str, voice: &str, espeak_ok: bool) -> String {
    phonemize_lang(text, crate::enumerate::kokoro_language(voice), espeak_ok)
}

/// Whether `voice` is a non-English voice that espeak can phonemize — i.e. whether probing
/// `espeak_available()` for it is even worthwhile. Lets a caller skip the probe entirely for
/// English (and unmapped) voices.
pub fn needs_espeak(voice: &str) -> bool {
    let lang = crate::enumerate::kokoro_language(voice);
    lang != "en" && kokoro_lang_to_espeak(lang).is_some()
}

fn phonemize_lang(text: &str, lang: &str, espeak_ok: bool) -> String {
    if lang == "en" {
        return phonemize(text);
    }
    match kokoro_lang_to_espeak(lang) {
        Some(ev) if espeak_ok => phonemize_espeak(text, ev).unwrap_or_else(|| phonemize(text)),
        _ => phonemize(text),
    }
}

/// Run `espeak-ng --ipa -q -v <lang>` over `text` and bridge its IPA into the
/// Kokoro phoneme set. `None` if espeak errors or yields nothing.
fn phonemize_espeak(text: &str, espeak_lang: &str) -> Option<String> {
    let out = std::process::Command::new("espeak-ng")
        .args(["--ipa", "-q", "-v", espeak_lang, text])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ipa = String::from_utf8_lossy(&out.stdout);
    let ipa = ipa.trim();
    if ipa.is_empty() {
        return None;
    }
    let mapped = voice_g2p::espeak_ipa_to_kokoro(ipa);
    (!mapped.trim().is_empty()).then_some(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_lang_to_espeak_maps_supported_families() {
        assert_eq!(kokoro_lang_to_espeak("es"), Some("es"));
        assert_eq!(kokoro_lang_to_espeak("pt"), Some("pt-br"));
        assert_eq!(kokoro_lang_to_espeak("zh"), Some("cmn"));
        assert_eq!(kokoro_lang_to_espeak("en"), None); // pure-Rust path
        assert_eq!(kokoro_lang_to_espeak("other"), None);
    }

    #[test]
    fn phonemize_for_english_voice_uses_pure_path() {
        // English voice never touches espeak — same as phonemize().
        assert_eq!(
            phonemize_for("Hello world", "af_sarah"),
            phonemize("Hello world")
        );
    }

    #[test]
    fn phonemize_hello_world_is_nonempty_and_vocab_mappable() {
        // voice-g2p resolves common words from its embedded dict (no espeak).
        let ph = phonemize("Hello world");
        assert!(!ph.trim().is_empty(), "expected phonemes for common words");
        // Every emitted char that maps must produce a non-empty token list.
        let ids = crate::vocab::tokenize(&ph);
        assert!(!ids.is_empty(), "tokenize should yield ids for hello world");
    }

    #[test]
    fn g2p_output_is_fully_covered_by_kokoro_vocab_for_common_and_edge_words() {
        // THE integration crux: voice-g2p's (misaki) IPA charset must land inside
        // the Kokoro vocab so the bridge in `tokenize` is near-lossless. We assert
        // EVERY char voice-g2p emits for a spread of common + tricky English words
        // is mappable via `vocab_id` — if voice-g2p ever emits a diacritic/variant
        // outside Kokoro's 114-char vocab, `tokenize` would silently drop it and
        // this test catches the regression. (Empirically all of these are covered;
        // misaki and Kokoro share the same phoneme inventory.)
        let words = [
            "hello",
            "world",
            "rhythm",
            "syzygy",
            "queue",
            "tested",
            "the",
            "quick",
            "brown",
            "fox",
            "jumps",
            "over",
            "lazy",
            "dog",
            "strength",
            "sixths",
            "schedule",
            "onomatopoeia",
            "colonel",
            "Wednesday",
            "February",
        ];
        for w in words {
            let ph = voice_g2p::english_to_phonemes(w).unwrap_or_default();
            assert!(!ph.trim().is_empty(), "expected phonemes for {w:?}");
            let unmapped: Vec<char> = ph
                .chars()
                .filter(|c| crate::vocab::vocab_id(*c).is_none())
                .collect();
            assert!(
                unmapped.is_empty(),
                "voice-g2p emitted chars outside KOKORO_VOCAB for {w:?}: \
                 phonemes={ph:?} unmapped={unmapped:?}"
            );
            // And the bridge yields a usable, non-empty token list.
            assert!(
                !crate::vocab::tokenize(&ph).is_empty(),
                "tokenize should yield ids for {w:?} -> {ph:?}"
            );
        }
    }

    #[test]
    fn phonemize_never_panics_on_punctuated_or_odd_input() {
        for t in [
            "",
            "   ",
            "Hello, world! How are you?",
            "Code: foo_bar();",
            "3.14 and 42",
            "zzqxq", // likely OOV; must not panic, must not abort
        ] {
            let _ = phonemize(t);
        }
    }

    #[test]
    fn per_word_path_recovers_oov_and_keeps_punctuation() {
        // Drive the per-word path directly. An OOV token is now SPELLED OUT (never
        // silent), and sentence punctuation is still preserved for the batcher.
        let ph = phonemize_per_word("Hello unresolvableglyphword. world");
        assert!(
            ph.contains('.'),
            "trailing sentence punct preserved: {ph:?}"
        );
        assert!(!ph.trim().is_empty(), "OOV word must not vanish: {ph:?}");
    }

    #[test]
    fn split_trailing_punct_peels_breaks() {
        assert_eq!(split_trailing_punct("world."), ("world", "."));
        assert_eq!(split_trailing_punct("hi?!"), ("hi", "?!"));
        assert_eq!(split_trailing_punct("plain"), ("plain", ""));
        assert_eq!(split_trailing_punct("..."), ("", "..."));
    }

    // ── OOV recovery (espeak-free) — the cross-platform parity fix ────────────────

    #[test]
    fn oov_proper_noun_is_audible_not_silent() {
        // "Nicole" is OOV in the misaki dict; without the fallback it phonemized to
        // EMPTY (silent). It must now be non-empty, mid-phrase AND alone.
        assert!(
            !phonemize("Nicole").trim().is_empty(),
            "Nicole alone must be audible"
        );
        let phrase = phonemize("This was Nicole");
        assert!(
            !phrase.trim().is_empty(),
            "phrase ending in an OOV name must carry the name: {phrase:?}"
        );
        // The lexicon pronunciation (not a spelled-out fallback) is used: it contains
        // the authored Nicole phonemes, not six letter-name tokens.
        assert!(
            phrase.contains("nɪkˈOl"),
            "expected the lexicon pronunciation: {phrase:?}"
        );
    }

    #[test]
    fn unknown_oov_word_is_pronounced_by_the_neural_fallback() {
        // A word with no dict entry and no lexicon entry is PRONOUNCED by the neural
        // predictor (parity with macOS) — audible, in-vocab, and not the spelled-out form.
        let ph = phonemize("Yanchenko");
        assert!(
            !ph.trim().is_empty(),
            "unknown OOV word must be audible, not vanish: {ph:?}"
        );
        let unmapped: Vec<char> = ph
            .chars()
            .filter(|c| crate::vocab::vocab_id(*c).is_none())
            .collect();
        assert!(
            unmapped.is_empty(),
            "neural OOV output must be in-vocab: {ph:?} {unmapped:?}"
        );
        // It's pronounced, not spelled: spelling would contain the letter-name 'O' phoneme
        // ˈO with spaces between every letter; the neural form has no such per-letter spacing.
        let spelled = phonemize_per_word("Yanchenko").matches(' ').count();
        assert!(
            spelled < 7,
            "expected a pronounced word, not 9 spelled letters: {ph:?}"
        );
    }

    #[test]
    fn neural_predictor_loads_and_pronounces() {
        // The bundled model loads and yields in-vocab phonemes for an arbitrary OOV word.
        let ph = neural_phonemes("kubernetes").expect("neural model should pronounce a word");
        assert!(!ph.trim().is_empty());
        let unmapped: Vec<char> = ph
            .chars()
            .filter(|c| crate::vocab::vocab_id(*c).is_none())
            .collect();
        assert!(
            unmapped.is_empty(),
            "neural phonemes out of vocab: {ph:?} {unmapped:?}"
        );
    }

    #[test]
    fn arpabet_bridge_is_vocab_safe_for_all_symbols() {
        // Every ARPABET symbol the predictor can emit (all 15 vowels × 3 stresses + 24
        // consonants) must bridge to phonemes fully inside KOKORO_VOCAB — else a neural
        // OOV word could still drop a char in `tokenize` and clip.
        let vowels = [
            "AA", "AE", "AH", "AO", "AW", "AY", "EH", "ER", "EY", "IH", "IY", "OW", "OY", "UH",
            "UW",
        ];
        let cons = [
            "B", "CH", "D", "DH", "F", "G", "HH", "JH", "K", "L", "M", "N", "NG", "P", "R", "S",
            "SH", "T", "TH", "V", "W", "Y", "Z", "ZH",
        ];
        let mut toks: Vec<String> = Vec::new();
        for v in vowels {
            for s in ["0", "1", "2"] {
                toks.push(format!("{v}{s}"));
            }
        }
        for c in cons {
            toks.push(c.to_string());
        }
        for t in &toks {
            let ipa = arpabet_to_kokoro(&[t.as_str()]);
            assert!(!ipa.is_empty(), "ARPABET {t:?} bridged to empty");
            let unmapped: Vec<char> = ipa
                .chars()
                .filter(|c| crate::vocab::vocab_id(*c).is_none())
                .collect();
            assert!(
                unmapped.is_empty(),
                "ARPABET {t:?} -> {ipa:?} has OOV chars {unmapped:?}"
            );
        }
    }

    #[test]
    fn arpabet_bridge_matches_misaki_stress_style() {
        // ARPABET "N IH0 K OW1 L" must bridge to exactly the misaki-style "nɪkˈOl" (stress
        // mark immediately before the stressed vowel), so neural output reads like the dict.
        assert_eq!(arpabet_to_kokoro(&["N", "IH0", "K", "OW1", "L"]), "nɪkˈOl");
    }

    #[test]
    fn in_vocab_text_is_unchanged_by_the_fallback() {
        // The fast path must be byte-identical to the raw whole-string conversion when
        // nothing is OOV — the fallback adds nothing for common prose.
        let raw = voice_g2p::english_to_phonemes("the quick brown fox").unwrap_or_default();
        assert_eq!(phonemize("the quick brown fox"), raw);
    }

    #[test]
    fn oov_lexicon_is_vocab_safe() {
        // Every hand-authored lexicon pronunciation must map fully into KOKORO_VOCAB
        // (else `tokenize` would silently drop a char and the word would still clip).
        for name in ["nicole", "aoede", "eric", "fenrir", "santa"] {
            let ph = name_lexicon(name).expect("lexicon entry");
            let unmapped: Vec<char> = ph
                .chars()
                .filter(|c| crate::vocab::vocab_id(*c).is_none())
                .collect();
            assert!(
                unmapped.is_empty(),
                "lexicon {name:?} has out-of-vocab chars: {unmapped:?}"
            );
            assert!(
                !crate::vocab::tokenize(ph).is_empty(),
                "lexicon {name:?} tokenizes empty"
            );
        }
    }

    #[test]
    fn every_letter_phoneme_is_vocab_safe() {
        // The spelling fallback must produce only in-vocab phonemes for a..z, so a
        // spelled OOV word is always playable.
        for c in 'a'..='z' {
            let ph = letter_phonemes(c).expect("a..z mapped");
            let unmapped: Vec<char> = ph
                .chars()
                .filter(|c| crate::vocab::vocab_id(*c).is_none())
                .collect();
            assert!(
                unmapped.is_empty(),
                "letter {c:?} has out-of-vocab chars: {unmapped:?}"
            );
            assert!(
                !crate::vocab::tokenize(ph).is_empty(),
                "letter {c:?} tokenizes empty"
            );
        }
    }
}
