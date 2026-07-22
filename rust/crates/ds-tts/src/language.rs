//! Shared language detection for every TTS backend.
//!
//! Two entry points. [`detect_language`] classifies one text and is what backends use
//! when the text is all they have. [`chunk_language`] decides what a *spoken chunk* is
//! spoken in: the engine calls it once per queued utterance, so a reply that switches
//! language mid-way is voiced per utterance rather than under one turn-wide verdict.

use std::sync::OnceLock;

use whatlang::{Detector, Info, Lang};

pub const DEFAULT_LANGUAGE: &str = "en";

/// Canonical `whatlang::Lang` ↔ two-letter ISO 639-1 code table — the single source for
/// [`language_code`], [`lang_for_code`], and [`full_range`], so the code a detector can
/// return and the code the mapper accepts cannot drift. Two-letter 639-1 is what every
/// model frontend consumes (whatlang's own `code()` is three-letter 639-3, which no model
/// accepts). Malay (`ms`) and Swahili (`sw`) are in Chatterbox's set but have no whatlang
/// variant, so they are absent here: their text detects as a related allowed language or
/// falls back to English — the accepted cost of whatlang over the much larger lingua models.
const LANG_CODES: &[(Lang, &str)] = &[
    (Lang::Ara, "ar"),
    (Lang::Cmn, "zh"),
    (Lang::Dan, "da"),
    (Lang::Deu, "de"),
    (Lang::Ell, "el"),
    (Lang::Eng, "en"),
    (Lang::Fin, "fi"),
    (Lang::Fra, "fr"),
    (Lang::Heb, "he"),
    (Lang::Hin, "hi"),
    (Lang::Ita, "it"),
    (Lang::Jpn, "ja"),
    (Lang::Kor, "ko"),
    (Lang::Nld, "nl"),
    (Lang::Nob, "no"),
    (Lang::Pol, "pl"),
    (Lang::Por, "pt"),
    (Lang::Rus, "ru"),
    (Lang::Spa, "es"),
    (Lang::Swe, "sv"),
    (Lang::Tur, "tr"),
];

/// Built once per model and shared by reference across threads — the engine detects on its
/// TTS worker while other callers may detect concurrently. `detect_lang` takes `&self` and
/// holds no interior mutability, so the shared `&Detector` needs no lock; this asserts the
/// `Sync` that the `static` relies on stays true.
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Detector>();
};

/// `whatlang::Lang` → the two-letter ISO 639-1 code every model frontend consumes.
fn language_code(language: Lang) -> &'static str {
    LANG_CODES
        .iter()
        .find(|(lang, _)| *lang == language)
        .map(|(_, code)| *code)
        .unwrap_or(DEFAULT_LANGUAGE)
}

/// Two-letter ISO 639-1 code → its `whatlang::Lang`. `None` for codes with no whatlang
/// variant (e.g. `ms`/`sw`), which therefore cannot enter a detector allowlist.
fn lang_for_code(code: &str) -> Option<Lang> {
    LANG_CODES
        .iter()
        .find(|(_, c)| *c == code)
        .map(|(lang, _)| *lang)
}

/// Every `whatlang::Lang` in the table — the detector range for models that accept any
/// language or select one internally.
fn full_range() -> Vec<Lang> {
    LANG_CODES.iter().map(|(lang, _)| *lang).collect()
}

/// The detector allowlist for a model: the whatlang variants of the languages it declares.
fn model_allowlist(model: ds_config::TtsModel) -> Vec<Lang> {
    let languages = model.descriptor().languages;
    // Empty or the `auto` sentinel (OmniVoice) → detect across the full mapped range:
    // the model either accepts any language or selects one internally. General rule,
    // not an OmniVoice special-case.
    if languages.is_empty() || languages.contains(&"auto") {
        return full_range();
    }
    languages
        .iter()
        .filter_map(|code| lang_for_code(code))
        .collect()
}

/// Per-model detector, cached. Indexing by `model as usize` mirrors `descriptor()`
/// (`TTS_MODELS[self as usize]`), so the model's declared languages scope its allowlist.
fn detector_for(model: ds_config::TtsModel) -> &'static Detector {
    static DETECTORS: [OnceLock<Detector>; ds_config::TtsModel::ALL.len()] =
        [const { OnceLock::new() }; ds_config::TtsModel::ALL.len()];
    DETECTORS[model as usize].get_or_init(|| Detector::with_allowlist(model_allowlist(model)))
}

/// One whatlang verdict scoped to `model`: the ISO code plus how much evidence backs it.
struct Classified {
    code: &'static str,
    /// `0.0` when nothing matched — the `en` below is a default, not a reading.
    confidence: f64,
    /// whatlang's own bar. Strict by design: it wants paragraph-length prose.
    reliable: bool,
}

fn classify(text: &str, model: ds_config::TtsModel) -> Classified {
    let prose = crate::normalize_spoken_text(text);
    let detected: Option<Info> = detector_for(model).detect(&prose);
    let code = detected
        .as_ref()
        .map(|info| language_code(info.lang()))
        .unwrap_or(DEFAULT_LANGUAGE);
    // Which language an utterance got is the first thing to check when speech comes out in
    // the wrong voice, and the fallback to `en` is otherwise indistinguishable from a
    // confident English detection. Logs the classified prose length rather than the prose,
    // which is user speech content.
    log::debug!(
        target: "tts",
        "whatlang detected {code} for {}{} at confidence {:.2} over {} chars",
        model.as_str(),
        if detected.is_none() { " (no match, default)" } else { "" },
        detected.as_ref().map_or(0.0, Info::confidence),
        prose.chars().count()
    );
    Classified {
        code,
        confidence: detected.as_ref().map_or(0.0, Info::confidence),
        reliable: detected.as_ref().is_some_and(Info::is_reliable),
    }
}

/// Detect the language of `text` scoped to `model`'s supported set; ambiguous / unspeakable
/// → `en`. Because the allowlist is derived from `descriptor().languages` and
/// `language_code` is its exact inverse, every non-fallback result is a language the model
/// can speak. The `en` fallback is valid because every built-in model supports `en`.
pub fn detect_language(text: &str, model: ds_config::TtsModel) -> String {
    classify(text, model).code.to_string()
}

/// Evidence a chunk needs to be voiced in the language it reads as. whatlang's own
/// `is_reliable` wants roughly a paragraph, which a one-line digest never has, but below
/// that bar its confidence still separates the two failure directions at digest length:
/// English lines that mis-top-1 as a Romance language sit under 0.25, while short prose
/// that really is Italian, Spanish, French, or Portuguese lands at 0.3 and above
/// (`digest_confidence_separates_english_from_the_rest` holds the measured table). Below
/// this, weak evidence loses to the turn's language or to English — the safer error,
/// since these replies are overwhelmingly English and a foreign voice on English text is
/// the failure users hear first.
const MIN_CHUNK_CONFIDENCE: f64 = 0.3;

/// Language for one spoken chunk. Each chunk is classified on its own text, so a reply
/// that switches language is voiced per utterance. `corpus` is the surrounding turn text
/// when the caller has it (narration sends the message-so-far); it is consulted only when
/// the chunk's own evidence is too thin to stand, letting a bare digest inherit the
/// language of the reply it came from instead of taking a coin flip.
pub fn chunk_language(chunk: &str, corpus: Option<&str>, model: ds_config::TtsModel) -> String {
    let own = classify(chunk, model);
    if own.reliable || own.confidence >= MIN_CHUNK_CONFIDENCE {
        return own.code.to_string();
    }
    match corpus.map(|text| classify(text, model)) {
        Some(turn) if turn.reliable => turn.code.to_string(),
        _ => DEFAULT_LANGUAGE.to_string(),
    }
}

/// Non-refusing clamp for an already-scoped code — used by the engine before it speaks an
/// item and by the warm helper, which trusts the code it gets over IPC and must never drop.
/// Guards a model-switch race: a code detected (or pinned) under one model that the live
/// model can't speak resolves to that model's default.
pub fn supported_language(language: &str, model: ds_config::TtsModel) -> String {
    let descriptor = model.descriptor();
    if descriptor.accepts_detected_language(language) {
        language.to_string()
    } else {
        descriptor.default_language.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ds_config::TtsModel;

    #[test]
    fn detects_representative_scripts() {
        // Chatterbox's language set covers all of these.
        let m = TtsModel::Chatterbox;
        assert_eq!(
            detect_language("This response is written in English.", m),
            "en"
        );
        assert_eq!(
            detect_language("Этот ответ написан на русском языке.", m),
            "ru"
        );
        assert_eq!(
            detect_language("この回答は日本語で書かれています。", m),
            "ja"
        );
        assert_eq!(
            detect_language("이 답변은 한국어로 작성되었습니다.", m),
            "ko"
        );
    }

    #[test]
    fn normalizes_markdown_before_detection() {
        assert_eq!(
            detect_language("**Bonjour**, comment allez-vous ?", TtsModel::Chatterbox),
            "fr"
        );
    }

    #[test]
    fn detects_kokoro_espeak_languages() {
        // The espeak-backed Kokoro languages, so a detection regression that routed one
        // of these to English (silently wrong voice) fails here.
        let m = TtsModel::Kokoro;
        assert_eq!(
            detect_language("Ciao, oggi è una bella giornata di sole.", m),
            "it"
        );
        assert_eq!(
            detect_language("Hola, hoy hace un día muy bonito.", m),
            "es"
        );
        assert_eq!(
            detect_language("Olá, hoje está um dia muito bonito.", m),
            "pt"
        );
    }

    #[test]
    fn defaults_to_english_when_detection_has_no_evidence() {
        for text in ["", "   ", "12345", "🎉"] {
            assert_eq!(detect_language(text, TtsModel::Kokoro), DEFAULT_LANGUAGE);
        }
    }

    #[test]
    fn detection_is_scoped_to_the_selected_model() {
        // Russian input. Kokoro has no Russian, so scoping falls it back to a supported code
        // (never a drop, never "ru"). Qwen/Chatterbox support Russian, so they keep it, and
        // OmniVoice's full range keeps it too.
        let russian = "Этот ответ написан на русском языке.";
        let kokoro = detect_language(russian, TtsModel::Kokoro);
        assert_ne!(kokoro, "ru");
        assert!(TtsModel::Kokoro.descriptor().supports_language(&kokoro));
        assert_eq!(detect_language(russian, TtsModel::Qwen), "ru");
        assert_eq!(detect_language(russian, TtsModel::Chatterbox), "ru");
        assert_eq!(detect_language(russian, TtsModel::OmniVoice), "ru");
    }

    #[test]
    fn supported_language_clamps_unsupported_to_the_model_default() {
        // Warm-helper clamp: a code the model can't speak becomes its default; a supported
        // code passes through; OmniVoice accepts anything.
        assert_eq!(supported_language("ru", TtsModel::Kokoro), "en");
        assert_eq!(supported_language("ru", TtsModel::Qwen), "ru");
        assert_eq!(supported_language("ru", TtsModel::OmniVoice), "ru");
    }

    #[test]
    fn lang_codes_round_trip_both_directions() {
        for (lang, code) in LANG_CODES {
            assert_eq!(language_code(*lang), *code);
            assert_eq!(lang_for_code(code), Some(*lang));
        }
    }

    #[test]
    fn every_detectable_code_has_an_omnivoice_prompt_token() {
        let descriptor = TtsModel::OmniVoice.descriptor();
        for (_, code) in LANG_CODES {
            let token = descriptor.runtime_language(code);
            assert!(!token.is_empty(), "{code} maps to an empty prompt token");
        }
        assert_eq!(descriptor.runtime_language("ar"), "arb");
        assert_eq!(descriptor.runtime_language("no"), "nb");
        assert_eq!(descriptor.runtime_language("auto"), "en");
    }

    #[test]
    fn codes_without_a_whatlang_variant_are_absent() {
        assert_eq!(lang_for_code("ms"), None);
        assert_eq!(lang_for_code("sw"), None);
        // The allowlist skips them without panic and still builds a usable detector.
        let allow = model_allowlist(TtsModel::Chatterbox);
        assert!(!allow.is_empty());
        let _ = Detector::with_allowlist(allow);
    }

    #[test]
    fn english_is_always_in_the_allowlist() {
        // Fallback safety: `detect_language`'s no-evidence `en` must be a valid result for
        // every model's detector.
        for model in TtsModel::ALL.iter().copied() {
            assert!(model_allowlist(model).contains(&Lang::Eng));
        }
    }

    /// Reply that opens in Italian and closes in English, as narration delivers it: one
    /// utterance per blockquote, each with the message-so-far as its corpus.
    const ITALIAN_QUOTE: &str = concat!(
        "Oggi è una giornata tranquilla e luminosa, e mi fa davvero piacere poter ",
        "scambiare due parole con te in italiano, una lingua che ha un ritmo caldo e ",
        "musicale che si sente subito quando viene letta ad alta voce."
    );
    const ENGLISH_QUOTE: &str = concat!(
        "Today has been a calm and bright sort of day, and it is genuinely a pleasure to ",
        "be able to trade a few words with you in English, a language whose rhythm sounds ",
        "quite different once it is read aloud."
    );

    #[test]
    fn a_chunk_with_prose_of_its_own_outranks_the_turn_corpus() {
        // The bug this policy exists for: the English quote arrives with a corpus whose
        // first (and longer) half is Italian, and must still be spoken in English.
        let corpus = format!("> {ITALIAN_QUOTE}\n\n> {ENGLISH_QUOTE}");
        for model in [TtsModel::Kokoro, TtsModel::Chatterbox] {
            assert_eq!(chunk_language(ITALIAN_QUOTE, Some(&corpus), model), "it");
            assert_eq!(chunk_language(ENGLISH_QUOTE, Some(&corpus), model), "en");
            // A mixed corpus resolves to a single language (here English) — whichever it
            // is, one of the two quotes would be voiced wrong under a turn-wide verdict.
            assert_eq!(detect_language(&corpus, model), "en");
        }
    }

    #[test]
    fn a_digest_too_short_to_classify_inherits_the_turn() {
        // Regression: short digests alone ("Bon courage.") false-positive as FR/PT. With an
        // English turn behind them they stay English; with an Italian one they follow it.
        let english_turn = concat!(
            "This assistant reply is written entirely in clear English prose so language ",
            "detection has a solid corpus for the whole turn.\n\n> Bon courage.\n\n",
            "More English body after the short digest keeps the turn unambiguous."
        );
        let italian_turn = format!("{ITALIAN_QUOTE}\n\n> Grazie mille.");
        for model in [TtsModel::Kokoro, TtsModel::Chatterbox] {
            assert_eq!(detect_language(english_turn, model), "en");
            assert_eq!(
                chunk_language("Bon courage.", Some(english_turn), model),
                "en"
            );
            assert_eq!(
                chunk_language("Grazie mille.", Some(&italian_turn), model),
                "it"
            );
            // No corpus and nothing solid of its own → English, never a coin flip.
            assert_eq!(chunk_language("Grazie mille.", None, model), "en");
        }
    }

    #[test]
    fn digest_confidence_separates_english_from_the_rest() {
        // Measured basis for `MIN_CHUNK_CONFIDENCE`: one-line digests, no corpus. English
        // lines never leave English (some top-1 as `fr` under 0.2); non-English lines with
        // enough trigram evidence get their language, and the rest fall back to English —
        // wrong voice for that line, but never a foreign voice on English text.
        const DIGESTS: &[(&str, &str)] = &[
            ("en", "Hello, it is nice to meet you."),
            ("en", "Let me check the logs."),
            ("en", "Two files changed, one test added."),
            ("en", "That should do it."),
            ("en", "Done."),
            ("en", "The build is green and all tests pass."),
            ("it", "Ciao, è un piacere conoscerti."),
            ("it", "Buongiorno, come stai oggi?"),
            ("it", "Ecco quello che ho trovato."),
            ("en", "Ciao!"),
            ("es", "Hola, encantado de conocerte."),
            ("es", "Todas las pruebas han pasado."),
            ("fr", "Bonjour, comment allez-vous ?"),
            ("en", "Bon courage."),
            ("pt", "Olá, prazer em conhecê-lo."),
            ("pt", "Todos os testes passaram."),
        ];
        for model in [TtsModel::Kokoro, TtsModel::Chatterbox] {
            for (want, digest) in DIGESTS {
                assert_eq!(
                    &chunk_language(digest, None, model),
                    want,
                    "{digest:?} on {}",
                    model.as_str()
                );
            }
        }
    }

    #[test]
    fn allowlist_matches_the_descriptor_languages() {
        // Structural guarantee: the auto/empty case detects across the full range (OmniVoice
        // is `["auto"]`), every other model only over codes it actually declares.
        for model in TtsModel::ALL.iter().copied() {
            let descriptor = model.descriptor();
            let allow = model_allowlist(model);
            if descriptor.languages.is_empty() || descriptor.languages.contains(&"auto") {
                assert_eq!(allow, full_range());
            } else {
                for lang in allow {
                    assert!(descriptor.supports_language(language_code(lang)));
                }
            }
        }
    }
}
