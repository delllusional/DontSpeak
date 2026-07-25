//! Built-in TTS model registry.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{Provider, TtsEngine};

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

    /// Wire tokens in `ALL` order for const contexts (MCP schemas). Pinned by
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

/// Built-in STT asset token (not a [`TtsModel`]). STT half of [`MODEL_ASSET_TOKENS`].
pub const STT_MODEL_TOKEN: &str = "parakeet";

/// On-disk model assets (model half of [`REMOVABLE_ASSET_TOKENS`]). Hand-written for const
/// MCP schemas; composition pinned by tests.
pub const MODEL_ASSET_TOKENS: &[&str] = &["kokoro", "chatterbox", "qwen", "omnivoice", "parakeet"];

pub const KOKORO_FRONTEND_ASSET_TOKEN: &str = "kokoro_frontend";
pub const ONNXRUNTIME_ASSET_TOKEN: &str = "onnxruntime";
pub const CUDA_ASSET_TOKEN: &str = "cuda";

/// Shared assets, inventory order. Reclaimable only while unreferenced (#220).
pub const SHARED_ASSET_TOKENS: &[&str] = &[
    KOKORO_FRONTEND_ASSET_TOKEN,
    ONNXRUNTIME_ASSET_TOKEN,
    CUDA_ASSET_TOKEN,
];

/// `models remove` ids: models then shared. Composition pinned by tests.
pub const REMOVABLE_ASSET_TOKENS: &[&str] = &[
    "kokoro",
    "chatterbox",
    "qwen",
    "omnivoice",
    "parakeet",
    "kokoro_frontend",
    "onnxruntime",
    "cuda",
];

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TtsParamKind {
    Float { min: f32, max: f32 },
    Int { min: i64, max: i64 },
    Choice(&'static [&'static str]),
}

/// Const-friendly registry default; runtime choices are owned strings.
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

/// Untagged wire value; validation coerces integral numbers for float parameters.
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

/// Param registry entry. Swift mirrors this; update both sides + drift tests together.
#[derive(Debug)]
pub struct TtsParamDescriptor {
    pub key: &'static str,
    pub kind: TtsParamKind,
    pub default: TtsParamDefault,
    pub user_visible: bool,
    pub honored_ort: bool,
    pub honored_mlx: bool,
}

pub type TtsParamMap = BTreeMap<String, TtsParamValue>;

/// Overrides for one `speak` target. Parameter keys share the persistent
/// `tts_params.<target>` descriptors; voice and language are utterance-only.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TtsTargetArgs {
    voice: Option<String>,
    language: Option<String>,
    params: TtsParamMap,
}

impl TtsTargetArgs {
    pub fn voice(&self) -> Option<&str> {
        self.voice.as_deref()
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    pub fn params(&self) -> &TtsParamMap {
        &self.params
    }
}

/// Per-engine/model utterance overrides accepted by MCP `speak`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TtsArgPools {
    system: Option<TtsTargetArgs>,
    kokoro: Option<TtsTargetArgs>,
    chatterbox: Option<TtsTargetArgs>,
    qwen: Option<TtsTargetArgs>,
    omnivoice: Option<TtsTargetArgs>,
}

impl TtsArgPools {
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let targets = value
            .as_object()
            .filter(|targets| !targets.is_empty())
            .ok_or_else(|| "tts_args must be a non-empty object".to_string())?;
        let mut pools = Self::default();
        for (target, raw) in targets {
            let model = (target != "system")
                .then(|| TtsModel::parse(target))
                .flatten();
            if target != "system" && model.is_none() {
                return Err(format!("unknown tts_args target `{target}`"));
            }
            let entries = raw
                .as_object()
                .filter(|entries| !entries.is_empty())
                .ok_or_else(|| format!("tts_args.{target} must be a non-empty object"))?;
            let mut args = TtsTargetArgs::default();
            for (key, raw) in entries {
                match key.as_str() {
                    "voice" => {
                        let voice = raw
                            .as_str()
                            .map(str::trim)
                            .filter(|voice| !voice.is_empty())
                            .ok_or_else(|| {
                                format!("tts_args.{target}.voice must be a non-empty string")
                            })?;
                        if let Some(model) = model
                            && model != TtsModel::Kokoro
                            && !model.descriptor().voices.contains(&voice)
                        {
                            return Err(format!(
                                "tts_args.{target}.voice `{voice}` is not supported"
                            ));
                        }
                        args.voice = Some(voice.to_string());
                    }
                    "language" => {
                        let language = raw
                            .as_str()
                            .map(str::trim)
                            .filter(|language| !language.is_empty())
                            .map(str::to_ascii_lowercase)
                            .ok_or_else(|| {
                                format!("tts_args.{target}.language must be a non-empty string")
                            })?;
                        if let Some(model) = model
                            && !model.descriptor().accepts_detected_language(&language)
                        {
                            return Err(format!(
                                "tts_args.{target}.language `{language}` is not supported"
                            ));
                        }
                        args.language = Some(language);
                    }
                    _ => {
                        let value: TtsParamValue =
                            serde_json::from_value(raw.clone()).map_err(|_| {
                                format!("tts_args.{target}.{key} has an unsupported value")
                            })?;
                        let value = match model {
                            Some(model) => model.descriptor().validate_param(key, &value),
                            None => validate_tts_param("system", SYSTEM_TTS_PARAMS, key, &value),
                        }
                        .map_err(|error| format!("tts_args.{target}: {error}"))?;
                        args.params.insert(key.clone(), value);
                    }
                }
            }
            *pools.target_mut(target, model) = Some(args);
        }
        Ok(pools)
    }

    pub fn for_target(&self, engine: TtsEngine, model: TtsModel) -> Option<&TtsTargetArgs> {
        match engine {
            TtsEngine::System => self.system.as_ref(),
            TtsEngine::BuiltIn => match model {
                TtsModel::Kokoro => self.kokoro.as_ref(),
                TtsModel::Chatterbox => self.chatterbox.as_ref(),
                TtsModel::Qwen => self.qwen.as_ref(),
                TtsModel::OmniVoice => self.omnivoice.as_ref(),
            },
        }
    }

    pub fn with_voice(engine: TtsEngine, model: TtsModel, voice: String) -> Self {
        let mut pools = Self::default();
        let target = if engine == TtsEngine::System {
            "system"
        } else {
            model.as_str()
        };
        *pools.target_mut(target, (engine == TtsEngine::BuiltIn).then_some(model)) =
            Some(TtsTargetArgs {
                voice: Some(voice),
                ..Default::default()
            });
        pools
    }

    fn target_mut(&mut self, target: &str, model: Option<TtsModel>) -> &mut Option<TtsTargetArgs> {
        if target == "system" {
            return &mut self.system;
        }
        match model.expect("validated model target") {
            TtsModel::Kokoro => &mut self.kokoro,
            TtsModel::Chatterbox => &mut self.chatterbox,
            TtsModel::Qwen => &mut self.qwen,
            TtsModel::OmniVoice => &mut self.omnivoice,
        }
    }
}

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

    /// Fall back to the registry default; undeclared keys are programmer errors.
    pub fn float(&self, model: TtsModel, key: &str) -> f32 {
        match self.get(key) {
            Some(TtsParamValue::Float(value)) => *value,
            Some(TtsParamValue::Int(value)) => *value as f32,
            _ => match model.descriptor().param(key).map(|p| p.default) {
                Some(TtsParamDefault::Float(value)) => value,
                Some(TtsParamDefault::Int(value)) => value as f32,
                _ => {
                    debug_assert!(
                        false,
                        "{key} is not a declared {} float param",
                        model.as_str()
                    );
                    0.0
                }
            },
        }
    }

    pub fn int(&self, model: TtsModel, key: &str) -> i64 {
        match self.get(key) {
            Some(TtsParamValue::Int(value)) => *value,
            _ => match model.descriptor().param(key).map(|p| p.default) {
                Some(TtsParamDefault::Int(value)) => value,
                _ => {
                    debug_assert!(
                        false,
                        "{key} is not a declared {} int param",
                        model.as_str()
                    );
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
    /// Inference settings; absent entries resolve to descriptor defaults.
    pub params: &'static [TtsParamDescriptor],
    /// Persisted settings. Kokoro adds transport-level `rate` outside the model ABI.
    pub config_params: &'static [TtsParamDescriptor],
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

    /// User-facing coverage count. OmniVoice selects a language internally, so it reports the
    /// full upstream language catalog rather than the DontSpeak frontend's routed subset.
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

    pub fn config_param(&self, key: &str) -> Option<&'static TtsParamDescriptor> {
        self.config_params.iter().find(|param| param.key == key)
    }

    /// Strict validation with normalization to the declared kind.
    pub fn validate_param(&self, key: &str, raw: &TtsParamValue) -> Result<TtsParamValue, String> {
        validate_tts_param(self.id, self.config_params, key, raw)
    }

    /// Resolve every inference key, replacing absent or invalid values with defaults.
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

    /// Shared ORT/MLX mapping to each model's prompt token.
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
            // Upstream uses `arb`/`nb`; empty prompt English.
            TtsModel::OmniVoice => match language {
                "" => "en",
                "ar" => "arb",
                "no" => "nb",
                other => other,
            },
            TtsModel::Kokoro | TtsModel::Chatterbox => language,
        }
    }
}

/// Strict validation shared by model and System setting pools.
pub fn validate_tts_param(
    owner: &str,
    params: &[TtsParamDescriptor],
    key: &str,
    raw: &TtsParamValue,
) -> Result<TtsParamValue, String> {
    let Some(param) = params.iter().find(|param| param.key == key) else {
        return Err(format!("`{key}` is not a {owner} parameter"));
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
            TtsParamValue::Choice(value) if choices.contains(&value.as_str()) => Ok(raw.clone()),
            _ => Err(format!("`{key}` must be one of: {}", choices.join(", "))),
        },
    }
}

// JA/ZH frontends dropped (~3.6 MiB + 27 MiB dict; eSpeak cannot stand in).
const KOKORO_LANGUAGES: &[&str] = &["en", "es", "fr", "hi", "it", "pt"];
// Upstream publishes eight languages (en-US/en-GB are one language count).
const KOKORO_MODEL_LANGUAGES: &[&str] = &["en", "es", "fr", "hi", "it", "ja", "pt", "zh"];
const CHATTERBOX_LANGUAGES: &[&str] = &[
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "it", "ja", "ko", "ms", "nl", "no",
    "pl", "pt", "ru", "sv", "sw", "tr", "zh",
];
const QWEN_LANGUAGES: &[&str] = &["zh", "en", "ja", "ko", "de", "fr", "ru", "pt", "es", "it"];
// OmniVoice selects language internally: empty range → `model_allowlist` yields the full table.
const OMNIVOICE_LANGUAGES: &[&str] = &[];
const KOKORO_VOICES: &[&str] = &["af_sarah", "bf_emma"];
const QWEN_VOICES: &[&str] = &[
    "serena", "vivian", "uncle_fu", "ryan", "aiden", "ono_anna", "sohee", "eric", "dylan",
];
const DEFAULT_VOICE: &[&str] = &["default"];
const QWEN_DEFAULT_VOICE: &[&str] = &["sohee"];
// ds-tts maps these ids to instructs and pins this order.
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
const RATE_PARAMS: &[TtsParamDescriptor] = &[TtsParamDescriptor {
    key: "rate",
    kind: TtsParamKind::Float { min: 0.5, max: 2.0 },
    default: TtsParamDefault::Float(1.0),
    user_visible: true,
    honored_ort: true,
    honored_mlx: true,
}];
pub const SYSTEM_TTS_PARAMS: &[TtsParamDescriptor] = RATE_PARAMS;
// Model-card range; 0.5 is neutral.
const CHATTERBOX_PARAMS: &[TtsParamDescriptor] = &[TtsParamDescriptor {
    key: "exaggeration",
    kind: TtsParamKind::Float {
        min: 0.25,
        max: 2.0,
    },
    default: TtsParamDefault::Float(0.5),
    user_visible: true,
    honored_ort: true,
    honored_mlx: true,
}];
// Greedy-decode repetition penalty (reference generation_config default 1.05).
const QWEN_PARAMS: &[TtsParamDescriptor] = &[TtsParamDescriptor {
    key: "repetition_penalty",
    kind: TtsParamKind::Float { min: 1.0, max: 3.0 },
    default: TtsParamDefault::Float(1.05),
    user_visible: true,
    honored_ort: true,
    honored_mlx: true,
}];
const OMNIVOICE_PARAMS: &[TtsParamDescriptor] = &[
    // 16 halves the `2 * steps` LLM forwards with unchanged measured code diversity.
    TtsParamDescriptor {
        key: "steps",
        kind: TtsParamKind::Int { min: 1, max: 64 },
        default: TtsParamDefault::Int(16),
        user_visible: true,
        honored_ort: true,
        honored_mlx: true,
    },
    // -1 derives a stable seed from language + voice; non-negative values override it.
    TtsParamDescriptor {
        key: "seed",
        kind: TtsParamKind::Int {
            min: -1,
            max: i64::MAX,
        },
        default: TtsParamDefault::Int(-1),
        user_visible: false,
        honored_ort: true,
        honored_mlx: true,
    },
];
const MLX_CUDA_CPU_PROVIDERS: &[Provider] = &[Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu];
// One ONNX profile serves every ORT provider.
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
            Provider::Fluid,
            Provider::OrtCuda,
            Provider::OrtCoreMl,
            Provider::OrtCpu,
        ],
        frontend: TtsFrontend::KokoroPhonemes,
        supports_rate: true,
        supports_full_duplex: true,
        supports_resume: true,
        params: NO_PARAMS,
        config_params: RATE_PARAMS,
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
        config_params: CHATTERBOX_PARAMS,
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
        config_params: QWEN_PARAMS,
    },
    TtsModelDescriptor {
        model: TtsModel::OmniVoice,
        id: "omnivoice",
        display_name: "OmniVoice",
        default_language: "en",
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
        config_params: OMNIVOICE_PARAMS,
    },
];

pub fn tts_model_descriptor(id: &str) -> Option<&'static TtsModelDescriptor> {
    TtsModel::parse(id).map(TtsModel::descriptor)
}

/// ISO 639-1 codes the detector can recognize: the sorted union of every model's
/// `model_languages`. Hand-written for const schema use; pinned by
/// `detectable_languages_are_the_sorted_union_of_model_coverage`. Equals the `en.yml`
/// `language.*` keys, so `preferred_languages` needs no new locale entry.
pub const DETECTABLE_LANGUAGES: &[&str] = &[
    "ar", "da", "de", "el", "en", "es", "fi", "fr", "he", "hi", "it", "ja", "ko", "ms", "nl", "no",
    "pl", "pt", "ru", "sv", "sw", "tr", "zh",
];

/// Normalize + gate a language code to [`DETECTABLE_LANGUAGES`]. `None` for unrecognized.
pub fn parse_language_code(s: &str) -> Option<String> {
    let code = s.trim().to_ascii_lowercase();
    DETECTABLE_LANGUAGES
        .contains(&code.as_str())
        .then_some(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_args_parse_target_specific_voice_language_and_params() {
        let pools = TtsArgPools::parse(&serde_json::json!({
            "system": { "voice": "Ava", "language": "RU", "rate": 1.25 },
            "kokoro": { "voice": "af_sarah", "language": "it", "rate": 0.8 },
            "chatterbox": { "language": "ru", "exaggeration": 1.5 },
            "qwen": { "voice": "ryan", "repetition_penalty": 1.2 },
            "omnivoice": { "language": "cs", "steps": 32, "seed": 7 }
        }))
        .unwrap();

        let system = pools
            .for_target(TtsEngine::System, TtsModel::Kokoro)
            .unwrap();
        assert_eq!(system.voice(), Some("Ava"));
        assert_eq!(system.language(), Some("ru"));
        assert_eq!(system.params()["rate"], TtsParamValue::Float(1.25));

        let kokoro = pools
            .for_target(TtsEngine::BuiltIn, TtsModel::Kokoro)
            .unwrap();
        assert_eq!(kokoro.voice(), Some("af_sarah"));
        assert_eq!(kokoro.language(), Some("it"));
        assert_eq!(kokoro.params()["rate"], TtsParamValue::Float(0.8));

        let omnivoice = pools
            .for_target(TtsEngine::BuiltIn, TtsModel::OmniVoice)
            .unwrap();
        assert_eq!(omnivoice.language(), Some("cs"));
        assert_eq!(omnivoice.params()["steps"], TtsParamValue::Int(32));
        assert_eq!(omnivoice.params()["seed"], TtsParamValue::Int(7));
    }

    #[test]
    fn speak_args_reject_unknown_or_cross_target_fields() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({ "wavenet": { "voice": "x" } }),
            serde_json::json!({ "kokoro": {} }),
            serde_json::json!({ "kokoro": { "language": "ru" } }),
            serde_json::json!({ "kokoro": { "exaggeration": 1.0 } }),
            serde_json::json!({ "qwen": { "rate": 1.2 } }),
            serde_json::json!({ "system": { "steps": 8 } }),
            serde_json::json!({ "system": { "voice": " " } }),
        ] {
            assert!(TtsArgPools::parse(&value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn speak_args_fixed_model_voices_match_their_descriptors() {
        for descriptor in TTS_MODELS
            .iter()
            .filter(|descriptor| descriptor.model != TtsModel::Kokoro)
        {
            for voice in descriptor.voices {
                let value = serde_json::json!({
                    (descriptor.id): { "voice": voice }
                });
                let pools = TtsArgPools::parse(&value).unwrap_or_else(|error| {
                    panic!("rejected {} voice {voice}: {error}", descriptor.id)
                });
                assert_eq!(
                    pools
                        .for_target(TtsEngine::BuiltIn, descriptor.model)
                        .and_then(TtsTargetArgs::voice),
                    Some(*voice)
                );
            }

            let value = serde_json::json!({
                (descriptor.id): { "voice": "not_a_registry_voice" }
            });
            let error = TtsArgPools::parse(&value).unwrap_err();
            assert!(
                error.contains("is not supported"),
                "{} accepted an unknown voice: {error}",
                descriptor.id
            );
        }
    }

    #[test]
    fn speak_args_kokoro_and_system_voices_remain_dynamic() {
        let pools = TtsArgPools::parse(&serde_json::json!({
            "kokoro": { "voice": "custom_pack_voice" },
            "system": { "voice": "Installed SAPI Voice" }
        }))
        .expect("dynamic voice names remain admissible");

        assert_eq!(
            pools
                .for_target(TtsEngine::BuiltIn, TtsModel::Kokoro)
                .and_then(TtsTargetArgs::voice),
            Some("custom_pack_voice")
        );
        assert_eq!(
            pools
                .for_target(TtsEngine::System, TtsModel::Kokoro)
                .and_then(TtsTargetArgs::voice),
            Some("Installed SAPI Voice")
        );
    }

    #[test]
    fn speak_arg_reserved_names_do_not_drift_into_model_params() {
        for descriptor in &TTS_MODELS {
            for param in descriptor.config_params {
                assert!(
                    !matches!(param.key, "voice" | "language"),
                    "{} parameter `{}` collides with a speak argument",
                    descriptor.id,
                    param.key
                );
            }
        }
        for param in SYSTEM_TTS_PARAMS {
            assert!(!matches!(param.key, "voice" | "language"));
        }
    }

    #[test]
    fn registry_order_matches_enum_discriminants() {
        for (index, model) in TtsModel::ALL.iter().copied().enumerate() {
            assert_eq!(model as usize, index);
            assert_eq!(model.descriptor().model, model);
            assert_eq!(TtsModel::parse(model.as_str()), Some(model));
            assert_eq!(TtsModel::TOKENS[index], model.as_str());
            assert_eq!(
                model.descriptor().supports_rate,
                model.descriptor().config_param("rate").is_some(),
                "{} rate capability drifted from its persisted settings",
                model.as_str()
            );
        }
        assert_eq!(TtsModel::TOKENS.len(), TtsModel::ALL.len());
        assert_eq!(TtsModel::parse("Qwen"), Some(TtsModel::Qwen));
        assert_eq!(TtsModel::parse(" KOKORO "), Some(TtsModel::Kokoro));
        assert_eq!(TtsModel::parse("bogus"), None);
    }

    /// Remove enum = TTS tokens then STT; new models must extend both lists.
    #[test]
    fn model_asset_tokens_are_the_tts_models_then_the_stt_model() {
        assert_eq!(
            &MODEL_ASSET_TOKENS[..TtsModel::TOKENS.len()],
            TtsModel::TOKENS
        );
        assert_eq!(
            &MODEL_ASSET_TOKENS[TtsModel::TOKENS.len()..],
            [STT_MODEL_TOKEN]
        );
        assert!(!TtsModel::TOKENS.contains(&STT_MODEL_TOKEN));
    }

    /// Models then shared, disjoint id spaces.
    #[test]
    fn removable_asset_tokens_are_the_models_then_the_shared_assets() {
        assert_eq!(
            &REMOVABLE_ASSET_TOKENS[..MODEL_ASSET_TOKENS.len()],
            MODEL_ASSET_TOKENS
        );
        assert_eq!(
            &REMOVABLE_ASSET_TOKENS[MODEL_ASSET_TOKENS.len()..],
            SHARED_ASSET_TOKENS
        );
        for id in SHARED_ASSET_TOKENS {
            assert!(!MODEL_ASSET_TOKENS.contains(id), "{id}");
        }
    }

    #[test]
    fn language_contract_is_model_specific() {
        assert!(TtsModel::Chatterbox.descriptor().supports_language("ru"));
        assert!(!TtsModel::Chatterbox.descriptor().supports_language("cs"));
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
        let omnivoice = TtsModel::OmniVoice.descriptor();
        assert!(omnivoice.languages.is_empty());
        assert!(omnivoice.model_languages.is_empty());
        assert!(omnivoice.accepts_detected_language("cs"));
        assert!(omnivoice.accepts_detected_language("en"));
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
    fn detectable_languages_are_the_sorted_union_of_model_coverage() {
        let mut union: Vec<&str> = TTS_MODELS
            .iter()
            .flat_map(|descriptor| descriptor.model_languages.iter().copied())
            .collect();
        union.sort_unstable();
        union.dedup();
        assert_eq!(DETECTABLE_LANGUAGES, union.as_slice());
        assert_eq!(parse_language_code(" EN "), Some("en".to_string()));
        assert_eq!(parse_language_code("zh"), Some("zh".to_string()));
        assert_eq!(parse_language_code("xx"), None);
        assert_eq!(parse_language_code("auto"), None);
        assert_eq!(parse_language_code(""), None);
    }

    #[test]
    fn default_language_is_a_supported_code() {
        // Warm-helper falls back to `default_language` — must be supported. OmniVoice detects
        // internally; its default is unused by the clamp.
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
        assert!(TtsModel::Kokoro.descriptor().params.is_empty());
        let rate = TtsModel::Kokoro
            .descriptor()
            .config_param("rate")
            .expect("kokoro declares persisted rate");
        assert_eq!(rate.default, TtsParamDefault::Float(1.0));
        assert_eq!(rate.kind, TtsParamKind::Float { min: 0.5, max: 2.0 });
        assert!(rate.user_visible && rate.honored_ort && rate.honored_mlx);
        assert_eq!(SYSTEM_TTS_PARAMS.len(), 1);
        assert_eq!(SYSTEM_TTS_PARAMS[0].key, rate.key);
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
        assert!(exaggeration.honored_mlx);
        let penalty = TtsModel::Qwen
            .descriptor()
            .param("repetition_penalty")
            .expect("qwen declares repetition_penalty");
        assert_eq!(penalty.default, TtsParamDefault::Float(1.05));
        assert_eq!(penalty.kind, TtsParamKind::Float { min: 1.0, max: 3.0 });
        assert!(penalty.user_visible && penalty.honored_ort && penalty.honored_mlx);
        let steps = TtsModel::OmniVoice
            .descriptor()
            .param("steps")
            .expect("omnivoice declares steps");
        assert_eq!(steps.default, TtsParamDefault::Int(16));
        assert_eq!(steps.kind, TtsParamKind::Int { min: 1, max: 64 });
        assert!(steps.user_visible && steps.honored_ort && steps.honored_mlx);
        let seed = TtsModel::OmniVoice
            .descriptor()
            .param("seed")
            .expect("omnivoice declares seed");
        assert_eq!(seed.default, TtsParamDefault::Int(-1));
        assert!(!seed.user_visible);
        assert!(seed.honored_ort && seed.honored_mlx);
        assert!(
            matches!(seed.kind, TtsParamKind::Int { min: -1, max } if max == i64::MAX),
            "seed must accept the full non-negative u32/u64-style range plus the -1 sentinel"
        );
        for model in TtsModel::ALL.iter().copied() {
            for param in model.descriptor().config_params {
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
        assert_eq!(
            validate_tts_param(
                "system",
                SYSTEM_TTS_PARAMS,
                "rate",
                &TtsParamValue::Float(1.5),
            ),
            Ok(TtsParamValue::Float(1.5))
        );
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
        let resolved = chatterbox.resolve_params(&TtsParamMap::new());
        assert_eq!(resolved.float(TtsModel::Chatterbox, "exaggeration"), 0.5);
        assert_eq!(resolved.iter().count(), chatterbox.params.len());
        let mut stored = TtsParamMap::new();
        stored.insert("exaggeration".into(), TtsParamValue::Float(1.5));
        assert_eq!(
            chatterbox
                .resolve_params(&stored)
                .float(TtsModel::Chatterbox, "exaggeration"),
            1.5
        );
        let mut stale = TtsParamMap::new();
        stale.insert("exaggeration".into(), TtsParamValue::Float(9.0));
        stale.insert("steps".into(), TtsParamValue::Int(8));
        let resolved = chatterbox.resolve_params(&stale);
        assert_eq!(resolved.float(TtsModel::Chatterbox, "exaggeration"), 0.5);
        assert!(resolved.get("steps").is_none(), "stale keys must drop");
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
        // FluidAudio publishes a Core ML Kokoro only — the other three models have no
        // export, so the rung must be absent from their descriptors rather than being
        // filtered out somewhere downstream.
        assert!(
            TtsModel::Kokoro
                .descriptor()
                .supports_provider(Provider::Fluid)
        );
        for model in [TtsModel::Chatterbox, TtsModel::Qwen, TtsModel::OmniVoice] {
            assert!(
                !model.descriptor().supports_provider(Provider::Fluid),
                "{} must not advertise a FluidAudio backend",
                model.as_str()
            );
        }
        assert!(TtsModel::Chatterbox.descriptor().wants_cuda("auto"));
        assert!(TtsModel::Qwen.descriptor().wants_cuda("cuda"));
        assert!(TtsModel::OmniVoice.descriptor().wants_cuda("auto"));
        assert!(TtsModel::OmniVoice.descriptor().wants_cuda("cuda"));
        assert!(!TtsModel::OmniVoice.descriptor().wants_cuda("cpu"));
    }
}
