//! Tool catalog single source (`TOOLS` → [`catalog`] / [`catalog_ui`]).
//! Dispatch lives in the MCP server.

use serde_json::{Map, Value, json};

pub mod descriptions;
mod set_config;
use descriptions::*;

pub use set_config::{SetConfigArgs, TtsParamUpdates, TtsVoiceUpdates};

/// #77: hide diarize from MCP/list/UI (dispatch still works). Keep `ds_model::inventory` in sync.
pub const DIARIZATION_ENABLED: bool = false;

const HIDDEN_TOOLS: &[&str] = &[DIARIZE_NAME, MANAGE_SPEAKERS_NAME];
const HIDDEN_SET_CONFIG_PARAMS: &[&str] =
    &[DIARIZER, ACTIVITY_THRESHOLD, MATCH_THRESHOLD, SPEAKER_LOCK];

enum PType {
    Str,
    Enum(&'static [&'static str]),
    Num(f64, f64),
    Int(i64, i64),
    Bool,
    EnumArray(&'static [&'static str]),
    VoicePools,
    ParamPools,
    TtsArgs,
    /// `capture_gain`: `"auto"` or 0.5–20 (`oneOf`).
    Gain,
}

struct Param {
    name: &'static str,
    ty: PType,
    required: bool,
    description: &'static str,
}

/// `min_one` ⇒ `minProperties: 1` (`set_config`).
struct Tool {
    name: &'static str,
    description: &'static str,
    params: &'static [Param],
    min_one: bool,
    annotations: Annotations,
    output: Option<Output>,
}

#[derive(Clone, Copy)]
struct Annotations {
    read_only: bool,
    destructive: bool,
    idempotent: bool,
    open_world: bool,
}

#[derive(Clone, Copy)]
enum Output {
    Status,
    Usage,
    Voices,
    Models,
}

const fn annotations(read_only: bool, destructive: bool, idempotent: bool) -> Annotations {
    Annotations {
        read_only,
        destructive,
        idempotent,
        open_world: false,
    }
}

const fn p(name: &'static str, ty: PType, required: bool, description: &'static str) -> Param {
    Param {
        name,
        ty,
        required,
        description,
    }
}

/// Tools-window display order (set_config last).
static TOOLS: &[Tool] = &[
    Tool {
        name: SPEAK_NAME,
        description: SPEAK,
        params: &[
            p(TEXT, PType::Str, true, SPEAK_TEXT),
            p(TTS_ARGS, PType::TtsArgs, false, SPEAK_TTS_ARGS),
        ],
        min_one: false,
        annotations: annotations(false, false, false),
        output: None,
    },
    Tool {
        name: LISTEN_NAME,
        description: LISTEN,
        params: &[p(SECONDS, PType::Int(1, 60), false, LISTEN_SECONDS)],
        min_one: false,
        annotations: annotations(false, false, false),
        output: None,
    },
    Tool {
        name: STOP_NAME,
        description: STOP,
        params: &[],
        min_one: false,
        annotations: annotations(false, true, true),
        output: None,
    },
    Tool {
        name: MUTE_NAME,
        description: MUTE,
        params: &[p(ON, PType::Bool, true, MUTE_ON)],
        min_one: false,
        annotations: annotations(false, false, true),
        output: None,
    },
    Tool {
        name: STATUS_NAME,
        description: STATUS,
        params: &[
            p(DETAIL, PType::Bool, false, STATUS_DETAIL),
            p(SINCE, PType::Int(0, i64::MAX), false, STATUS_SINCE),
            p(TIMEOUT_MS, PType::Int(1, 60_000), false, STATUS_TIMEOUT_MS),
        ],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Status),
    },
    Tool {
        name: USAGE_NAME,
        description: USAGE,
        params: &[p(REFRESH, PType::Bool, false, USAGE_REFRESH)],
        min_one: false,
        annotations: Annotations {
            read_only: true,
            destructive: false,
            idempotent: true,
            open_world: true,
        },
        output: Some(Output::Usage),
    },
    Tool {
        name: VOICES_NAME,
        description: VOICES,
        params: &[
            p(
                TTS_ENGINE,
                PType::Enum(&["built_in", "system"]),
                false,
                VOICES_ENGINE,
            ),
            p(
                TTS_MODEL,
                PType::Enum(ds_config::TtsModel::TOKENS),
                false,
                VOICES_MODEL,
            ),
            p(LANGUAGE, PType::Str, false, VOICES_LANGUAGE),
        ],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Voices),
    },
    Tool {
        name: MODELS_NAME,
        description: MODELS,
        params: &[p(
            REMOVE,
            PType::Enum(ds_config::REMOVABLE_ASSET_TOKENS),
            false,
            MODELS_REMOVE,
        )],
        min_one: false,
        annotations: annotations(false, true, true),
        output: Some(Output::Models),
    },
    // Visibility-gated by DIARIZATION_ENABLED.
    Tool {
        name: DIARIZE_NAME,
        description: DIARIZE,
        params: &[p(SECONDS, PType::Int(1, 60), false, DIARIZE_SECONDS)],
        min_one: false,
        annotations: annotations(false, false, false),
        output: None,
    },
    Tool {
        name: MANAGE_SPEAKERS_NAME,
        description: MANAGE_SPEAKERS,
        params: &[
            p(
                ACTION,
                PType::Enum(&["list", "enroll", "forget"]),
                true,
                SPEAKERS_ACTION,
            ),
            p(NAME, PType::Str, false, SPEAKERS_NAME),
            p(SECONDS, PType::Int(1, 60), false, SPEAKERS_SECONDS),
        ],
        min_one: false,
        annotations: annotations(false, true, false),
        output: None,
    },
    Tool {
        name: SET_CONFIG_NAME,
        description: SET_CONFIG,
        params: &[
            p(
                TTS_ENGINE,
                PType::Enum(&["built_in", "system", "off"]),
                false,
                SET_CONFIG_TTS_ENGINE,
            ),
            p(
                TTS_MODEL,
                PType::Enum(ds_config::TtsModel::TOKENS),
                false,
                SET_CONFIG_TTS_MODEL,
            ),
            p(
                PREFERRED_LANGUAGES,
                PType::EnumArray(ds_config::DETECTABLE_LANGUAGES),
                false,
                SET_CONFIG_PREFERRED_LANGUAGES,
            ),
            p(TTS_VOICES, PType::VoicePools, false, SET_CONFIG_TTS_VOICES),
            p(TTS_PARAMS, PType::ParamPools, false, SET_CONFIG_TTS_PARAMS),
            p(
                NARRATE,
                PType::EnumArray(&["shorts", "digests"]),
                false,
                SET_CONFIG_NARRATE,
            ),
            p(GREET, PType::Bool, false, SET_CONFIG_GREET),
            p(
                CLEAR_ON_INPUT,
                PType::EnumArray(&["current", "other"]),
                false,
                SET_CONFIG_INPUT_CLEARS,
            ),
            p(PAUSE_BG, PType::Bool, false, SET_CONFIG_PAUSE_BG),
            p(EARCON_REPLY, PType::Str, false, SET_CONFIG_EARCON_REPLY),
            p(EARCON_INPUT, PType::Str, false, SET_CONFIG_EARCON_INPUT),
            p(CAPS, PType::Bool, false, SET_CONFIG_CAPS),
            p(
                STT_ENGINE,
                PType::Enum(&["built_in", "system", "claude_code", "off"]),
                false,
                SET_CONFIG_STT_ENGINE,
            ),
            p(CAPTURE_GAIN, PType::Gain, false, SET_CONFIG_CAPTURE_GAIN),
            p(
                DOUBLE_TAP_SUBMIT,
                PType::Bool,
                false,
                SET_CONFIG_DOUBLE_TAP_SUBMITS,
            ),
            p(
                PASTE_DELAY_MS,
                PType::Int(0, 5000),
                false,
                SET_CONFIG_PASTE_SUBMIT_DELAY_MS,
            ),
            p(FULL_DUPLEX, PType::Bool, false, SET_CONFIG_FULL_DUPLEX),
            p(
                PROVIDER,
                PType::EnumArray(&["mlx", "fluid", "cuda", "coreml", "cpu"]),
                false,
                SET_CONFIG_PROVIDER,
            ),
            p(
                DIARIZER,
                PType::EnumArray(&["mlx", "fluid"]),
                false,
                SET_CONFIG_DIARIZER,
            ),
            p(
                ACTIVITY_THRESHOLD,
                PType::Num(0.1, 0.9),
                false,
                SET_CONFIG_ACTIVITY_THRESHOLD,
            ),
            p(
                MATCH_THRESHOLD,
                PType::Num(0.0, 1.0),
                false,
                SET_CONFIG_SPEAKER_THRESH,
            ),
            p(SPEAKER_LOCK, PType::Bool, false, SET_CONFIG_SPEAKER_LOCK),
            p(
                TRAY,
                PType::EnumArray(&["stt", "tts", "stt_animated", "tts_animated"]),
                false,
                SET_CONFIG_TRAY,
            ),
            p(AGENTS, PType::Bool, false, SET_CONFIG_AGENTS),
        ],
        min_one: true,
        annotations: annotations(false, true, true),
        output: None,
    },
];

/// Config `agents` gate hides `usage`; diarization gate separate.
fn is_visible(t: &Tool, agents: bool) -> bool {
    if t.name == USAGE_NAME {
        return agents;
    }
    DIARIZATION_ENABLED || !HIDDEN_TOOLS.contains(&t.name)
}

fn visible_params(t: &Tool) -> Vec<&Param> {
    t.params
        .iter()
        .filter(|p| DIARIZATION_ENABLED || !HIDDEN_SET_CONFIG_PARAMS.contains(&p.name))
        .collect()
}

/// Visible catalog names (`router_handles_every_catalog_tool` pins dispatch).
pub fn tool_names(agents: bool) -> impl Iterator<Item = &'static str> {
    TOOLS
        .iter()
        .filter(move |t| is_visible(t, agents))
        .map(|t| t.name)
}

/// Validate against advertised `inputSchema`.
pub fn validate_arguments(name: &str, arguments: &Value, agents: bool) -> Result<(), String> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name && is_visible(tool, agents))
        .ok_or_else(|| format!("unknown tool: {name}"))?;
    let object = arguments
        .as_object()
        .ok_or_else(|| "arguments must be an object".to_string())?;
    let params = visible_params(tool);

    if tool.min_one && object.is_empty() {
        return Err("arguments must contain at least one property".into());
    }
    for key in object.keys() {
        if !params.iter().any(|param| param.name == key) {
            return Err(format!("unknown argument `{key}`"));
        }
    }
    for param in params {
        let Some(value) = object.get(param.name) else {
            if param.required {
                return Err(format!("missing required argument `{}`", param.name));
            }
            continue;
        };
        validate_param(param, value)
            .map_err(|reason| format!("invalid argument `{}`: {reason}", param.name))?;
    }
    Ok(())
}

fn validate_param(param: &Param, value: &Value) -> Result<(), String> {
    fn number_in(value: &Value, min: f64, max: f64) -> bool {
        value
            .as_f64()
            .is_some_and(|number| number >= min && number <= max)
    }

    fn token_in(value: &Value, values: &[&str]) -> bool {
        value.as_str().is_some_and(|token| values.contains(&token))
    }

    match &param.ty {
        PType::Str if value.is_string() => Ok(()),
        PType::Str => Err("must be a string".into()),
        PType::Enum(values) if token_in(value, values) => Ok(()),
        PType::Enum(values) => Err(format!("must be one of: {}", values.join(", "))),
        PType::Num(min, max) if number_in(value, *min, *max) => Ok(()),
        PType::Num(min, max) => Err(format!("must be a number from {min} to {max}")),
        PType::Int(min, max)
            if value
                .as_i64()
                .is_some_and(|number| number >= *min && number <= *max) =>
        {
            Ok(())
        }
        PType::Int(min, max) => Err(format!("must be an integer from {min} to {max}")),
        PType::Bool if value.is_boolean() => Ok(()),
        PType::Bool => Err("must be a boolean".into()),
        PType::EnumArray(values)
            if value
                .as_array()
                .is_some_and(|items| items.iter().all(|item| token_in(item, values))) =>
        {
            Ok(())
        }
        PType::EnumArray(values) => Err(format!(
            "must be an array containing only: {}",
            values.join(", ")
        )),
        PType::VoicePools if voice_pools_valid(value) => Ok(()),
        PType::VoicePools => Err(
            "must be a non-empty object of system, kokoro, chatterbox, qwen, or omnivoice string arrays"
                .into(),
        ),
        PType::ParamPools if tts_param_pools_valid(value) => Ok(()),
        PType::ParamPools => Err(
            "must be a non-empty object of system, kokoro, chatterbox, qwen, or omnivoice parameter objects; see models"
                .into(),
        ),
        PType::TtsArgs if ds_config::TtsArgPools::parse(value).is_ok() => Ok(()),
        PType::TtsArgs => Err(
            "must be a non-empty object of system, kokoro, chatterbox, qwen, or omnivoice utterance arguments; see models"
                .into(),
        ),
        PType::Gain if value.as_str() == Some("auto") || number_in(value, 0.5, 20.0) => Ok(()),
        PType::Gain => Err("must be `auto` or a number from 0.5 to 20".into()),
    }
}

fn voice_pools_valid(value: &Value) -> bool {
    let Some(pools) = value.as_object().filter(|pools| !pools.is_empty()) else {
        return false;
    };
    pools.iter().all(|(name, voices)| {
        let Some(voices) = voices.as_array() else {
            return false;
        };
        let known = name == "system" || ds_config::TtsModel::TOKENS.contains(&name.as_str());
        let allowed_empty = name == "system";
        known
            && (allowed_empty || !voices.is_empty())
            && voices
                .iter()
                .all(|voice| voice.as_str().is_some_and(|voice| !voice.trim().is_empty()))
    })
}

/// `tts_params` against the schema descriptors.
fn tts_param_pools_valid(value: &Value) -> bool {
    let Some(pools) = value.as_object().filter(|pools| !pools.is_empty()) else {
        return false;
    };
    pools.iter().all(|(name, params)| {
        let Some(params) = params.as_object() else {
            return false;
        };
        params.iter().all(|(key, raw)| {
            serde_json::from_value::<ds_config::TtsParamValue>(raw.clone()).is_ok_and(|value| {
                match ds_config::tts_model_descriptor(name) {
                    Some(descriptor) => descriptor.validate_param(key, &value).is_ok(),
                    None if name == "system" => ds_config::validate_tts_param(
                        "system",
                        ds_config::SYSTEM_TTS_PARAMS,
                        key,
                        &value,
                    )
                    .is_ok(),
                    None => false,
                }
            })
        })
    })
}

/// Visible primary tool definitions for MCP `tools/list`.
pub fn catalog(agents: bool) -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t, agents))
            .map(|t| tool_schema(t, t.name))
            .collect(),
    )
}

fn tool_schema(t: &Tool, name: &str) -> Value {
    let mut tool = json!({
        "name": name,
        "description": t.description,
        "inputSchema": input_schema(t),
        "annotations": {
            "readOnlyHint": t.annotations.read_only,
            "destructiveHint": t.annotations.destructive,
            "idempotentHint": t.annotations.idempotent,
            "openWorldHint": t.annotations.open_world,
        },
    });
    if let Some(output) = t.output {
        tool["outputSchema"] = output_schema_for(output);
    }
    tool
}

/// Model tokens plus `null` for the `voices` output, whose `model` is absent for the
/// system engine.
fn model_enum_or_null() -> Vec<Value> {
    let mut values: Vec<Value> = ds_config::TtsModel::TOKENS
        .iter()
        .map(|token| Value::String((*token).to_string()))
        .collect();
    values.push(Value::Null);
    values
}

/// One pool per built-in model plus `system`. Only `system` may be empty — clearing a
/// model's pool is spelled by omitting it, so an empty array is a client bug.
fn voice_pool_properties() -> Value {
    let mut properties = Map::new();
    properties.insert(
        "system".to_string(),
        json!({ "type": "array", "items": { "type": "string", "minLength": 1 } }),
    );
    for token in ds_config::TtsModel::TOKENS {
        properties.insert(
            (*token).to_string(),
            json!({ "type": "array", "minItems": 1, "items": { "type": "string", "minLength": 1 } }),
        );
    }
    Value::Object(properties)
}

fn tts_param_schema(param: &ds_config::TtsParamDescriptor) -> Value {
    match param.kind {
        ds_config::TtsParamKind::Float { min, max } => {
            json!({ "type": "number", "minimum": min, "maximum": max })
        }
        ds_config::TtsParamKind::Int { min, max } => {
            json!({ "type": "integer", "minimum": min, "maximum": max })
        }
        ds_config::TtsParamKind::Choice(choices) => {
            json!({ "type": "string", "enum": choices })
        }
    }
}

fn tts_param_pool_properties() -> Value {
    let mut properties = Map::new();
    let setting_object = |params: &[ds_config::TtsParamDescriptor]| {
        let mut settings = Map::new();
        for param in params {
            settings.insert(param.key.to_string(), tts_param_schema(param));
        }
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": Value::Object(settings),
        })
    };
    properties.insert(
        "system".to_string(),
        setting_object(ds_config::SYSTEM_TTS_PARAMS),
    );
    for descriptor in &ds_config::TTS_MODELS {
        properties.insert(
            descriptor.id.to_string(),
            setting_object(descriptor.config_params),
        );
    }
    Value::Object(properties)
}

fn tts_arg_pool_properties() -> Value {
    let dynamic_voice = |description: &'static str| {
        json!({
            "type": "string",
            "minLength": 1,
            "description": description,
        })
    };
    let target = |params: &[ds_config::TtsParamDescriptor], voice: Value, language: Value| {
        let mut args = Map::new();
        args.insert("voice".to_string(), voice);
        args.insert("language".to_string(), language);
        for param in params {
            args.insert(param.key.to_string(), tts_param_schema(param));
        }
        json!({
            "type": "object",
            "minProperties": 1,
            "additionalProperties": false,
            "properties": Value::Object(args),
        })
    };

    let mut properties = Map::new();
    properties.insert(
        "system".to_string(),
        target(
            ds_config::SYSTEM_TTS_PARAMS,
            dynamic_voice(SPEAK_SYSTEM_VOICE),
            json!({ "type": "string", "minLength": 1 }),
        ),
    );
    for descriptor in &ds_config::TTS_MODELS {
        let voice = if descriptor.model == ds_config::TtsModel::Kokoro {
            dynamic_voice(SPEAK_KOKORO_VOICE)
        } else {
            json!({ "type": "string", "enum": descriptor.voices })
        };
        let language = if descriptor.detects_language_automatically() {
            json!({ "type": "string", "minLength": 1 })
        } else {
            json!({ "type": "string", "enum": descriptor.languages })
        };
        properties.insert(
            descriptor.id.to_string(),
            target(descriptor.config_params, voice, language),
        );
    }
    Value::Object(properties)
}

/// The per-model capability block the `models` rows carry (moved out of `voices`). Split
/// from its parent literal only because one `json!` cannot expand this deep.
fn model_capabilities_schema() -> Value {
    json!({
        "type": ["object", "null"],
        "properties": {
            "name": { "type": "string" },
            "default_language": { "type": "string" },
            "languages": { "type": "array", "items": { "type": "string" } },
            "providers": {
                "type": "array",
                "items": { "type": "string", "enum": ["mlx", "fluid", "cuda", "coreml", "cpu"] }
            },
            "supports_rate": { "type": "boolean" },
            "supports_full_duplex": { "type": "boolean" },
            "params": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "kind": { "type": "string", "enum": ["float", "int", "choice"] },
                        "default": { "type": ["number", "string"] },
                        "min": { "type": "number" },
                        "max": { "type": "number" },
                        "choices": { "type": "array", "items": { "type": "string" } },
                        "visible": { "type": "boolean" }
                    },
                    "required": ["key", "kind", "default", "visible"],
                    "additionalProperties": false
                }
            }
        },
        "required": [
            "name", "default_language", "languages", "providers",
            "supports_rate", "supports_full_duplex", "params"
        ],
        "additionalProperties": false
    })
}

fn output_schema_for(output: Output) -> Value {
    match output {
        Output::Status => json!({
            "type": "object",
            "properties": {
                "engine": { "type": "string", "enum": ["built_in", "system", "off"] },
                "model": { "type": "string", "enum": ds_config::TtsModel::TOKENS },
                "voices": { "type": "array", "items": { "type": "string" } },
                "ignored_voices": { "type": "array", "items": { "type": "string" } },
                "rates": {
                    "type": "object",
                    "properties": {
                        "system": { "type": "number" },
                        "kokoro": { "type": "number" }
                    },
                    "required": ["system", "kokoro"],
                    "additionalProperties": false
                },
                "agents": { "type": "boolean" },
                "state": {
                    "type": "object",
                    "properties": {
                        "running": { "type": "boolean" },
                        "seq": { "type": "integer", "minimum": 0 },
                        "tts_active": { "type": "boolean" },
                        "queued": { "type": "integer", "minimum": 0 },
                        "muted": { "type": "boolean" },
                        "voice_sessions": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "pane_id": { "type": "string" },
                                    "source": { "type": ["string", "null"] },
                                    "active": { "type": "boolean" },
                                    "speaking": { "type": "boolean" },
                                    "queued": { "type": "integer", "minimum": 0 },
                                    "blocked": { "type": "boolean" },
                                    "voice": { "type": ["string", "null"] },
                                    "language": { "type": ["string", "null"] }
                                },
                                "required": [
                                    "pane_id", "source", "active", "speaking", "queued",
                                    "blocked", "voice", "language"
                                ],
                                "additionalProperties": false
                            }
                        },
                        "utterance_id": { "type": ["integer", "null"], "minimum": 1 },
                        "voice": { "type": ["string", "null"] },
                        "detected_language": { "type": ["string", "null"] },
                        "warning": {
                            "type": ["string", "null"],
                            "enum": ["voice_language_mismatch", null]
                        },
                        "recent_utterances": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "integer", "minimum": 1 },
                                    "voice": { "type": ["string", "null"] },
                                    "language": { "type": ["string", "null"] },
                                    "warning": {
                                        "type": ["string", "null"],
                                        "enum": ["voice_language_mismatch", null]
                                    },
                                    "outcome": {
                                        "type": ["string", "null"],
                                        "enum": ["spoken", "failed", "cancelled", "dropped", null]
                                    }
                                },
                                "required": ["id", "voice", "language", "warning", "outcome"],
                                "additionalProperties": false
                            }
                        },
                        "tts": { "type": ["object", "null"] },
                        "stt": { "type": ["object", "null"] },
                        "downloads": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "target": { "type": "string" },
                                    "done_bytes": { "type": "integer", "minimum": 0 },
                                    "total_bytes": { "type": "integer", "minimum": 0 },
                                    "start_bytes": { "type": "integer", "minimum": 0 },
                                    "elapsed_seconds": { "type": "integer", "minimum": 0 }
                                },
                                "required": [
                                    "target", "done_bytes", "total_bytes",
                                    "start_bytes", "elapsed_seconds"
                                ],
                                "additionalProperties": false
                            }
                        },
                        "note": { "type": "string" }
                    },
                    "required": ["running"],
                    "additionalProperties": false
                },
                "status": { "type": "object" }
            },
            "required": ["engine", "model", "voices", "rates", "state"],
            "additionalProperties": false
        }),
        Output::Usage => json!({
            "type": "object",
            "properties": {
                "cards": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                // Registry-driven — new wireable client can't drift out.
                                "enum": ds_config::WiredAgent::ALL
                                    .iter()
                                    .map(|c| c.as_str())
                                    .collect::<Vec<_>>()
                            },
                            "account": { "type": "string" },
                            "needs_auth": { "type": "boolean" },
                            "rows": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "period": {
                                            "type": "string",
                                            "enum": ["session", "week", "month"]
                                        },
                                        "used_percent": {
                                            "type": "number",
                                            "minimum": 0,
                                            "maximum": 100
                                        },
                                        "resets_at_unix": { "type": "integer", "minimum": 1 }
                                    },
                                    "required": ["period", "used_percent", "resets_at_unix"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["agent", "rows"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["cards"],
            "additionalProperties": false
        }),
        Output::Voices => json!({
            "type": "object",
            "properties": {
                "engine": { "type": "string", "enum": ["built_in", "system"] },
                "model": { "type": ["string", "null"], "enum": model_enum_or_null() },
                "language": { "type": ["string", "null"] },
                "ignored_voices": { "type": "array", "items": { "type": "string" } },
                "languages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "language": { "type": "string" },
                            "voices": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "label": { "type": "string" },
                                        "language_tag": { "type": ["string", "null"] },
                                        "gender": { "type": ["string", "null"] },
                                        "engine": { "type": "string", "enum": ["built_in", "system"] },
                                        "model": { "type": "string", "enum": ds_config::TtsModel::TOKENS },
                                        "active": { "type": "boolean" }
                                    },
                                    "required": ["id", "label", "language_tag", "gender", "engine", "active"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["language", "voices"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["engine", "model", "language", "languages"],
            "additionalProperties": false
        }),
        Output::Models => json!({
            "type": "object",
            "properties": {
                "model_dir": { "type": "string" },
                "total_bytes": { "type": "integer", "minimum": 0 },
                "assets": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "kind": { "type": "string", "enum": ["tts", "stt", "frontend", "runtime"] },
                            "installed": { "type": "boolean" },
                            "bytes": { "type": "integer", "minimum": 0 },
                            "active": { "type": "boolean" },
                            "removable": { "type": "boolean" },
                            "reason": { "type": ["string", "null"], "enum": ["active", "shared", null] },
                            "variants": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "installed": { "type": "boolean" },
                                        "bytes": { "type": "integer", "minimum": 0 }
                                    },
                                    "required": ["id", "installed", "bytes"],
                                    "additionalProperties": false
                                }
                            },
                            "capabilities": model_capabilities_schema()
                        },
                        "required": ["id", "kind", "installed", "bytes", "active", "removable", "reason", "variants", "capabilities"],
                        "additionalProperties": false
                    }
                },
                "removed": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "enum": ds_config::REMOVABLE_ASSET_TOKENS },
                        "bytes": { "type": "integer", "minimum": 0 }
                    },
                    "required": ["id", "bytes"],
                    "additionalProperties": false
                }
            },
            "required": ["model_dir", "total_bytes", "assets"],
            "additionalProperties": false
        }),
    }
}

pub fn output_schema(name: &str) -> Option<Value> {
    // Catalog metadata, not a dispatch gate — full visibility (`usage` keeps its schema).
    TOOLS
        .iter()
        .find(|tool| tool.name == name && is_visible(tool, true))
        .and_then(|tool| tool.output)
        .map(output_schema_for)
}

/// Raw `inputSchema` ignoring `DIARIZATION_ENABLED` — hidden-tool parity tests.
pub fn raw_input_schema(name: &str) -> Option<Value> {
    TOOLS.iter().find(|t| t.name == name).map(input_schema)
}

/// App/UI catalog: ordered `params` array (not JSON-Schema property key order).
pub fn catalog_ui(agents: bool) -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t, agents))
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "params": Value::Array(visible_params(t).into_iter().map(param_ui).collect()),
                })
            })
            .collect(),
    )
}

fn input_schema(t: &Tool) -> Value {
    let params = visible_params(t);
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    if !params.is_empty() {
        let mut props = Map::new();
        let mut required = Vec::new();
        for param in params {
            props.insert(param.name.into(), param_schema(param));
            if param.required {
                required.push(json!(param.name));
            }
        }
        schema.insert("properties".into(), Value::Object(props));
        if !required.is_empty() {
            schema.insert("required".into(), Value::Array(required));
        }
    }
    if t.min_one {
        schema.insert("minProperties".into(), json!(1));
    }
    schema.insert("additionalProperties".into(), json!(false));
    Value::Object(schema)
}

fn param_schema(param: &Param) -> Value {
    let d = param.description;
    match &param.ty {
        PType::Str => json!({ "type": "string", "description": d }),
        PType::Enum(vals) => json!({ "type": "string", "enum": vals, "description": d }),
        PType::Num(lo, hi) => {
            json!({ "type": "number", "minimum": lo, "maximum": hi, "description": d })
        }
        PType::Int(lo, hi) => {
            json!({ "type": "integer", "minimum": lo, "maximum": hi, "description": d })
        }
        PType::Bool => json!({ "type": "boolean", "description": d }),
        PType::EnumArray(vals) => {
            json!({ "type": "array", "items": { "type": "string", "enum": vals }, "description": d })
        }
        PType::VoicePools => json!({
            "type": "object",
            "description": d,
            "minProperties": 1,
            "additionalProperties": false,
            "properties": voice_pool_properties()
        }),
        PType::ParamPools => json!({
            "type": "object",
            "description": d,
            "minProperties": 1,
            "additionalProperties": false,
            "properties": tts_param_pool_properties()
        }),
        PType::TtsArgs => json!({
            "type": "object",
            "description": d,
            "minProperties": 1,
            "additionalProperties": false,
            "properties": tts_arg_pool_properties()
        }),
        // No top-level `type` — `oneOf` of the two accepted shapes.
        PType::Gain => json!({
            "description": d,
            "oneOf": [ { "type": "string", "enum": ["auto"] }, { "type": "number", "minimum": 0.5, "maximum": 20.0 } ],
        }),
    }
}

fn param_ui(param: &Param) -> Value {
    let mut o = Map::new();
    o.insert("name".into(), json!(param.name));
    o.insert("required".into(), json!(param.required));
    o.insert("description".into(), json!(param.description));
    match &param.ty {
        PType::Str => {
            o.insert("type".into(), json!("string"));
        }
        PType::Enum(vals) => {
            o.insert("type".into(), json!("string"));
            o.insert("enum".into(), json!(vals));
        }
        PType::Num(lo, hi) => {
            o.insert("type".into(), json!("number"));
            o.insert("minimum".into(), json!(lo));
            o.insert("maximum".into(), json!(hi));
        }
        PType::Int(lo, hi) => {
            o.insert("type".into(), json!("integer"));
            o.insert("minimum".into(), json!(lo));
            o.insert("maximum".into(), json!(hi));
        }
        PType::Bool => {
            o.insert("type".into(), json!("boolean"));
        }
        PType::EnumArray(vals) => {
            o.insert("type".into(), json!("array"));
            o.insert("enum".into(), json!(vals));
        }
        PType::VoicePools => {
            o.insert("type".into(), json!("object"));
        }
        PType::ParamPools => {
            o.insert("type".into(), json!("object"));
        }
        PType::TtsArgs => {
            o.insert("type".into(), json!("object"));
        }
        PType::Gain => {
            o.insert("type".into(), json!("number_or_enum"));
            o.insert("enum".into(), json!(["auto"]));
            o.insert("minimum".into(), json!(0.5));
            o.insert("maximum".into(), json!(20.0));
        }
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_validation_matches_advertised_constraints() {
        let cases = [
            (
                "speak",
                json!({"text": "hello", "tts_args": {"kokoro": {"voice": "af_sarah", "language": "en", "rate": 1.25}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"kokoro": {"voice": "custom_pack_voice"}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"system": {"voice": "Installed SAPI Voice"}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"chatterbox": {"voice": "default"}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"qwen": {"voice": "ryan"}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"omnivoice": {"voice": "young_woman"}}}),
                true,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"chatterbox": {"voice": "unknown"}}}),
                false,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"qwen": {"voice": "unknown"}}}),
                false,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"omnivoice": {"voice": "unknown"}}}),
                false,
            ),
            (
                "speak",
                json!({"tts_args": {"system": {"rate": 1.25}}}),
                false,
            ),
            ("speak", json!({"text": 7}), false),
            ("speak", json!({"text": "hello", "rate": 1.25}), false),
            (
                "speak",
                json!({"text": "hello", "voice": "af_sarah"}),
                false,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"qwen": {"rate": 1.25}}}),
                false,
            ),
            (
                "speak",
                json!({"text": "hello", "tts_args": {"kokoro": {"language": "ru"}}}),
                false,
            ),
            ("listen", json!({"seconds": 60}), true),
            ("listen", json!({"seconds": 0}), false),
            ("listen", json!({"seconds": 1.5}), false),
            ("mute", json!({"on": true}), true),
            ("mute", json!({"on": "true"}), false),
            ("voices", json!({"tts_engine": "built_in"}), true),
            ("voices", json!({"tts_engine": "bogus"}), false),
            ("models", json!({}), true),
            ("models", json!({"remove": "kokoro"}), true),
            ("models", json!({"remove": "parakeet"}), true),
            ("models", json!({"remove": "onnxruntime"}), true),
            ("models", json!({"remove": "cuda"}), true),
            ("models", json!({"remove": "sepformer"}), false),
            ("models", json!({"extra": true}), false),
            ("set_config", json!({"narrate": ["shorts"]}), true),
            ("set_config", json!({"narrate": ["other"]}), false),
            (
                "set_config",
                json!({"tts_voices": {"kokoro": ["af_sarah"]}}),
                true,
            ),
            ("set_config", json!({"tts_voices": {"kokoro": [7]}}), false),
            ("set_config", json!({"tts_voices": {"system": []}}), true),
            (
                "set_config",
                json!({"tts_voices": {"legacy": ["voice"]}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"chatterbox": {"exaggeration": 1.5}}}),
                true,
            ),
            (
                "set_config",
                json!({"tts_params": {"system": {"rate": 1.25}, "kokoro": {"rate": 0.8}}}),
                true,
            ),
            (
                "set_config",
                json!({"tts_params": {"omnivoice": {"steps": 32}}}),
                true,
            ),
            // Reset-to-defaults spelling.
            ("set_config", json!({"tts_params": {"qwen": {}}}), true),
            ("set_config", json!({"tts_params": {}}), false),
            (
                "set_config",
                json!({"tts_params": {"chatterbox": {"exaggeration": 2.5}}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"kokoro": {"steps": 8}}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"system": {"language": "en"}}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"qwen": {"rate": 1.2}}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"legacy": {"steps": 8}}}),
                false,
            ),
            (
                "set_config",
                json!({"tts_params": {"omnivoice": {"steps": true}}}),
                false,
            ),
            ("set_config", json!({"preferred_languages": ["en"]}), true),
            (
                "set_config",
                json!({"preferred_languages": ["en", "fr"]}),
                true,
            ),
            ("set_config", json!({"preferred_languages": []}), true),
            (
                "set_config",
                json!({"preferred_languages": ["english"]}),
                false,
            ),
            ("set_config", json!({"tts_language": "ru"}), false),
            ("set_config", json!({"language": "ru"}), false),
            ("set_config", json!({"rate": 1.0}), false),
            ("set_config", json!({"capture_gain": "auto"}), true),
            ("set_config", json!({"capture_gain": 20.1}), false),
            ("set_config", json!({}), false),
            ("status", json!({"extra": true}), false),
            ("usage", json!({"refresh": true}), true),
            ("usage", json!({"refresh": "true"}), false),
            ("usage", json!({"extra": true}), false),
            ("stop", json!({}), true),
            ("get_usage", json!({}), false),
            ("list_voices", json!({}), false),
            ("get_status", json!({}), false),
            ("stop_speech", json!({}), false),
        ];

        for (tool, arguments, valid) in cases {
            assert_eq!(
                validate_arguments(tool, &arguments, true).is_ok(),
                valid,
                "{tool} {arguments}"
            );
        }
        assert!(validate_arguments("status", &json!([]), true).is_err());
        assert!(validate_arguments("unknown", &json!({}), true).is_err());
    }

    /// Config `agents` gate: `usage` invisible + unavailable when off; catalogs shrink by one.
    #[test]
    fn agents_gate_hides_usage_everywhere() {
        assert!(tool_names(true).any(|name| name == "usage"));
        assert!(!tool_names(false).any(|name| name == "usage"));
        assert_eq!(tool_names(true).count(), tool_names(false).count() + 1);

        let err = validate_arguments("usage", &json!({}), false).unwrap_err();
        assert!(err.contains("unknown tool"), "got: {err}");
        assert!(validate_arguments("usage", &json!({}), true).is_ok());
        // Other tools are unaffected by the gate.
        assert!(validate_arguments("stop", &json!({}), false).is_ok());

        for cat in [catalog(false), catalog_ui(false)] {
            assert!(
                cat.as_array().unwrap().iter().all(|t| t["name"] != "usage"),
                "usage must be hidden while agents is off"
            );
        }
        assert!(
            catalog(true)
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "usage")
        );
    }

    const MCP_TOOLS_DOC: &str = include_str!("../../../../docs/MCP-TOOLS.md");

    /// Normalize whitespace (doc wraps), strip backticks, and unescape markdown
    /// table pipes (`\|` → `|`) before comparing.
    fn normalize(s: &str) -> String {
        s.replace('`', "")
            .replace("\\|", "|")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// DRIFT GUARD: hand-written `docs/MCP-TOOLS.md` must track catalog names +
    /// tool/param descriptions (`descriptions.rs` is the single source).
    #[test]
    fn mcp_tools_doc_matches_catalog_descriptions() {
        let doc = normalize(MCP_TOOLS_DOC);
        for t in TOOLS {
            assert!(
                doc.contains(t.name),
                "docs/MCP-TOOLS.md is missing tool `{}`",
                t.name
            );
            assert!(
                doc.contains(&normalize(t.description)),
                "docs/MCP-TOOLS.md's `{}` description doesn't match the catalog's:\n{}",
                t.name,
                t.description
            );
            for param in visible_params(t) {
                assert!(
                    doc.contains(&normalize(param.description)),
                    "docs/MCP-TOOLS.md's `{}.{}` param description doesn't match the catalog's:\n{}",
                    t.name,
                    param.name,
                    param.description
                );
            }
        }
    }

    /// DRIFT GUARD: the doc's annotation summary named `listen`/`diarize` read-only
    /// (issue #221) because the description guard above compares prose only. Pin the
    /// hint lists to the catalog, in catalog order.
    #[test]
    fn mcp_tools_doc_matches_catalog_annotations() {
        fn claim(label: &str, hint: fn(&Annotations) -> bool) -> String {
            let names: Vec<&str> = TOOLS
                .iter()
                .filter(|t| is_visible(t, true) && hint(&t.annotations))
                .map(|t| t.name)
                .collect();
            format!("{label}: {}.", names.join(", "))
        }
        let doc = normalize(MCP_TOOLS_DOC);
        for claim in [
            claim("Read-only", |a| a.read_only),
            claim("Idempotent", |a| a.idempotent),
            claim("Open-world", |a| a.open_world),
        ] {
            assert!(
                doc.contains(&claim),
                "docs/MCP-TOOLS.md must claim `{claim}`"
            );
        }
    }

    /// DRIFT GUARD: every catalog name/description matches `descriptions` constants;
    /// MCP `tools/list` and Tools-tab `catalog_ui` emit the same prose.
    #[test]
    fn catalog_and_ui_use_description_constants() {
        type ToolExpect = (
            &'static str,
            &'static str,
            &'static [(&'static str, &'static str)],
        );
        let expected: &[ToolExpect] = &[
            (
                SPEAK_NAME,
                SPEAK,
                &[(TEXT, SPEAK_TEXT), (TTS_ARGS, SPEAK_TTS_ARGS)],
            ),
            (LISTEN_NAME, LISTEN, &[(SECONDS, LISTEN_SECONDS)]),
            (STOP_NAME, STOP, &[]),
            (MUTE_NAME, MUTE, &[(ON, MUTE_ON)]),
            (
                STATUS_NAME,
                STATUS,
                &[
                    (DETAIL, STATUS_DETAIL),
                    (SINCE, STATUS_SINCE),
                    (TIMEOUT_MS, STATUS_TIMEOUT_MS),
                ],
            ),
            (USAGE_NAME, USAGE, &[(REFRESH, USAGE_REFRESH)]),
            (
                VOICES_NAME,
                VOICES,
                &[
                    (TTS_ENGINE, VOICES_ENGINE),
                    (TTS_MODEL, VOICES_MODEL),
                    (LANGUAGE, VOICES_LANGUAGE),
                ],
            ),
            (MODELS_NAME, MODELS, &[(REMOVE, MODELS_REMOVE)]),
            (DIARIZE_NAME, DIARIZE, &[(SECONDS, DIARIZE_SECONDS)]),
            (
                MANAGE_SPEAKERS_NAME,
                MANAGE_SPEAKERS,
                &[
                    (ACTION, SPEAKERS_ACTION),
                    (NAME, SPEAKERS_NAME),
                    (SECONDS, SPEAKERS_SECONDS),
                ],
            ),
            (
                SET_CONFIG_NAME,
                SET_CONFIG,
                &[
                    (TTS_ENGINE, SET_CONFIG_TTS_ENGINE),
                    (TTS_MODEL, SET_CONFIG_TTS_MODEL),
                    (PREFERRED_LANGUAGES, SET_CONFIG_PREFERRED_LANGUAGES),
                    (TTS_VOICES, SET_CONFIG_TTS_VOICES),
                    (TTS_PARAMS, SET_CONFIG_TTS_PARAMS),
                    (NARRATE, SET_CONFIG_NARRATE),
                    (GREET, SET_CONFIG_GREET),
                    (CLEAR_ON_INPUT, SET_CONFIG_INPUT_CLEARS),
                    (PAUSE_BG, SET_CONFIG_PAUSE_BG),
                    (EARCON_REPLY, SET_CONFIG_EARCON_REPLY),
                    (EARCON_INPUT, SET_CONFIG_EARCON_INPUT),
                    (CAPS, SET_CONFIG_CAPS),
                    (STT_ENGINE, SET_CONFIG_STT_ENGINE),
                    (CAPTURE_GAIN, SET_CONFIG_CAPTURE_GAIN),
                    (DOUBLE_TAP_SUBMIT, SET_CONFIG_DOUBLE_TAP_SUBMITS),
                    (PASTE_DELAY_MS, SET_CONFIG_PASTE_SUBMIT_DELAY_MS),
                    (FULL_DUPLEX, SET_CONFIG_FULL_DUPLEX),
                    (PROVIDER, SET_CONFIG_PROVIDER),
                    (DIARIZER, SET_CONFIG_DIARIZER),
                    (ACTIVITY_THRESHOLD, SET_CONFIG_ACTIVITY_THRESHOLD),
                    (MATCH_THRESHOLD, SET_CONFIG_SPEAKER_THRESH),
                    (SPEAKER_LOCK, SET_CONFIG_SPEAKER_LOCK),
                    (TRAY, SET_CONFIG_TRAY),
                    (AGENTS, SET_CONFIG_AGENTS),
                ],
            ),
        ];
        assert_eq!(TOOLS.len(), expected.len());
        for (tool, &(name, desc, params)) in TOOLS.iter().zip(expected) {
            assert_eq!(tool.name, name, "tool name must match name constant");
            assert_eq!(
                tool.description, desc,
                "tool `{name}` description must match description constant"
            );
            assert_eq!(tool.params.len(), params.len(), "tool `{name}`");
            for (param, &(pname, pdesc)) in tool.params.iter().zip(params) {
                assert_eq!(
                    param.name, pname,
                    "param name on `{name}` must match name constant"
                );
                assert_eq!(
                    param.description, pdesc,
                    "param `{pname}` on `{name}` description must match constant"
                );
            }
        }

        // MCP + Tools-tab JSON surfaces emit the same names and descriptions.
        for agents in [false, true] {
            let mcp = catalog(agents);
            let ui = catalog_ui(agents);
            let mcp_tools = mcp.as_array().unwrap();
            let ui_tools = ui.as_array().unwrap();
            assert_eq!(mcp_tools.len(), ui_tools.len());
            for (m, u) in mcp_tools.iter().zip(ui_tools) {
                assert_eq!(m["name"], u["name"]);
                assert_eq!(m["description"], u["description"]);
                let u_params = u["params"].as_array().unwrap();
                match m["inputSchema"]["properties"].as_object() {
                    Some(props) => {
                        assert_eq!(props.len(), u_params.len());
                        for up in u_params {
                            let pname = up["name"].as_str().unwrap();
                            assert_eq!(
                                props[pname]["description"], up["description"],
                                "MCP vs Tools-tab param description for {pname}"
                            );
                        }
                    }
                    None => assert!(u_params.is_empty()),
                }
            }
        }
    }

    /// DRIFT GUARD: set_config description defaults must agree with VoiceConfig::default().
    /// Bridges catalog and ds-config — either crate alone cannot catch the split.
    #[test]
    fn set_config_descriptions_match_voice_defaults() {
        use ds_config::{
            CancelSpeechScope, CaptureGain, NarrateKind, Provider, TrayKind, VoiceConfig,
        };

        fn mentions(description: &str, expected: &str, field: &str) {
            assert!(
                description.contains(expected),
                "`{field}` description does not state its live default `{expected}`:\n{description}"
            );
        }

        let v = VoiceConfig::default();
        assert!(v.tts_engine.is_none());
        mentions(
            SET_CONFIG_TTS_ENGINE,
            "Omit to keep the automatic preference",
            "tts_engine",
        );
        assert!(v.preferred_languages.is_empty());
        mentions(
            SET_CONFIG_PREFERRED_LANGUAGES,
            "auto-detect (default)",
            "preferred_languages",
        );
        assert!(v.tts_voices.system.is_empty());
        mentions(
            SET_CONFIG_TTS_VOICES,
            "system: []` uses the OS default",
            "tts_voices.system",
        );
        assert_eq!(v.system_rate(), 1.0);
        assert_eq!(v.model_rate(ds_config::TtsModel::Kokoro), 1.0);
        mentions(SET_CONFIG_TTS_PARAMS, "default 1.0", "tts_params.*.rate");

        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        mentions(SET_CONFIG_NARRATE, "Default both", "narrate");
        mentions(
            SET_CONFIG_GREET,
            if v.greet { "Default on" } else { "Default off" },
            "greet",
        );
        mentions(
            SET_CONFIG_INPUT_CLEARS,
            &format!(
                "Default {}",
                serde_json::to_string(&v.clear_on_input).unwrap()
            ),
            "clear_on_input",
        );
        mentions(
            SET_CONFIG_PAUSE_BG,
            if v.pause_bg {
                "Default true"
            } else {
                "Default false"
            },
            "pause_bg",
        );

        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        assert!(!v.earcon_reply.is_empty());
        mentions(SET_CONFIG_EARCON_REPLY, "Default: OS chime", "earcon_reply");
        assert!(v.earcon_input.is_empty());
        mentions(SET_CONFIG_EARCON_INPUT, "Default off", "earcon_input");

        mentions(
            SET_CONFIG_CAPS,
            if v.caps { "Default on" } else { "Default off" },
            "caps",
        );
        assert!(v.stt_engine.is_none());
        mentions(
            SET_CONFIG_STT_ENGINE,
            "Omit to keep the automatic preference",
            "stt_engine",
        );
        assert_eq!(v.capture_gain, CaptureGain::Auto);
        mentions(
            SET_CONFIG_CAPTURE_GAIN,
            "\"auto\" (default)",
            "capture_gain",
        );
        mentions(
            SET_CONFIG_DOUBLE_TAP_SUBMITS,
            if v.double_tap_submit {
                "Default true"
            } else {
                "Default false"
            },
            "double_tap_submit",
        );
        mentions(
            SET_CONFIG_PASTE_SUBMIT_DELAY_MS,
            &format!("Default {}", v.paste_delay_ms),
            "paste_delay_ms",
        );

        assert_eq!(
            v.provider,
            vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu]
        );
        mentions(
            SET_CONFIG_PROVIDER,
            &format!("Default {}", serde_json::to_string(&v.provider).unwrap()),
            "provider",
        );
        assert!(v.diarizer.is_empty());
        mentions(SET_CONFIG_DIARIZER, "[] = off (default)", "diarizer");
        mentions(
            SET_CONFIG_ACTIVITY_THRESHOLD,
            &format!("Default {}", v.activity_threshold),
            "activity_threshold",
        );
        mentions(
            SET_CONFIG_SPEAKER_THRESH,
            &format!("Default {}", v.match_threshold),
            "match_threshold",
        );
        mentions(
            SET_CONFIG_SPEAKER_LOCK,
            if v.speaker_lock {
                "Default on"
            } else {
                "Default off"
            },
            "speaker_lock",
        );
        mentions(
            SET_CONFIG_FULL_DUPLEX,
            if v.full_duplex {
                "Default true"
            } else {
                "Default false"
            },
            "full_duplex",
        );
        assert_eq!(v.tray, vec![TrayKind::Stt, TrayKind::TtsAnimated]);
        mentions(
            SET_CONFIG_TRAY,
            &format!("Default {}", serde_json::to_string(&v.tray).unwrap()),
            "tray",
        );
        assert!(!v.agents);
        mentions(SET_CONFIG_AGENTS, "Off by default", "agents");
        assert_eq!(v.clear_on_input, vec![CancelSpeechScope::Current]);
    }

    #[test]
    fn catalog_is_a_nonempty_array_of_named_tools() {
        let c = catalog(true);
        let arr = c.as_array().expect("catalog is a JSON array");
        let expected = if DIARIZATION_ENABLED { 11 } else { 9 };
        assert_eq!(arr.len(), expected, "expected {expected} catalog entries");
        for t in arr {
            assert!(
                t.get("name").and_then(|v| v.as_str()).is_some(),
                "each tool has a name"
            );
            assert!(
                t.get("description").and_then(|v| v.as_str()).is_some(),
                "each tool has a description"
            );
            assert!(
                t.get("inputSchema").is_some(),
                "each tool has an inputSchema"
            );
            let annotations = t["annotations"]
                .as_object()
                .expect("each tool has annotations");
            assert_eq!(annotations.len(), 4, "all annotation hints are explicit");
            assert_eq!(annotations["openWorldHint"], t["name"] == "usage");
            for hint in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                assert!(annotations[hint].is_boolean(), "{hint} is boolean");
            }
        }
    }

    #[test]
    fn structured_tools_advertise_their_output_schemas() {
        let catalog = catalog(true);
        let tools = catalog.as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if matches!(name, "status" | "usage" | "voices" | "models") {
                assert_eq!(tool["outputSchema"]["type"], "object");
                assert_eq!(output_schema(name), Some(tool["outputSchema"].clone()));
            } else {
                assert!(tool.get("outputSchema").is_none(), "{name}");
                assert!(output_schema(name).is_none(), "{name}");
            }
        }
        assert!(output_schema("get_status").is_none());
        assert!(output_schema("list_voices").is_none());
        assert!(output_schema("get_usage").is_none());
    }

    #[test]
    fn tools_that_launch_the_host_are_not_annotated_read_only() {
        for name in ["listen", "diarize", "models"] {
            let tool = TOOLS
                .iter()
                .find(|tool| tool.name == name)
                .expect("tool definition");
            assert!(!tool.annotations.read_only, "{name}");
        }
    }

    #[test]
    fn voices_output_schema_allows_an_unfiltered_system_language() {
        assert_eq!(
            output_schema("voices").unwrap()["properties"]["language"]["type"],
            json!(["string", "null"])
        );
    }

    /// UI params are ordered (MCP `properties` can't convey order).
    #[test]
    fn catalog_ui_params_are_ordered() {
        let ui = catalog_ui(true);
        let arr = ui.as_array().expect("ui catalog is an array");
        let expected = if DIARIZATION_ENABLED { 11 } else { 9 };
        assert_eq!(arr.len(), expected, "UI lists primary tools only");

        let speak = arr
            .iter()
            .find(|t| t["name"] == "speak")
            .expect("speak tool");
        let names: Vec<&str> = speak["params"]
            .as_array()
            .expect("speak has a params array")
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["text", "tts_args"],
            "speak params keep their authored order"
        );
    }

    #[test]
    fn speak_tts_args_schema_matches_every_target_descriptor() {
        let catalog = catalog(true);
        let speak = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "speak")
            .unwrap();
        let targets = speak["inputSchema"]["properties"]["tts_args"]["properties"]
            .as_object()
            .unwrap();
        let mut advertised_targets: Vec<&str> = targets.keys().map(String::as_str).collect();
        advertised_targets.sort_unstable();
        let mut expected_targets = ds_config::TtsModel::TOKENS.to_vec();
        expected_targets.push("system");
        expected_targets.sort_unstable();
        assert_eq!(advertised_targets, expected_targets);

        let assert_target = |target: &str, params: &[ds_config::TtsParamDescriptor]| {
            let properties = targets[target]["properties"].as_object().unwrap();
            let mut advertised: Vec<&str> = properties.keys().map(String::as_str).collect();
            advertised.sort_unstable();
            let mut expected = vec!["language", "voice"];
            expected.extend(params.iter().map(|param| param.key));
            expected.sort_unstable();
            assert_eq!(advertised, expected, "{target} speak arguments drifted");
            assert_eq!(targets[target]["minProperties"], 1);
            assert_eq!(targets[target]["additionalProperties"], false);
        };
        assert_target("system", ds_config::SYSTEM_TTS_PARAMS);
        let assert_dynamic_voice = |target: &str, description: &str| {
            let voice = &targets[target]["properties"]["voice"];
            assert_eq!(voice["type"], "string", "{target} voice type drifted");
            assert_eq!(voice["minLength"], 1, "{target} voice must stay non-empty");
            assert!(
                voice.get("enum").is_none(),
                "{target} voice catalog is dynamic"
            );
            assert_eq!(
                voice["description"], description,
                "{target} voice discovery prose drifted"
            );
        };
        assert_dynamic_voice("system", SPEAK_SYSTEM_VOICE);
        assert!(SPEAK_SYSTEM_VOICE.contains("macOS"));
        assert!(SPEAK_SYSTEM_VOICE.contains("Windows"));
        assert!(SPEAK_SYSTEM_VOICE.contains("without tool enumeration"));
        for descriptor in &ds_config::TTS_MODELS {
            assert_target(descriptor.id, descriptor.config_params);
            if descriptor.model == ds_config::TtsModel::Kokoro {
                assert_dynamic_voice(descriptor.id, SPEAK_KOKORO_VOICE);
                assert!(SPEAK_KOKORO_VOICE.contains("`voices` tool"));
            } else {
                let voice = &targets[descriptor.id]["properties"]["voice"];
                assert_eq!(
                    voice["enum"],
                    json!(descriptor.voices),
                    "{} voice schema drifted",
                    descriptor.id
                );
                assert!(
                    voice.get("description").is_none(),
                    "{} voice values are fully described by its enum",
                    descriptor.id
                );
            }
            if descriptor.detects_language_automatically() {
                assert!(
                    targets[descriptor.id]["properties"]["language"]
                        .get("enum")
                        .is_none()
                );
            } else {
                assert_eq!(
                    targets[descriptor.id]["properties"]["language"]["enum"],
                    json!(descriptor.languages),
                    "{} language schema drifted",
                    descriptor.id
                );
            }
        }
    }

    /// PARITY GUARD for the OTHER provider enum. `set_config`'s array is pinned by
    /// `set_config_enums_match_config_types` below, but the `models` OUTPUT schema carries
    /// a second, independent literal in `model_capabilities_schema` — and the `ds-core`
    /// payload check validates key presence and JSON type, never enum membership. Without
    /// this, adding a `Provider` variant leaves `models` advertising a capability list that
    /// omits a provider it actually reports, and nothing fails.
    #[test]
    fn model_capabilities_provider_enum_matches_the_provider_vocabulary() {
        use ds_config::Provider;

        let advertised: Vec<String> =
            model_capabilities_schema()["properties"]["providers"]["items"]["enum"]
                .as_array()
                .expect("providers.items.enum is an array")
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("provider token is a string")
                        .to_string()
                })
                .collect();
        let expected: Vec<String> = Provider::ALL
            .iter()
            .map(|provider| provider.as_str().to_string())
            .collect();
        assert_eq!(advertised, expected);
    }

    /// CATALOG-LEVEL drift guard for HIDDEN enum params. `set_config_enums_match_config_types`
    /// walks the EMITTED schema, so while `DIARIZATION_ENABLED == false` the `diarizer` param is
    /// absent and NOTHING there checks its vocabulary against `DiarizerProvider`. This walks the
    /// `TOOLS` catalog directly instead, so a new `DiarizerProvider` rung that isn't mirrored
    /// into the param's `EnumArray` fails HERE even while the row ships dark. Every hidden
    /// enum-array param must be pinned, so the next hidden rung cannot drift either.
    #[test]
    fn hidden_enum_params_track_their_vocabularies() {
        use ds_config::DiarizerProvider;

        let set_config = TOOLS
            .iter()
            .find(|t| t.name == "set_config")
            .expect("set_config tool in TOOLS");

        // Each hidden set_config param that carries an enum-array vocabulary, pinned to its
        // backing ds_config enum. Extend this when a hidden enum param is added.
        let pinned: &[(&str, Vec<&'static str>)] = &[(
            DIARIZER,
            DiarizerProvider::ALL.iter().map(|&d| d.as_str()).collect(),
        )];

        let mut checked = 0;
        for param in set_config.params {
            if !HIDDEN_SET_CONFIG_PARAMS.contains(&param.name) {
                continue;
            }
            if let PType::EnumArray(vocab) = &param.ty {
                let (_, want) = pinned
                    .iter()
                    .find(|(name, _)| *name == param.name)
                    .unwrap_or_else(|| {
                        panic!("hidden enum param {} has no pinned vocabulary", param.name)
                    });
                assert_eq!(
                    vocab.to_vec(),
                    *want,
                    "hidden param {} enum drifted from its ds_config vocabulary",
                    param.name
                );
                checked += 1;
            }
        }
        // The guard is only load-bearing while the param is actually hidden from the wire.
        assert!(HIDDEN_SET_CONFIG_PARAMS.contains(&DIARIZER));
        assert_eq!(checked, pinned.len(), "every pinned hidden enum was found");
    }

    /// PARITY GUARD: set_config enums must list exactly the backing ds_config tokens.
    #[test]
    fn set_config_enums_match_config_types() {
        use ds_config::{
            CancelSpeechScope, DiarizerProvider, Provider, SttEngine, TrayKind, TtsEngine,
        };

        fn toks<T: Copy>(all: &[T], as_str: fn(T) -> &'static str) -> Vec<String> {
            all.iter().map(|&v| as_str(v).to_string()).collect()
        }

        let cat = catalog(true);
        let set_config = cat
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "set_config")
            .expect("set_config in catalog");
        let props = &set_config["inputSchema"]["properties"];
        // The plain SET / ladder array fields → tokens live at `items.enum`.
        let schema_item_enum = |field: &str| -> Vec<String> {
            props[field]["items"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{field} should have an items.enum array"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };
        // tts_engine / stt_engine are now a scalar-string PREFERENCE (force one engine, or the
        // literal "off") — tokens live at the plain top-level `enum`, same as any other
        // `PType::Enum` field. `Engine::ALL` stays the single source for the REAL engine
        // tokens; `"off"` is appended separately since it's not a real `TtsEngine`/`SttEngine`
        // variant (the enums carry no `Off` — off is handled directly by the deserializer).
        let schema_string_enum = |field: &str| -> Vec<String> {
            props[field]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{field} should have an enum array"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };

        let mut tts_tokens = toks(TtsEngine::ALL, TtsEngine::as_str);
        tts_tokens.push("off".to_string());
        assert_eq!(schema_string_enum("tts_engine"), tts_tokens);

        let mut stt_tokens = toks(SttEngine::ALL, SttEngine::as_str);
        stt_tokens.push("off".to_string());
        assert_eq!(schema_string_enum("stt_engine"), stt_tokens);
        assert_eq!(
            schema_string_enum("tts_model"),
            toks(ds_config::TtsModel::ALL, ds_config::TtsModel::as_str)
        );
        assert_eq!(
            schema_item_enum("provider"),
            toks(Provider::ALL, Provider::as_str)
        );
        assert_eq!(
            schema_item_enum("preferred_languages"),
            ds_config::DETECTABLE_LANGUAGES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(props["agents"]["type"], "boolean");
        // `diarizer` is one of the hidden diarization params (see
        // `HIDDEN_SET_CONFIG_PARAMS`) — pin that it's actually absent from the wire
        // schema while the gate is off, rather than just skipping the assertion.
        if DIARIZATION_ENABLED {
            assert_eq!(
                schema_item_enum("diarizer"),
                toks(DiarizerProvider::ALL, DiarizerProvider::as_str)
            );
        } else {
            assert!(
                props.get("diarizer").is_none(),
                "diarizer should be hidden from the schema while DIARIZATION_ENABLED is false"
            );
        }
        assert_eq!(
            schema_item_enum("tray"),
            toks(TrayKind::ALL, TrayKind::as_str)
        );
        assert_eq!(
            schema_item_enum("clear_on_input"),
            toks(CancelSpeechScope::ALL, CancelSpeechScope::as_str)
        );

        let pools = props["tts_params"]["properties"]
            .as_object()
            .expect("tts_params should be an object schema");
        let mut pool_names: Vec<&str> = pools.keys().map(String::as_str).collect();
        pool_names.sort_unstable();
        let mut target_tokens: Vec<&str> = ds_config::TtsModel::TOKENS.to_vec();
        target_tokens.push("system");
        target_tokens.sort_unstable();
        assert_eq!(pool_names, target_tokens);
        assert_eq!(
            pools["system"]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["rate"]
        );
        for descriptor in &ds_config::TTS_MODELS {
            let mut declared: Vec<&str> = descriptor.config_params.iter().map(|p| p.key).collect();
            declared.sort_unstable();
            let advertised: Vec<&str> = pools[descriptor.id]["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(advertised, declared, "{} params drifted", descriptor.id);
        }
    }

    /// Map a JSON value to the JSON-Schema scalar `type` token it satisfies.
    fn json_type_of(v: &serde_json::Value) -> &'static str {
        use serde_json::Value::*;
        match v {
            Bool(_) => "boolean",
            String(_) => "string",
            Array(_) => "array",
            Object(_) => "object",
            Number(n) => {
                if n.is_f64() {
                    "number"
                } else {
                    "integer"
                }
            }
            Null => "null",
        }
    }

    /// DRIFT GUARD: the GENERATED `set_config` schema must match the fields of
    /// `crate::SetConfigArgs` — the struct the handler deserializes into — by NAME and
    /// declared TYPE. The fully-populated literal is exhaustive (no `..`), so a NEW struct
    /// field breaks this at COMPILE time; the names come from serde, so this can't go stale.
    #[test]
    fn set_config_schema_matches_args() {
        use crate::{SetConfigArgs, TtsParamUpdates, TtsVoiceUpdates};
        use ds_config::{
            CancelSpeechScope, CaptureGain, DiarizerProvider, Provider, SttEngine, TrayKind,
            TtsEngine, TtsModel, TtsParamValue,
        };

        let populated = SetConfigArgs {
            tts_voices: Some(TtsVoiceUpdates {
                system: Some(vec!["Samantha".to_string()]),
                kokoro: Some(vec!["af_sarah".to_string()]),
                chatterbox: Some(vec!["default".to_string()]),
                qwen: Some(vec!["sohee".to_string()]),
                omnivoice: Some(vec!["whisper".to_string()]),
            }),
            tts_params: Some(TtsParamUpdates {
                system: Some([("rate".to_string(), TtsParamValue::Float(1.25))].into()),
                kokoro: Some([("rate".to_string(), TtsParamValue::Float(0.8))].into()),
                chatterbox: Some([("exaggeration".to_string(), TtsParamValue::Float(1.5))].into()),
                qwen: Some([("repetition_penalty".to_string(), TtsParamValue::Float(1.2))].into()),
                omnivoice: Some([("steps".to_string(), TtsParamValue::Int(32))].into()),
            }),
            tts_model: Some(TtsModel::Kokoro),
            preferred_languages: Some(vec!["en".into()]),
            tts_engine: Some(vec![TtsEngine::BuiltIn]),
            stt_engine: Some(vec![SttEngine::ClaudeCode]),
            provider: Some(vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu]),
            diarizer: Some(vec![DiarizerProvider::Mlx]),
            activity_threshold: Some(0.5),
            match_threshold: Some(0.65),
            speaker_lock: Some(false),
            full_duplex: Some(true),
            narrate: Some(vec![ds_config::NarrateKind::Digests]),
            caps: Some(true),
            greet: Some(true),
            tray: Some(vec![TrayKind::Stt, TrayKind::Tts]),
            capture_gain: Some(CaptureGain::Manual(2.0)),
            double_tap_submit: Some(true),
            paste_delay_ms: Some(100),
            clear_on_input: Some(vec![CancelSpeechScope::Current, CancelSpeechScope::Other]),
            pause_bg: Some(true),
            earcon_reply: Some("Tink".to_string()),
            earcon_input: Some("Funk".to_string()),
            agents: Some(true),
        };
        let args = serde_json::to_value(&populated).expect("SetConfigArgs serializes");
        let fields = args.as_object().expect("serializes to an object");

        let cat = catalog(true);
        let set_config = cat
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "set_config")
            .expect("set_config tool in catalog");
        let props = set_config["inputSchema"]["properties"]
            .as_object()
            .expect("set_config inputSchema has properties");

        // Name parity. `SetConfigArgs` intentionally still carries the hidden diarization
        // fields even while they're absent from the wire schema (dispatch/config keep
        // working end-to-end when called directly — this is a visibility gate, not a
        // rip-out), so filter them out of the struct side unless the gate is on.
        let mut schema_keys: Vec<&String> = props.keys().collect();
        let mut struct_keys: Vec<&String> = fields
            .keys()
            .filter(|k| DIARIZATION_ENABLED || !HIDDEN_SET_CONFIG_PARAMS.contains(&k.as_str()))
            .collect();
        schema_keys.sort();
        struct_keys.sort();
        assert_eq!(
            schema_keys, struct_keys,
            "set_config inputSchema properties and SetConfigArgs fields are out of sync"
        );

        // Type parity, for every property declaring a scalar `type`. `capture_gain` uses
        // `oneOf` (no top-level `type`), so it is name-checked only. `tts_engine`/`stt_engine`
        // are ALSO name-checked only: the schema declares `"string"` (the wire shape — a
        // single scalar token, or "off"), but `SetConfigArgs`'s plain derived `Serialize`
        // (used only by this test, never at runtime — the real wire path is the strict
        // deserializer) renders the underlying `Option<Vec<Engine>>` as a JSON array, so the
        // two representations intentionally diverge here.
        for (name, prop) in props {
            if name == "tts_engine" || name == "stt_engine" {
                continue;
            }
            if let Some(decl) = prop.get("type").and_then(|t| t.as_str()) {
                let actual = json_type_of(&fields[name]);
                assert_eq!(
                    decl, actual,
                    "set_config property `{name}`: schema type `{decl}` != struct field type `{actual}`"
                );
            }
        }
    }
}
