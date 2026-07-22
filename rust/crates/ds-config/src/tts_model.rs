//! Built-in TTS model registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

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

    /// `ALL`'s wire tokens, in the same order, for const contexts that cannot call
    /// [`TtsModel::as_str`] (MCP schema enums). Pinned to the descriptors by
    /// `registry_order_matches_enum_discriminants`.
    pub const TOKENS: &'static [&'static str] = &["kokoro", "chatterbox", "qwen", "omnivoice"];

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

/// Value space of one declared TTS parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TtsParamKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Choice(&'static [&'static str]),
}

/// Static-constructible default for the `pub static` registry (an owned
/// [`TtsParamValue::Choice`] String cannot live in a const initializer).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TtsParamDefault {
    Float(f32),
    Int(i64),
    Choice(&'static str),
}

impl TtsParamDefault {
    pub fn value(self) -> TtsParamValue {
        match self {
            Self::Float(value) => TtsParamValue::Float(value),
            Self::Int(value) => TtsParamValue::Int(value),
            Self::Choice(value) => TtsParamValue::Choice(value.to_string()),
        }
    }
}

/// Owned runtime/config parameter value. Untagged wire form: a JSON/TOML integer is
/// `Int`, any other number `Float`, a string `Choice` — [`TtsModelDescriptor::validate_param`]
/// coerces an integral number where the declared kind is `Float`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TtsParamValue {
    Int(i64),
    Float(f32),
    Choice(String),
}

impl std::fmt::Display for TtsParamValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(value) => write!(f, "{value}"),
            Self::Float(value) => write!(f, "{value}"),
            Self::Choice(value) => write!(f, "{value}"),
        }
    }
}

/// One declared inference knob. `honored_ort`/`honored_mlx` record which backend
/// actually consumes the key — the Swift shim keeps a hand-maintained mirror of this
/// registry (`apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift`); update both
/// together with the `mlx_params` drift test in ds-tts.
#[derive(Debug)]
pub struct TtsParamDescriptor {
    pub key: &'static str,
    pub kind: TtsParamKind,
    pub default: TtsParamDefault,
    pub user_visible: bool,
    pub honored_ort: bool,
    pub honored_mlx: bool,
}

/// Sparse stored/wire form: overrides only, keyed by descriptor key.
pub type TtsParamMap = BTreeMap<String, TtsParamValue>;

/// Complete validated params for one model: every declared key present
/// ([`TtsModelDescriptor::resolve_params`] fills defaults and drops invalid entries).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResolvedTtsParams(TtsParamMap);

impl ResolvedTtsParams {
    pub fn get(&self, key: &str) -> Option<&TtsParamValue> {
        self.0.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &TtsParamValue)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// Declared float param, falling back to the registry default. An undeclared key
    /// is a programmer error (debug assert), not a runtime failure.
    pub fn float(&self, model: TtsModel, key: &str) -> f32 {
        match self.get(key) {
            Some(TtsParamValue::Float(value)) => *value,
            Some(TtsParamValue::Int(value)) => *value as f32,
            _ => match model.descriptor().param(key).map(|p| p.default) {
                Some(TtsParamDefault::Float(value)) => value,
                Some(TtsParamDefault::Int(value)) => value as f32,
                _ => {
                    debug_assert!(false, "{key} is not a declared {} float param", model.as_str());
                    0.0
                }
            },
        }
    }

    /// Declared int param, falling back to the registry default (see [`Self::float`]).
    pub fn int(&self, model: TtsModel, key: &str) -> i64 {
        match self.get(key) {
            Some(TtsParamValue::Int(value)) => *value,
            _ => match model.descriptor().param(key).map(|p| p.default) {
                Some(TtsParamDefault::Int(value)) => value,
                _ => {
                    debug_assert!(false, "{key} is not a declared {} int param", model.as_str());
                    0
                }
            },
        }
    }
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
    /// Declared inference knobs; absent config entries resolve to each default, so an
    /// empty `[tts_params]` block is byte-identical to pre-parameter behavior.
    pub params: &'static [TtsParamDescriptor],
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

    pub fn param(&self, key: &str) -> Option<&'static TtsParamDescriptor> {
        self.params.iter().find(|param| param.key == key)
    }

    /// Strict validation (MCP `set_config`): unknown key, wrong type, or out-of-range
    /// value is an `Err`. Returns the value normalized to the declared kind (an
    /// integral number is coerced for a `Float` param).
    pub fn validate_param(&self, key: &str, raw: &TtsParamValue) -> Result<TtsParamValue, String> {
        let Some(param) = self.param(key) else {
            return Err(format!("`{key}` is not a {} parameter", self.id));
        };
        match param.kind {
            TtsParamKind::Float { min, max } => {
                let value = match raw {
                    TtsParamValue::Float(value) => *value,
                    TtsParamValue::Int(value) => *value as f32,
                    TtsParamValue::Choice(_) => {
                        return Err(format!("`{key}` must be a number from {min} to {max}"));
                    }
                };
                if !value.is_finite() || value < min || value > max {
                    return Err(format!("`{key}` must be a number from {min} to {max}"));
                }
                Ok(TtsParamValue::Float(value))
            }
            TtsParamKind::Int { min, max } => {
                let value = match raw {
                    TtsParamValue::Int(value) => *value,
                    // Accept a lossless integral float (TOML/JSON clients may send 32.0).
                    TtsParamValue::Float(value) if value.fract() == 0.0 => *value as i64,
                    _ => {
                        return Err(format!("`{key}` must be an integer from {min} to {max}"));
                    }
                };
                if value < min || value > max {
                    return Err(format!("`{key}` must be an integer from {min} to {max}"));
                }
                Ok(TtsParamValue::Int(value))
            }
            TtsParamKind::Choice(choices) => match raw {
                TtsParamValue::Choice(value) if choices.contains(&value.as_str()) => {
                    Ok(raw.clone())
                }
                _ => Err(format!("`{key}` must be one of: {}", choices.join(", "))),
            },
        }
    }

    /// Fail-open resolution (helper playback + config load): every declared key comes
    /// back — a stored value that validates is kept, anything else (unknown key after a
    /// model switch, out-of-range, wrong type) falls to the declared default. Never
    /// refuses an utterance; mirrors the voice/language model-switch clamps.
    pub fn resolve_params(&self, stored: &TtsParamMap) -> ResolvedTtsParams {
        let mut resolved = TtsParamMap::new();
        for param in self.params {
            let value = stored
                .get(param.key)
                .and_then(|raw| self.validate_param(param.key, raw).ok())
                .unwrap_or_else(|| param.default.value());
            resolved.insert(param.key.to_string(), value);
        }
        ResolvedTtsParams(resolved)
    }

    /// Language token expected by the model implementation. The seam every backend
    /// (ORT and MLX) routes through — never a synth-local map.
    pub fn runtime_language<'a>(&self, language: &'a str) -> &'a str {
        match self.model {
            TtsModel::Qwen => match language {
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
            },
            // OmniVoice's prompt takes upstream lang_map tokens (mostly ISO 639-1, with
            // 639-3 where upstream has no two-letter entry): the detector's `ar`/`no`
            // become upstream `arb` (Standard Arabic) / `nb` (Bokmål), and the `auto`
            // sentinel (or nothing) prompts English.
            TtsModel::OmniVoice => match language {
                "" | "auto" => "en",
                "ar" => "arb",
                "no" => "nb",
                other => other,
            },
            TtsModel::Kokoro | TtsModel::Chatterbox => language,
        }
    }
}

// Japanese and Mandarin are dropped: their frontends cost ~3.6 MiB in every binary and
// a 27 MiB dictionary download for a pipeline eSpeak cannot stand in for.
const KOKORO_LANGUAGES: &[&str] = &["en", "es", "fr", "hi", "it", "pt"];
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
// Short speakable ids; ds-tts owns the id -> style-instruct table (OMNIVOICE_PRESETS)
// and a drift guard pins that table to this list exactly.
const OMNIVOICE_VOICES: &[&str] = &[
    "default",
    "young_woman",
    "young_man",
    "mature_woman",
    "mature_man",
    "british_woman",
    "british_man",
    "bright_woman",
    "deep_man",
    "whisper",
];
const OMNIVOICE_DEFAULT_VOICE: &[&str] = &["young_woman"];
const NO_PARAMS: &[TtsParamDescriptor] = &[];
// Emotion exaggeration fed to the pinned export's `exaggeration` input each embed step
// (model card range; 0.5 = neutral — the value the ORT port hardcoded before params).
const CHATTERBOX_PARAMS: &[TtsParamDescriptor] = &[TtsParamDescriptor {
    key: "exaggeration",
    kind: TtsParamKind::Float { min: 0.25, max: 2.0 },
    default: TtsParamDefault::Float(0.5),
    user_visible: true,
    honored_ort: true,
    honored_mlx: false,
}];
// Greedy-decode repetition penalty (reference generation_config default 1.05).
const QWEN_PARAMS: &[TtsParamDescriptor] = &[TtsParamDescriptor {
    key: "repetition_penalty",
    kind: TtsParamKind::Float { min: 1.0, max: 3.0 },
    default: TtsParamDefault::Float(1.05),
    user_visible: true,
    honored_ort: true,
    honored_mlx: false,
}];
const OMNIVOICE_PARAMS: &[TtsParamDescriptor] = &[
    // Iterative-unmasking step count. Upstream defaults to 32; 16 halves the
    // `2 * steps` LLM forwards per piece with per-codebook code diversity measured
    // unchanged (50-70 band held at both settings — see the decode-rewrite commit body).
    TtsParamDescriptor {
        key: "steps",
        kind: TtsParamKind::Int { min: 1, max: 64 },
        default: TtsParamDefault::Int(16),
        user_visible: true,
        honored_ort: true,
        honored_mlx: false,
    },
    // Gumbel position-noise seed OVERRIDE: >= 0 replaces the derived seed for every
    // piece of the utterance; the -1 default keeps the per-request FNV-1a derivation
    // over (runtime_lang, instruct, piece text) — reproducible either way, never
    // entropy (ds-tts omnivoice::stable_seed).
    TtsParamDescriptor {
        key: "seed",
        kind: TtsParamKind::Int {
            min: -1,
            max: i64::MAX,
        },
        default: TtsParamDefault::Int(-1),
        user_visible: false,
        honored_ort: true,
        honored_mlx: false,
    },
];
const MLX_CUDA_CPU_PROVIDERS: &[Provider] = &[Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu];
// One pinned OmniVoice ONNX profile for every ORT provider: FP16 audio sub-models plus
// the single fp32 bidirectional LLM backbone (no per-provider variants).
const OMNIVOICE_PROVIDERS: &[Provider] = MLX_CUDA_CPU_PROVIDERS;

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
        params: NO_PARAMS,
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
        params: CHATTERBOX_PARAMS,
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
        params: QWEN_PARAMS,
    },
    TtsModelDescriptor {
        model: TtsModel::OmniVoice,
        id: "omnivoice",
        display_name: "OmniVoice",
        default_language: "auto",
        languages: OMNIVOICE_LANGUAGES,
        model_languages: OMNIVOICE_LANGUAGES,
        voices: OMNIVOICE_VOICES,
        default_voices: OMNIVOICE_DEFAULT_VOICE,
        warmup_voice: "young_woman",
        providers: OMNIVOICE_PROVIDERS,
        frontend: TtsFrontend::PlainText,
        supports_rate: false,
        supports_full_duplex: false,
        supports_resume: true,
        params: OMNIVOICE_PARAMS,
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
            // TOKENS is hand-written for const contexts; it must stay the descriptors'
            // ids in ALL's order, or MCP schemas advertise a model that cannot parse.
            assert_eq!(TtsModel::TOKENS[index], model.as_str());
        }
        assert_eq!(TtsModel::TOKENS.len(), TtsModel::ALL.len());
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
        // OmniVoice prompts upstream lang_map tokens: auto/empty → en, the two codes
        // upstream has no two-letter entry for remap, the rest pass through.
        let omnivoice = TtsModel::OmniVoice.descriptor();
        assert_eq!(omnivoice.runtime_language("auto"), "en");
        assert_eq!(omnivoice.runtime_language(""), "en");
        assert_eq!(omnivoice.runtime_language("ar"), "arb");
        assert_eq!(omnivoice.runtime_language("no"), "nb");
        assert_eq!(omnivoice.runtime_language("ru"), "ru");
        assert_eq!(omnivoice.runtime_language("zh"), "zh");
        let kokoro = TtsModel::Kokoro.descriptor();
        assert_eq!(kokoro.languages, KOKORO_LANGUAGES);
        assert_eq!(
            kokoro.model_languages,
            &["en", "es", "fr", "hi", "it", "ja", "pt", "zh"]
        );
        assert_eq!(kokoro.supported_language_count(), 8);
        // Published upstream, not routed here — the JA/ZH frontends were dropped.
        for dropped in ["ja", "zh"] {
            assert!(!kokoro.supports_language(dropped));
            assert!(!kokoro.accepts_detected_language(dropped));
            assert!(kokoro.model_languages.contains(&dropped));
        }
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
    fn default_language_is_a_supported_code() {
        // The warm-helper clamp falls an unsupported code back to `default_language`, so it
        // must itself be supported. OmniVoice's default is the `auto` sentinel the clamp
        // never returns (it accepts every code), so it is exempt.
        for model in TtsModel::ALL.iter().copied() {
            if model == TtsModel::OmniVoice {
                continue;
            }
            let descriptor = model.descriptor();
            assert!(
                descriptor.supports_language(descriptor.default_language),
                "{} default language {} is not in its supported set",
                descriptor.id,
                descriptor.default_language
            );
        }
    }

    #[test]
    fn declared_params_pin_their_defaults_as_literals() {
        // The literals ARE the pre-parameter hardcoded behavior; ds-tts pins the same
        // values against its backend consts until S3/S6 turn those consts into
        // descriptor reads.
        assert!(TtsModel::Kokoro.descriptor().params.is_empty());
        let exaggeration = TtsModel::Chatterbox
            .descriptor()
            .param("exaggeration")
            .expect("chatterbox declares exaggeration");
        assert_eq!(exaggeration.default, TtsParamDefault::Float(0.5));
        assert_eq!(
            exaggeration.kind,
            TtsParamKind::Float {
                min: 0.25,
                max: 2.0
            }
        );
        assert!(exaggeration.user_visible && exaggeration.honored_ort);
        assert!(!exaggeration.honored_mlx);
        let penalty = TtsModel::Qwen
            .descriptor()
            .param("repetition_penalty")
            .expect("qwen declares repetition_penalty");
        assert_eq!(penalty.default, TtsParamDefault::Float(1.05));
        assert_eq!(penalty.kind, TtsParamKind::Float { min: 1.0, max: 3.0 });
        let steps = TtsModel::OmniVoice
            .descriptor()
            .param("steps")
            .expect("omnivoice declares steps");
        assert_eq!(steps.default, TtsParamDefault::Int(16));
        assert_eq!(steps.kind, TtsParamKind::Int { min: 1, max: 64 });
        // Seed default -1 = "derive per request" — MUST stay the default, or every
        // utterance of an agent would share one noise stream.
        let seed = TtsModel::OmniVoice
            .descriptor()
            .param("seed")
            .expect("omnivoice declares seed");
        assert_eq!(seed.default, TtsParamDefault::Int(-1));
        assert!(!seed.user_visible);
        assert!(
            matches!(seed.kind, TtsParamKind::Int { min: -1, max } if max == i64::MAX),
            "seed must accept the full non-negative u32/u64-style range plus the -1 sentinel"
        );
        // Every declared default must itself validate (a default outside its own range
        // would make resolve_params produce out-of-contract values).
        for model in TtsModel::ALL.iter().copied() {
            for param in model.descriptor().params {
                assert_eq!(
                    model
                        .descriptor()
                        .validate_param(param.key, &param.default.value()),
                    Ok(param.default.value()),
                    "{} {} default fails its own validation",
                    model.as_str(),
                    param.key
                );
            }
        }
    }

    #[test]
    fn validate_param_rejects_unknown_type_and_range_errors() {
        let chatterbox = TtsModel::Chatterbox.descriptor();
        let err = chatterbox
            .validate_param("bogus", &TtsParamValue::Float(1.0))
            .unwrap_err();
        assert!(err.contains("not a chatterbox parameter"), "{err}");
        let err = chatterbox
            .validate_param("exaggeration", &TtsParamValue::Float(2.5))
            .unwrap_err();
        assert!(err.contains("0.25 to 2"), "{err}");
        assert!(
            chatterbox
                .validate_param("exaggeration", &TtsParamValue::Choice("high".into()))
                .is_err()
        );
        assert!(
            chatterbox
                .validate_param("exaggeration", &TtsParamValue::Float(f32::NAN))
                .is_err()
        );
        // Integral numbers coerce to the declared kind, lossy floats don't.
        assert_eq!(
            chatterbox.validate_param("exaggeration", &TtsParamValue::Int(1)),
            Ok(TtsParamValue::Float(1.0))
        );
        let omnivoice = TtsModel::OmniVoice.descriptor();
        assert_eq!(
            omnivoice.validate_param("steps", &TtsParamValue::Float(32.0)),
            Ok(TtsParamValue::Int(32))
        );
        assert!(
            omnivoice
                .validate_param("steps", &TtsParamValue::Float(31.5))
                .is_err()
        );
        assert!(
            omnivoice
                .validate_param("steps", &TtsParamValue::Int(0))
                .is_err()
        );
        assert!(
            omnivoice
                .validate_param("steps", &TtsParamValue::Int(65))
                .is_err()
        );
    }

    #[test]
    fn resolve_params_fills_defaults_and_falls_invalid_entries_to_defaults() {
        let chatterbox = TtsModel::Chatterbox.descriptor();
        // Absent config ⇒ all defaults (byte-identical pre-parameter behavior).
        let resolved = chatterbox.resolve_params(&TtsParamMap::new());
        assert_eq!(
            resolved.float(TtsModel::Chatterbox, "exaggeration"),
            0.5
        );
        assert_eq!(resolved.iter().count(), chatterbox.params.len());
        // A valid override is kept.
        let mut stored = TtsParamMap::new();
        stored.insert("exaggeration".into(), TtsParamValue::Float(1.5));
        assert_eq!(
            chatterbox
                .resolve_params(&stored)
                .float(TtsModel::Chatterbox, "exaggeration"),
            1.5
        );
        // Out-of-range and stale (other-model) keys fall to defaults — never refused.
        let mut stale = TtsParamMap::new();
        stale.insert("exaggeration".into(), TtsParamValue::Float(9.0));
        stale.insert("steps".into(), TtsParamValue::Int(8));
        let resolved = chatterbox.resolve_params(&stale);
        assert_eq!(resolved.float(TtsModel::Chatterbox, "exaggeration"), 0.5);
        assert!(resolved.get("steps").is_none(), "stale keys must drop");
        // Wire form: the untagged value round-trips as bare JSON scalars.
        let json = serde_json::to_string(&resolved).unwrap();
        assert_eq!(json, r#"{"exaggeration":0.5}"#);
        let back: TtsParamMap = serde_json::from_str(r#"{"steps":24,"x":1.5,"v":"a"}"#).unwrap();
        assert_eq!(back["steps"], TtsParamValue::Int(24));
        assert_eq!(back["x"], TtsParamValue::Float(1.5));
        assert_eq!(back["v"], TtsParamValue::Choice("a".into()));
    }

    #[test]
    fn providers_are_filtered_by_model() {
        for model in TtsModel::ALL {
            assert!(model.descriptor().supports_provider(Provider::Mlx));
            assert!(model.descriptor().supports_provider(Provider::OrtCpu));
            assert!(model.descriptor().supports_provider(Provider::OrtCuda));
        }
        assert!(
            TtsModel::Kokoro
                .descriptor()
                .supports_provider(Provider::OrtCoreMl)
        );
        assert!(TtsModel::Chatterbox.descriptor().wants_cuda("auto"));
        assert!(TtsModel::Qwen.descriptor().wants_cuda("cuda"));
        // Declaring the CUDA profile is not the same as fetching it: whether the CUDA-only
        // assets are downloaded is `ds_model::tts_wants_cuda_assets`'s effective-provider call.
        assert!(TtsModel::OmniVoice.descriptor().wants_cuda("auto"));
        assert!(TtsModel::OmniVoice.descriptor().wants_cuda("cuda"));
        assert!(!TtsModel::OmniVoice.descriptor().wants_cuda("cpu"));
    }
}
