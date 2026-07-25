//! Shared language detection for every TTS backend.
//!
//! [`detect_language`] classifies one text; [`chunk_language`] scopes a spoken chunk so
//! mid-reply language switches voice per utterance, not one turn-wide verdict.

use std::sync::OnceLock;

use whatlang::{Detector, Info, Lang};

pub const DEFAULT_LANGUAGE: &str = "en";

/// Single source for [`language_code`] / [`lang_for_code`] / [`full_range`] (639-1;
//! whatlang's own codes are 639-3). `ms`/`sw` lack whatlang variants — absent here.
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

/// `Detector` is `Sync` — shared statics need no lock.
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Detector>();
};

/// `whatlang::Lang` → ISO 639-1 for model frontends.
fn language_code(language: Lang) -> &'static str {
    LANG_CODES
        .iter()
        .find(|(lang, _)| *lang == language)
        .map(|(_, code)| *code)
        .unwrap_or(DEFAULT_LANGUAGE)
}

/// ISO 639-1 → `whatlang::Lang`; `None` when no whatlang variant (e.g. `ms`/`sw`).
fn lang_for_code(code: &str) -> Option<Lang> {
    LANG_CODES
        .iter()
        .find(|(_, c)| *c == code)
        .map(|(lang, _)| *lang)
}

/// Full table range for models that accept any language or select internally.
fn full_range() -> Vec<Lang> {
    LANG_CODES.iter().map(|(lang, _)| *lang).collect()
}

/// whatlang allowlist from the model's declared languages.
fn model_allowlist(model: ds_config::TtsModel) -> Vec<Lang> {
    let languages = model.descriptor().languages;
    // Empty / `auto` → full range (model accepts any or picks internally).
    if languages.is_empty() || languages.contains(&"auto") {
        return full_range();
    }
    languages
        .iter()
        .filter_map(|code| lang_for_code(code))
        .collect()
}

/// Cached per-model detector (`model as usize` matches `descriptor()` indexing).
fn detector_for(model: ds_config::TtsModel) -> &'static Detector {
    static DETECTORS: [OnceLock<Detector>; ds_config::TtsModel::ALL.len()] =
        [const { OnceLock::new() }; ds_config::TtsModel::ALL.len()];
    DETECTORS[model as usize].get_or_init(|| Detector::with_allowlist(model_allowlist(model)))
}

fn detector_for_any_language() -> &'static Detector {
    static DETECTOR: OnceLock<Detector> = OnceLock::new();
    DETECTOR.get_or_init(|| Detector::with_allowlist(full_range()))
}

/// whatlang verdict: ISO code + confidence evidence.
struct Classified {
    code: &'static str,
    /// `0.0` when nothing matched (`en` is a default, not a reading).
    confidence: f64,
    /// whatlang's own bar (paragraph-length prose).
    reliable: bool,
}

fn classify_with(text: &str, detector: &Detector, target: &str) -> Classified {
    let prose = crate::normalize_spoken_text(text);
    let detected: Option<Info> = detector.detect(&prose);
    let code = detected
        .as_ref()
        .map(|info| language_code(info.lang()))
        .unwrap_or(DEFAULT_LANGUAGE);
    // Length only — prose is user content. Distinguishes en-default from confident en.
    log::debug!(
        target: "tts",
        "whatlang detected {code} for {}{} at confidence {:.2} over {} chars",
        target,
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

fn classify(text: &str, model: ds_config::TtsModel) -> Classified {
    classify_with(text, detector_for(model), model.as_str())
}

fn classify_any_language(text: &str) -> Classified {
    classify_with(text, detector_for_any_language(), "system")
}

/// Language of `text` scoped to `model`; ambiguous / unspeakable → `en`
/// (every built-in supports `en`; non-fallback codes are always speakable).
pub fn detect_language(text: &str, model: ds_config::TtsModel) -> String {
    classify(text, model).code.to_string()
}

/// Digest-length confidence floor. `is_reliable` wants paragraphs; below that, measured
/// table in `digest_confidence_separates_english_from_the_rest`: weak Romance mis-top-1s
/// on English sit under 0.25; real short non-English lands ≥0.3. Below this, prefer turn
/// language or English (foreign voice on English is the worst failure mode).
const MIN_CHUNK_CONFIDENCE: f64 = 0.3;

/// Per-chunk language. Own text first; `corpus` (message-so-far) only when evidence is thin.
pub fn chunk_language(chunk: &str, corpus: Option<&str>, model: ds_config::TtsModel) -> String {
    choose_chunk_language(chunk, corpus, |text| classify(text, model))
}

/// System speech: full mapped range, same short-chunk confidence policy.
pub fn chunk_language_any(chunk: &str, corpus: Option<&str>) -> String {
    choose_chunk_language(chunk, corpus, classify_any_language)
}

fn choose_chunk_language(
    chunk: &str,
    corpus: Option<&str>,
    classify: impl Fn(&str) -> Classified,
) -> String {
    let own = classify(chunk);
    if own.reliable || own.confidence >= MIN_CHUNK_CONFIDENCE {
        return own.code.to_string();
    }
    match corpus.map(classify) {
        Some(turn) if turn.reliable => turn.code.to_string(),
        _ => DEFAULT_LANGUAGE.to_string(),
    }
}

/// Clamp an already-scoped code to `model` (model-switch race; IPC path must never drop).
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
        // espeak-backed Kokoro langs — silent en-fallback would wrong-voice them.
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
        // Kokoro lacks Russian → supported code; models that support it keep `ru`.
        let russian = "Этот ответ написан на русском языке.";
        let kokoro = detect_language(russian, TtsModel::Kokoro);
        assert_ne!(kokoro, "ru");
        assert!(TtsModel::Kokoro.descriptor().supports_language(&kokoro));
        assert_eq!(detect_language(russian, TtsModel::Qwen), "ru");
        assert_eq!(detect_language(russian, TtsModel::Chatterbox), "ru");
        assert_eq!(detect_language(russian, TtsModel::OmniVoice), "ru");
    }

    #[test]
    fn system_detection_is_not_scoped_to_the_built_in_model() {
        let russian = "Этот ответ написан на русском языке.";
        assert_eq!(chunk_language_any(russian, None), "ru");
        assert_ne!(chunk_language(russian, None, TtsModel::Kokoro), "ru");
    }

    #[test]
    fn supported_language_clamps_unsupported_to_the_model_default() {
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
        let allow = model_allowlist(TtsModel::Chatterbox);
        assert!(!allow.is_empty());
        let _ = Detector::with_allowlist(allow);
    }

    #[test]
    fn english_is_always_in_the_allowlist() {
        // no-evidence `en` fallback must be valid for every model.
        for model in TtsModel::ALL.iter().copied() {
            assert!(model_allowlist(model).contains(&Lang::Eng));
        }
    }

    /// Narration-style fixtures: one utterance per blockquote, message-so-far as corpus.
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
        // English quote must stay en even when corpus is majority Italian.
        let corpus = format!("> {ITALIAN_QUOTE}\n\n> {ENGLISH_QUOTE}");
        for model in [TtsModel::Kokoro, TtsModel::Chatterbox] {
            assert_eq!(chunk_language(ITALIAN_QUOTE, Some(&corpus), model), "it");
            assert_eq!(chunk_language(ENGLISH_QUOTE, Some(&corpus), model), "en");
            // Turn-wide would wrong-voice one of the two.
            assert_eq!(detect_language(&corpus, model), "en");
        }
    }

    #[test]
    fn a_digest_too_short_to_classify_inherits_the_turn() {
        // Short digests false-positive alone; inherit turn language when present.
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
            assert_eq!(chunk_language("Grazie mille.", None, model), "en");
        }
    }

    #[test]
    fn digest_confidence_separates_english_from_the_rest() {
        // Measured basis for `MIN_CHUNK_CONFIDENCE` (one-line digests, no corpus).
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
        // auto/empty → full range; else only declared codes.
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
