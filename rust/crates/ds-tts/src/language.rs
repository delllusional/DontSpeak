//! Shared per-utterance language detection for every TTS backend.

use std::sync::OnceLock;

use whatlang::{Detector, Lang};

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
    languages.iter().filter_map(|code| lang_for_code(code)).collect()
}

/// Per-model detector, cached. Indexing by `model as usize` mirrors `descriptor()`
/// (`TTS_MODELS[self as usize]`), so the model's declared languages scope its allowlist.
fn detector_for(model: ds_config::TtsModel) -> &'static Detector {
    static DETECTORS: [OnceLock<Detector>; ds_config::TtsModel::ALL.len()] =
        [const { OnceLock::new() }; ds_config::TtsModel::ALL.len()];
    DETECTORS[model as usize].get_or_init(|| Detector::with_allowlist(model_allowlist(model)))
}

/// Detect the language of `text` scoped to `model`'s supported set; ambiguous / unspeakable
/// → `en`. Because the allowlist is derived from `descriptor().languages` and
/// [`language_code`] is its exact inverse, every non-fallback result is a language the model
/// can speak. The `en` fallback is valid because every built-in model supports `en`.
pub fn detect_language(text: &str, model: ds_config::TtsModel) -> String {
    let prose = crate::normalize_spoken_text(text);
    let detected = detector_for(model).detect_lang(&prose);
    let code = detected.map(language_code).unwrap_or(DEFAULT_LANGUAGE);
    // Which language an utterance got is the first thing to check when speech comes out in
    // the wrong voice, and the fallback to `en` is otherwise indistinguishable from a
    // confident English detection. Logs the classified prose length rather than the prose,
    // which is user speech content.
    log::debug!(
        target: "tts",
        "whatlang detected {code} for {}{} over {} chars",
        model.as_str(),
        if detected.is_none() { " (no match, default)" } else { "" },
        prose.chars().count()
    );
    code.to_string()
}

/// Non-refusing clamp used only by the warm helper, which trusts the engine's already-
/// scoped code over IPC and must never drop. Guards a model-switch race: if the model
/// changed after the engine detected, clamp an unsupported code to the model's default.
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
        assert_eq!(detect_language("This response is written in English.", m), "en");
        assert_eq!(detect_language("Этот ответ написан на русском языке.", m), "ru");
        assert_eq!(detect_language("この回答は日本語で書かれています。", m), "ja");
        assert_eq!(detect_language("이 답변은 한국어로 작성되었습니다.", m), "ko");
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
        assert_eq!(detect_language("Ciao, oggi è una bella giornata di sole.", m), "it");
        assert_eq!(detect_language("Hola, hoy hace un día muy bonito.", m), "es");
        assert_eq!(detect_language("Olá, hoje está um dia muito bonito.", m), "pt");
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
