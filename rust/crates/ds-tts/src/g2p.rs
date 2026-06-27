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
/// string. Never shells espeak; never returns an error (worst case: a word's
/// phonemes are empty and that word is silent).
pub fn phonemize(text: &str) -> String {
    // Expand digits to English words first — `voice-g2p` (misaki) has no number
    // frontend, so a bare "12" would drop at the vocab layer (silent). English
    // only: non-English voices reach espeak (which expands in-language) and never
    // call this. See [`crate::numbers`].
    let text = crate::numbers::expand_numbers(text);
    match voice_g2p::english_to_phonemes(&text) {
        Ok(ph) => ph,
        Err(_) => phonemize_resilient(&text),
    }
}

/// Per-word resilient phonemization: split on whitespace, phonemize each token
/// alone, drop a token whose conversion errors, and emit trailing punctuation
/// between words so sentence batching downstream still works.
fn phonemize_resilient(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace() {
        // Separate any leading/trailing ASCII punctuation the batcher cares about
        // so a word like "world." still contributes the '.' even if the core
        // word fails to convert.
        let (core, trailing) = split_trailing_punct(word);
        if !core.is_empty()
            && let Ok(ph) = voice_g2p::english_to_phonemes(core)
        {
            let ph = ph.trim();
            if !ph.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(ph);
            }
        }
        // On Err: skip this word entirely (graceful degradation).
        if !trailing.is_empty() {
            out.push_str(trailing);
        }
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
    fn resilient_path_skips_a_bad_word_but_keeps_punctuation() {
        // Drive the resilient path directly. Even if a token is OOV and yields
        // nothing, sentence punctuation is preserved for the batcher.
        let ph = phonemize_resilient("Hello unresolvableglyphword. world");
        // The '.' must survive so split_phonemes can break the batch there.
        assert!(
            ph.contains('.'),
            "trailing sentence punct preserved: {ph:?}"
        );
    }

    #[test]
    fn split_trailing_punct_peels_breaks() {
        assert_eq!(split_trailing_punct("world."), ("world", "."));
        assert_eq!(split_trailing_punct("hi?!"), ("hi", "?!"));
        assert_eq!(split_trailing_punct("plain"), ("plain", ""));
        assert_eq!(split_trailing_punct("..."), ("", "..."));
    }
}
