//! Shared per-utterance language detection for every TTS backend.

use std::sync::OnceLock;

use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};

pub const DEFAULT_LANGUAGE: &str = "en";

const LANGUAGES: &[Language] = &[
    Language::Arabic,
    Language::Bokmal,
    Language::Chinese,
    Language::Danish,
    Language::Dutch,
    Language::English,
    Language::Finnish,
    Language::French,
    Language::German,
    Language::Greek,
    Language::Hebrew,
    Language::Hindi,
    Language::Italian,
    Language::Japanese,
    Language::Korean,
    Language::Malay,
    Language::Polish,
    Language::Portuguese,
    Language::Russian,
    Language::Spanish,
    Language::Swahili,
    Language::Swedish,
    Language::Turkish,
];

fn detector() -> &'static LanguageDetector {
    static DETECTOR: OnceLock<LanguageDetector> = OnceLock::new();
    DETECTOR.get_or_init(|| {
        LanguageDetectorBuilder::from_languages(LANGUAGES)
            .with_minimum_relative_distance(0.15)
            .build()
    })
}

/// Ambiguous / unspeakable → `en`.
pub fn detect_language(text: &str) -> String {
    let prose = crate::normalize_spoken_text(text);
    let detected = detector().detect_language_of(&prose);
    let code = detected.map(language_code).unwrap_or(DEFAULT_LANGUAGE);
    // Which language an utterance got is the first thing to check when speech comes out
    // in the wrong voice or a frontend refuses it, and the fallback to `en` is otherwise
    // indistinguishable from a confident English detection. Logs the classified prose
    // length rather than the prose, which is user speech content.
    log::debug!(
        target: "tts",
        "lingua detected {code}{} over {} chars",
        if detected.is_none() { " (no match, default)" } else { "" },
        prose.chars().count()
    );
    code.to_string()
}

fn language_code(language: Language) -> &'static str {
    match language {
        Language::Arabic => "ar",
        Language::Bokmal => "no",
        Language::Chinese => "zh",
        Language::Danish => "da",
        Language::Dutch => "nl",
        Language::English => "en",
        Language::Finnish => "fi",
        Language::French => "fr",
        Language::German => "de",
        Language::Greek => "el",
        Language::Hebrew => "he",
        Language::Hindi => "hi",
        Language::Italian => "it",
        Language::Japanese => "ja",
        Language::Korean => "ko",
        Language::Malay => "ms",
        Language::Polish => "pl",
        Language::Portuguese => "pt",
        Language::Russian => "ru",
        Language::Spanish => "es",
        Language::Swahili => "sw",
        Language::Swedish => "sv",
        Language::Turkish => "tr",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_representative_scripts() {
        assert_eq!(
            detect_language("This response is written in English."),
            "en"
        );
        assert_eq!(
            detect_language("Этот ответ написан на русском языке."),
            "ru"
        );
        assert_eq!(detect_language("この回答は日本語で書かれています。"), "ja");
        assert_eq!(detect_language("이 답변은 한국어로 작성되었습니다."), "ko");
    }

    #[test]
    fn normalizes_markdown_before_detection() {
        assert_eq!(detect_language("**Bonjour**, comment allez-vous ?"), "fr");
    }

    #[test]
    fn detects_kokoro_espeak_languages() {
        // The espeak-backed Kokoro languages, so a detection regression that routed one
        // of these to English (silently wrong voice) fails here.
        assert_eq!(
            detect_language("Ciao, oggi è una bella giornata di sole."),
            "it"
        );
        assert_eq!(detect_language("Hola, hoy hace un día muy bonito."), "es");
        assert_eq!(detect_language("Olá, hoje está um dia muito bonito."), "pt");
    }

    #[test]
    fn defaults_to_english_when_detection_has_no_evidence() {
        for text in ["", "   ", "12345", "🎉"] {
            assert_eq!(detect_language(text), DEFAULT_LANGUAGE);
        }
    }
}
