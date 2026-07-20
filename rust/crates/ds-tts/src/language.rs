//! Shared per-utterance language detection for every TTS backend.

use std::sync::OnceLock;

use whatlang::{Detector, Lang};

pub const DEFAULT_LANGUAGE: &str = "en";

/// The languages any built-in model can speak, as the `whatlang` variants that map to a
/// two-letter code below. Restricting the detector to this set (rather than whatlang's
/// full 80+) keeps it from returning a language no model supports and improves accuracy
/// by removing confusable candidates. Malay and Swahili are in Chatterbox's set but not
/// in whatlang, so their text detects as a related allowed language or falls back to
/// English — the accepted cost of whatlang over the much larger lingua models.
const LANGUAGES: &[Lang] = &[
    Lang::Ara,
    Lang::Cmn,
    Lang::Dan,
    Lang::Deu,
    Lang::Ell,
    Lang::Eng,
    Lang::Fin,
    Lang::Fra,
    Lang::Heb,
    Lang::Hin,
    Lang::Ita,
    Lang::Jpn,
    Lang::Kor,
    Lang::Nld,
    Lang::Nob,
    Lang::Pol,
    Lang::Por,
    Lang::Rus,
    Lang::Spa,
    Lang::Swe,
    Lang::Tur,
];

/// Built once and shared by reference across threads — the engine detects on its TTS
/// worker while other callers may detect concurrently. `detect_lang` takes `&self` and
/// holds no interior mutability, so the shared `&Detector` needs no lock; this asserts the
/// `Sync` that the `static` relies on stays true.
const _: fn() = || {
    fn assert_sync<T: Sync>() {}
    assert_sync::<Detector>();
};

fn detector() -> &'static Detector {
    static DETECTOR: OnceLock<Detector> = OnceLock::new();
    DETECTOR.get_or_init(|| Detector::with_allowlist(LANGUAGES.to_vec()))
}

/// Ambiguous / unspeakable → `en`.
pub fn detect_language(text: &str) -> String {
    let prose = crate::normalize_spoken_text(text);
    let detected = detector().detect_lang(&prose);
    let code = detected.map(language_code).unwrap_or(DEFAULT_LANGUAGE);
    // Which language an utterance got is the first thing to check when speech comes out
    // in the wrong voice or a frontend refuses it, and the fallback to `en` is otherwise
    // indistinguishable from a confident English detection. Logs the classified prose
    // length rather than the prose, which is user speech content.
    log::debug!(
        target: "tts",
        "whatlang detected {code}{} over {} chars",
        if detected.is_none() { " (no match, default)" } else { "" },
        prose.chars().count()
    );
    code.to_string()
}

/// The single decision — shared by every synthesis path (engine gate, one-shot, warm
/// helper) — of whether a model can speak a detected language, and the one wording of
/// the refusal. Kept here next to [`detect_language`] so the check and the codes it
/// validates cannot drift. `Ok` when the model accepts the code (OmniVoice auto-detects,
/// so it accepts anything).
pub fn ensure_model_speaks(language: &str, model: ds_config::TtsModel) -> Result<(), String> {
    if model.descriptor().accepts_detected_language(language) {
        Ok(())
    } else {
        Err(format!(
            "detected language `{language}` is not supported by {}",
            model.as_str()
        ))
    }
}

/// [`detect_language`] followed by [`ensure_model_speaks`], for the paths that start from
/// raw text (one-shot; warm helper when it re-detects). The engine detects earlier — for
/// its status gate — so it calls the two halves separately.
pub fn detect_supported_language(text: &str, model: ds_config::TtsModel) -> Result<String, String> {
    let language = detect_language(text);
    ensure_model_speaks(&language, model)?;
    Ok(language)
}

/// `whatlang::Lang` → the two-letter ISO 639-1 code every model frontend consumes
/// (whatlang's own `code()` is three-letter 639-3, which no model accepts). Exhaustive
/// over [`LANGUAGES`]; any other variant cannot occur under the allowlist and defaults.
fn language_code(language: Lang) -> &'static str {
    match language {
        Lang::Ara => "ar",
        Lang::Cmn => "zh",
        Lang::Dan => "da",
        Lang::Deu => "de",
        Lang::Ell => "el",
        Lang::Eng => "en",
        Lang::Fin => "fi",
        Lang::Fra => "fr",
        Lang::Heb => "he",
        Lang::Hin => "hi",
        Lang::Ita => "it",
        Lang::Jpn => "ja",
        Lang::Kor => "ko",
        Lang::Nld => "nl",
        Lang::Nob => "no",
        Lang::Pol => "pl",
        Lang::Por => "pt",
        Lang::Rus => "ru",
        Lang::Spa => "es",
        Lang::Swe => "sv",
        Lang::Tur => "tr",
        _ => DEFAULT_LANGUAGE,
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

    #[test]
    fn ensure_model_speaks_gates_on_the_model_language_set() {
        use ds_config::TtsModel;
        // Kokoro has no Russian; Qwen does. One shared decision for every synthesis path.
        assert!(ensure_model_speaks("ru", TtsModel::Kokoro).is_err());
        assert!(ensure_model_speaks("ru", TtsModel::Qwen).is_ok());
        assert!(ensure_model_speaks("en", TtsModel::Kokoro).is_ok());
        // OmniVoice auto-detects, so it accepts any detected code.
        assert!(ensure_model_speaks("ru", TtsModel::OmniVoice).is_ok());
        let message = ensure_model_speaks("ru", TtsModel::Kokoro).unwrap_err();
        assert!(message.contains("`ru`") && message.contains("kokoro"));
    }

    #[test]
    fn detect_supported_language_combines_detection_and_the_gate() {
        use ds_config::TtsModel;
        // Russian text detects as `ru`, which Qwen accepts but Kokoro refuses.
        let russian = "Этот ответ написан на русском языке.";
        assert_eq!(
            detect_supported_language(russian, TtsModel::Qwen).unwrap(),
            "ru"
        );
        assert!(detect_supported_language(russian, TtsModel::Kokoro).is_err());
    }
}
