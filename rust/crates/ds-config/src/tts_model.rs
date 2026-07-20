//! Built-in TTS model registry.

use serde::{Deserialize, Deserializer};

use crate::Provider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TtsModel {
    #[default]
    Kokoro,
    Chatterbox,
    Qwen,
    OmniVoice,
}

impl TtsModel {
    pub const ALL: &'static [Self] = &[Self::Kokoro, Self::Chatterbox, Self::Qwen, Self::OmniVoice];

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "kokoro" => Some(Self::Kokoro),
            "chatterbox" => Some(Self::Chatterbox),
            "qwen" => Some(Self::Qwen),
            "omnivoice" => Some(Self::OmniVoice),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.descriptor().id
    }

    pub fn descriptor(self) -> &'static TtsModelDescriptor {
        &TTS_MODELS[self as usize]
    }
}

serialize_as_str!(TtsModel);
strict_de!(TtsModel, "kokoro|chatterbox|qwen|omnivoice");

pub(crate) fn de_tts_model<'de, D>(deserializer: D) -> Result<TtsModel, D::Error>
where
    D: Deserializer<'de>,
{
    let value = toml::Value::deserialize(deserializer).unwrap_or(toml::Value::Boolean(false));
    Ok(value.as_str().and_then(TtsModel::parse).unwrap_or_default())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsFrontend {
    KokoroPhonemes,
    PlainText,
}

/// Static behavior shared by config, downloads, helpers, status, and tools.
#[derive(Debug)]
pub struct TtsModelDescriptor {
    pub model: TtsModel,
    pub id: &'static str,
    pub display_name: &'static str,
    pub default_language: &'static str,
    /// Languages the DontSpeak frontend can currently send to this model.
    pub languages: &'static [&'static str],
    /// Published model coverage shown in the Libraries catalog.
    pub model_languages: &'static [&'static str],
    pub voices: &'static [&'static str],
    /// Out-of-box pool for this model. `voices` may expose additional choices.
    pub default_voices: &'static [&'static str],
    /// Provider-neutral voice used by the shared discarded inference before READY.
    pub warmup_voice: &'static str,
    pub providers: &'static [Provider],
    pub frontend: TtsFrontend,
    pub supports_rate: bool,
    pub supports_full_duplex: bool,
    pub supports_resume: bool,
}

impl TtsModelDescriptor {
    pub fn supports_provider(&self, provider: Provider) -> bool {
        self.providers.contains(&provider)
    }

    /// Whether this model's pinned assets and the runtime preference both select CUDA.
    pub fn wants_cuda(&self, preference: &str) -> bool {
        self.supports_provider(Provider::OrtCuda) && crate::provider_pref_wants_gpu(preference)
    }

    pub fn supports_language(&self, language: &str) -> bool {
        self.languages.contains(&language)
    }

    /// User-facing coverage count. OmniVoice selects a language internally, so its runtime
    /// sentinel is `auto` while the pinned model covers the full upstream language catalog.
    pub fn supported_language_count(&self) -> usize {
        match self.model {
            TtsModel::OmniVoice => 646,
            _ => self.model_languages.len(),
        }
    }

    pub fn detects_language_automatically(&self) -> bool {
        self.model == TtsModel::OmniVoice
    }

    /// Pinned upstream language/voice list when the catalog benefits from a primary source.
    pub fn language_list_url(&self) -> Option<&'static str> {
        match self.model {
            TtsModel::Kokoro => Some(
                "https://huggingface.co/hexgrad/Kokoro-82M/blob/c3327e9bac3dbe55779397bfa82de0f8806fb3bc/VOICES.md",
            ),
            TtsModel::OmniVoice => Some(
                "https://github.com/k2-fsa/OmniVoice/blob/468e927ba3716cd8dd86421148dfb3046e9f9d7b/docs/languages.md",
            ),
            TtsModel::Chatterbox | TtsModel::Qwen => None,
        }
    }

    /// Whether an automatically detected ISO language can be sent to this model.
    pub fn accepts_detected_language(&self, language: &str) -> bool {
        self.model == TtsModel::OmniVoice || self.supports_language(language)
    }

    /// Language token expected by the model implementation.
    pub fn runtime_language<'a>(&self, language: &'a str) -> &'a str {
        if self.model != TtsModel::Qwen {
            return language;
        }
        match language {
            "zh" => "chinese",
            "en" => "english",
            "ja" => "japanese",
            "ko" => "korean",
            "de" => "german",
            "fr" => "french",
            "ru" => "russian",
            "pt" => "portuguese",
            "es" => "spanish",
            "it" => "italian",
            other => other,
        }
    }
}

const KOKORO_LANGUAGES: &[&str] = &["en", "es", "fr", "hi", "it", "ja", "pt", "zh"];
// Kokoro v1.0 publishes eight languages. American and British English are separate
// voice families but one language in the upstream release count.
const KOKORO_MODEL_LANGUAGES: &[&str] = &["en", "es", "fr", "hi", "it", "ja", "pt", "zh"];
const CHATTERBOX_LANGUAGES: &[&str] = &[
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "it", "ja", "ko", "ms", "nl", "no",
    "pl", "pt", "ru", "sv", "sw", "tr", "zh",
];
const QWEN_LANGUAGES: &[&str] = &["zh", "en", "ja", "ko", "de", "fr", "ru", "pt", "es", "it"];
const OMNIVOICE_LANGUAGES: &[&str] = &["auto"];
const KOKORO_VOICES: &[&str] = &["af_sarah", "bf_emma"];
const QWEN_VOICES: &[&str] = &[
    "serena", "vivian", "uncle_fu", "ryan", "aiden", "ono_anna", "sohee", "eric", "dylan",
];
const DEFAULT_VOICE: &[&str] = &["default"];
const QWEN_DEFAULT_VOICE: &[&str] = &["sohee"];
const OMNIVOICE_VOICES: &[&str] = &["warm, clear female voice"];
const MLX_CUDA_CPU_PROVIDERS: &[Provider] = &[Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu];
// The portable OmniVoice profile uses FP16 audio sub-models and an INT4 LLM because
// ONNX Runtime GenAI has no FP16 CPU LLM profile. The all-FP16 export is CUDA-only.
const OMNIVOICE_PROVIDERS: &[Provider] = &[Provider::Mlx, Provider::OrtCpu];

pub static TTS_MODELS: [TtsModelDescriptor; 4] = [
    TtsModelDescriptor {
        model: TtsModel::Kokoro,
        id: "kokoro",
        display_name: "Kokoro",
        default_language: "en",
        languages: KOKORO_LANGUAGES,
        model_languages: KOKORO_MODEL_LANGUAGES,
        voices: KOKORO_VOICES,
        default_voices: KOKORO_VOICES,
        warmup_voice: "af_heart",
        providers: &[
            Provider::Mlx,
            Provider::OrtCuda,
            Provider::OrtCoreMl,
            Provider::OrtCpu,
        ],
        frontend: TtsFrontend::KokoroPhonemes,
        supports_rate: true,
        supports_full_duplex: true,
        supports_resume: true,
    },
    TtsModelDescriptor {
        model: TtsModel::Chatterbox,
        id: "chatterbox",
        display_name: "Chatterbox Multilingual",
        default_language: "en",
        languages: CHATTERBOX_LANGUAGES,
        model_languages: CHATTERBOX_LANGUAGES,
        voices: DEFAULT_VOICE,
        default_voices: DEFAULT_VOICE,
        warmup_voice: "default",
        providers: MLX_CUDA_CPU_PROVIDERS,
        frontend: TtsFrontend::PlainText,
        supports_rate: false,
        supports_full_duplex: false,
        supports_resume: true,
    },
    TtsModelDescriptor {
        model: TtsModel::Qwen,
        id: "qwen",
        display_name: "Qwen3-TTS",
        default_language: "en",
        languages: QWEN_LANGUAGES,
        model_languages: QWEN_LANGUAGES,
        voices: QWEN_VOICES,
        default_voices: QWEN_DEFAULT_VOICE,
        warmup_voice: "ryan",
        providers: MLX_CUDA_CPU_PROVIDERS,
        frontend: TtsFrontend::PlainText,
        supports_rate: false,
        supports_full_duplex: false,
        supports_resume: true,
    },
    TtsModelDescriptor {
        model: TtsModel::OmniVoice,
        id: "omnivoice",
        display_name: "OmniVoice",
        default_language: "auto",
        languages: OMNIVOICE_LANGUAGES,
        model_languages: OMNIVOICE_LANGUAGES,
        voices: OMNIVOICE_VOICES,
        default_voices: OMNIVOICE_VOICES,
        warmup_voice: "default",
        providers: OMNIVOICE_PROVIDERS,
        frontend: TtsFrontend::PlainText,
        supports_rate: false,
        supports_full_duplex: false,
        supports_resume: true,
    },
];

pub fn tts_model_descriptor(id: &str) -> Option<&'static TtsModelDescriptor> {
    TtsModel::parse(id).map(TtsModel::descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_order_matches_enum_discriminants() {
        for (index, model) in TtsModel::ALL.iter().copied().enumerate() {
            assert_eq!(model as usize, index);
            assert_eq!(model.descriptor().model, model);
            assert_eq!(TtsModel::parse(model.as_str()), Some(model));
        }
        // parse trims + lowercases like every other config-token enum; unknown stays None.
        assert_eq!(TtsModel::parse("Qwen"), Some(TtsModel::Qwen));
        assert_eq!(TtsModel::parse(" KOKORO "), Some(TtsModel::Kokoro));
        assert_eq!(TtsModel::parse("bogus"), None);
    }

    #[test]
    fn language_contract_is_model_specific() {
        assert!(TtsModel::Chatterbox.descriptor().supports_language("ru"));
        assert!(!TtsModel::Chatterbox.descriptor().supports_language("cs"));
        assert!(TtsModel::OmniVoice.descriptor().supports_language("auto"));
        assert!(!TtsModel::OmniVoice.descriptor().supports_language("cs"));
        assert!(
            TtsModel::OmniVoice
                .descriptor()
                .accepts_detected_language("cs")
        );
        assert!(
            !TtsModel::Kokoro
                .descriptor()
                .accepts_detected_language("ru")
        );
        assert!(!TtsModel::Qwen.descriptor().supports_language("EN"));
        assert_eq!(
            TtsModel::Qwen.descriptor().runtime_language("ja"),
            "japanese"
        );
        assert_eq!(
            TtsModel::Chatterbox.descriptor().runtime_language("ja"),
            "ja"
        );
        let kokoro = TtsModel::Kokoro.descriptor();
        assert_eq!(kokoro.languages, KOKORO_MODEL_LANGUAGES);
        assert_eq!(
            kokoro.model_languages,
            &["en", "es", "fr", "hi", "it", "ja", "pt", "zh"]
        );
        assert_eq!(kokoro.supported_language_count(), 8);
        assert!(
            kokoro
                .language_list_url()
                .is_some_and(|url| url.contains("hexgrad/Kokoro-82M"))
        );
        for descriptor in &TTS_MODELS {
            for language in descriptor.languages {
                assert!(
                    descriptor.model_languages.contains(language),
                    "{} runtime language {language} is absent from published coverage",
                    descriptor.id
                );
            }
        }
        assert_eq!(
            TtsModel::Chatterbox.descriptor().supported_language_count(),
            CHATTERBOX_LANGUAGES.len()
        );
        assert_eq!(
            TtsModel::OmniVoice.descriptor().supported_language_count(),
            646
        );
        assert!(
            TtsModel::OmniVoice
                .descriptor()
                .detects_language_automatically()
        );
        assert!(
            TtsModel::OmniVoice
                .descriptor()
                .language_list_url()
                .is_some_and(|url| url.contains("k2-fsa/OmniVoice"))
        );
        assert!(TtsModel::Qwen.descriptor().language_list_url().is_none());
    }

    #[test]
    fn providers_are_filtered_by_model() {
        for model in TtsModel::ALL {
            assert!(model.descriptor().supports_provider(Provider::Mlx));
            assert!(model.descriptor().supports_provider(Provider::OrtCpu));
        }
        for model in [TtsModel::Kokoro, TtsModel::Chatterbox, TtsModel::Qwen] {
            assert!(model.descriptor().supports_provider(Provider::OrtCuda));
        }
        assert!(
            !TtsModel::OmniVoice
                .descriptor()
                .supports_provider(Provider::OrtCuda)
        );
        assert!(TtsModel::Chatterbox.descriptor().wants_cuda("auto"));
        assert!(TtsModel::Qwen.descriptor().wants_cuda("cuda"));
        assert!(!TtsModel::OmniVoice.descriptor().wants_cuda("auto"));
        assert!(!TtsModel::OmniVoice.descriptor().wants_cuda("cuda"));
    }
}
