//! Single source for DontSpeak's tool catalog.
//!
//! Tools + params authored once (`TOOLS`); consumers generated:
//! * [`catalog`] — MCP schemas + annotations
//! * [`catalog_ui`] — ordered `params` for app FFI (authored order, not property key order)
//!
//! Pure catalog data; dispatch is in the MCP server.

use serde_json::{Map, Value, json};

mod descriptions;
mod set_config;
use descriptions::*;

pub use set_config::{SetConfigArgs, TtsVoiceUpdates};

/// Visibility gate (#77): hide diarize from MCP/list/UI; dispatch/config still work if called.
pub const DIARIZATION_ENABLED: bool = false;

const HIDDEN_TOOLS: &[&str] = &["diarize", "manage_speakers"];
const HIDDEN_SET_CONFIG_PARAMS: &[&str] = &[
    "diarizer",
    "activity_threshold",
    "match_threshold",
    "speaker_lock",
];

enum PType {
    Str,
    Enum(&'static [&'static str]),
    Num(f64, f64),
    Int(i64, i64),
    Bool,
    EnumArray(&'static [&'static str]),
    /// Nested per-engine/model string-array voice pools.
    VoicePools,
    /// `capture_gain`: `"auto"` OR a number `0.5–20` (`oneOf`).
    Gain,
}

struct Param {
    name: &'static str,
    ty: PType,
    required: bool,
    description: &'static str,
}

/// `min_one` ⇒ `minProperties: 1` (for `set_config`).
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

/// Display order (Tools window): speak·listen, stop·mute, read-only, diarize, set_config last.
static TOOLS: &[Tool] = &[
    Tool {
        name: "speak",
        description: SPEAK,
        params: &[
            p("text", PType::Str, true, SPEAK_TEXT),
            p("voice", PType::Str, false, SPEAK_VOICE),
            p("rate", PType::Num(0.5, 2.0), false, SPEAK_RATE),
        ],
        min_one: false,
        annotations: annotations(false, false, false),
        output: None,
    },
    Tool {
        name: "listen",
        description: LISTEN,
        params: &[p("seconds", PType::Int(1, 60), false, LISTEN_SECONDS)],
        min_one: false,
        annotations: annotations(true, false, false),
        output: None,
    },
    Tool {
        name: "stop",
        description: STOP,
        params: &[],
        min_one: false,
        annotations: annotations(false, true, true),
        output: None,
    },
    Tool {
        name: "mute",
        description: MUTE,
        params: &[p("on", PType::Bool, true, MUTE_ON)],
        min_one: false,
        annotations: annotations(false, false, true),
        output: None,
    },
    Tool {
        name: "status",
        description: STATUS,
        params: &[p("detail", PType::Bool, false, STATUS_DETAIL)],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Status),
    },
    Tool {
        name: "usage",
        description: USAGE,
        params: &[p("refresh", PType::Bool, false, USAGE_REFRESH)],
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
        name: "voices",
        description: VOICES,
        params: &[
            p(
                "tts_engine",
                PType::Enum(&["built_in", "system"]),
                false,
                VOICES_ENGINE,
            ),
            p(
                "tts_model",
                PType::Enum(ds_config::TtsModel::TOKENS),
                false,
                VOICES_MODEL,
            ),
            p("language", PType::Str, false, VOICES_LANGUAGE),
        ],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Voices),
    },
    // Hidden when DIARIZATION_ENABLED is false (visibility only).
    Tool {
        name: "diarize",
        description: DIARIZE,
        params: &[p("seconds", PType::Int(1, 60), false, DIARIZE_SECONDS)],
        min_one: false,
        annotations: annotations(true, false, false),
        output: None,
    },
    Tool {
        name: "manage_speakers",
        description: MANAGE_SPEAKERS,
        params: &[
            p(
                "action",
                PType::Enum(&["list", "enroll", "forget"]),
                true,
                SPEAKERS_ACTION,
            ),
            p("name", PType::Str, false, SPEAKERS_NAME),
            p("seconds", PType::Int(1, 60), false, SPEAKERS_SECONDS),
        ],
        min_one: false,
        annotations: annotations(false, true, false),
        output: None,
    },
    Tool {
        name: "set_config",
        description: SET_CONFIG,
        // Grouped by concern — Tools window order.
        params: &[
            p(
                "tts_engine",
                PType::Enum(&["built_in", "system", "off"]),
                false,
                SET_CONFIG_TTS_ENGINE,
            ),
            p(
                "tts_model",
                PType::Enum(ds_config::TtsModel::TOKENS),
                false,
                SET_CONFIG_TTS_MODEL,
            ),
            p(
                "tts_voices",
                PType::VoicePools,
                false,
                SET_CONFIG_TTS_VOICES,
            ),
            p("rate", PType::Num(0.5, 2.0), false, SET_CONFIG_TTS_RATE),
            p(
                "narrate",
                PType::EnumArray(&["shorts", "digests"]),
                false,
                SET_CONFIG_NARRATE,
            ),
            p("greet", PType::Bool, false, SET_CONFIG_GREET),
            p(
                "clear_on_input",
                PType::EnumArray(&["current", "other"]),
                false,
                SET_CONFIG_INPUT_CLEARS,
            ),
            p("pause_bg", PType::Bool, false, SET_CONFIG_PAUSE_BG),
            p("earcon_reply", PType::Str, false, SET_CONFIG_EARCON_REPLY),
            p("earcon_input", PType::Str, false, SET_CONFIG_EARCON_INPUT),
            p("caps", PType::Bool, false, SET_CONFIG_CAPS),
            p(
                "stt_engine",
                PType::Enum(&["built_in", "system", "claude_code", "off"]),
                false,
                SET_CONFIG_STT_ENGINE,
            ),
            p("capture_gain", PType::Gain, false, SET_CONFIG_CAPTURE_GAIN),
            p(
                "double_tap_submit",
                PType::Bool,
                false,
                SET_CONFIG_DOUBLE_TAP_SUBMITS,
            ),
            p(
                "paste_delay_ms",
                PType::Int(0, 5000),
                false,
                SET_CONFIG_PASTE_SUBMIT_DELAY_MS,
            ),
            p("full_duplex", PType::Bool, false, SET_CONFIG_FULL_DUPLEX),
            p(
                "provider",
                PType::EnumArray(&["mlx", "cuda", "coreml", "cpu"]),
                false,
                SET_CONFIG_PROVIDER,
            ),
            // Diarization (hidden when gate off).
            p(
                "diarizer",
                PType::EnumArray(&["mlx"]),
                false,
                SET_CONFIG_DIARIZER,
            ),
            p(
                "activity_threshold",
                PType::Num(0.1, 0.9),
                false,
                SET_CONFIG_ACTIVITY_THRESHOLD,
            ),
            p(
                "match_threshold",
                PType::Num(0.0, 1.0),
                false,
                SET_CONFIG_SPEAKER_THRESH,
            ),
            p("speaker_lock", PType::Bool, false, SET_CONFIG_SPEAKER_LOCK),
            p(
                "tray",
                PType::EnumArray(&["stt", "tts", "stt_animated", "tts_animated"]),
                false,
                SET_CONFIG_TRAY,
            ),
        ],
        min_one: true,
        annotations: annotations(false, true, true),
        output: None,
    },
];

fn is_visible(t: &Tool) -> bool {
    DIARIZATION_ENABLED || !HIDDEN_TOOLS.contains(&t.name)
}

fn visible_params(t: &Tool) -> Vec<&Param> {
    t.params
        .iter()
        .filter(|p| DIARIZATION_ENABLED || !HIDDEN_SET_CONFIG_PARAMS.contains(&p.name))
        .collect()
}

/// Visible primary catalog names. MCP dispatch pins via `router_handles_every_catalog_tool`.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().filter(|t| is_visible(t)).map(|t| t.name)
}

/// Validate against advertised `inputSchema`. Unknown/hidden → unavailable.
pub fn validate_arguments(name: &str, arguments: &Value) -> Result<(), String> {
    let tool = TOOLS
        .iter()
        .find(|tool| tool.name == name && is_visible(tool))
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

/// Visible primary tool definitions for MCP `tools/list`.
pub fn catalog() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t))
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

fn output_schema_for(output: Output) -> Value {
    match output {
        Output::Status => json!({
            "type": "object",
            "properties": {
                "engine": { "type": "string", "enum": ["built_in", "system", "off"] },
                "model": { "type": "string", "enum": ds_config::TtsModel::TOKENS },
                "language": { "type": "string" },
                "voices": { "type": "array", "items": { "type": "string" } },
                "rate": { "type": "number" },
                "state": {
                    "type": "object",
                    "properties": {
                        "running": { "type": "boolean" },
                        "tts_active": { "type": "boolean" },
                        "queued": { "type": "integer", "minimum": 0 },
                        "muted": { "type": "boolean" },
                        "note": { "type": "string" }
                    },
                    "required": ["running"],
                    "additionalProperties": false
                },
                "status": { "type": "object" }
            },
            "required": ["engine", "model", "language", "voices", "rate", "state"],
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
                                "enum": ds_config::ClientSource::CLIENTS
                                    .iter()
                                    .map(|c| c.as_str())
                                    .collect::<Vec<_>>()
                            },
                            "account": { "type": "string" },
                            // true only when guarded (macOS ACL); authorize FFI unlocks.
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
                "language": { "type": "string" },
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
                },
                "models": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "enum": ds_config::TtsModel::TOKENS },
                            "name": { "type": "string" },
                            "default_language": { "type": "string" },
                            "languages": { "type": "array", "items": { "type": "string" } },
                            "providers": { "type": "array", "items": { "type": "string", "enum": ["mlx", "cuda", "coreml", "cpu"] } },
                            "supports_rate": { "type": "boolean" },
                            "supports_full_duplex": { "type": "boolean" }
                        },
                        "required": ["id", "name", "default_language", "languages", "providers", "supports_rate", "supports_full_duplex"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["engine", "model", "language", "languages", "models"],
            "additionalProperties": false
        }),
    }
}

pub fn output_schema(name: &str) -> Option<Value> {
    TOOLS
        .iter()
        .find(|tool| tool.name == name && is_visible(tool))
        .and_then(|tool| tool.output)
        .map(output_schema_for)
}

/// Raw `inputSchema` ignoring `DIARIZATION_ENABLED` — hidden-tool parity tests.
pub fn raw_input_schema(name: &str) -> Option<Value> {
    TOOLS.iter().find(|t| t.name == name).map(input_schema)
}

/// App/UI catalog: ordered `params` array (not JSON-Schema property key order).
pub fn catalog_ui() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t))
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
            ("speak", json!({"text": "hello", "rate": 1.25}), true),
            ("speak", json!({"rate": 1.25}), false),
            ("speak", json!({"text": 7}), false),
            ("speak", json!({"text": "hello", "rate": 2.1}), false),
            ("listen", json!({"seconds": 60}), true),
            ("listen", json!({"seconds": 0}), false),
            ("listen", json!({"seconds": 1.5}), false),
            ("mute", json!({"on": true}), true),
            ("mute", json!({"on": "true"}), false),
            ("voices", json!({"tts_engine": "built_in"}), true),
            ("voices", json!({"tts_engine": "off"}), false),
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
            ("set_config", json!({"tts_language": "ru"}), false),
            ("set_config", json!({"rate": 0.49}), false),
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
                validate_arguments(tool, &arguments).is_ok(),
                valid,
                "{tool} {arguments}"
            );
        }
        assert!(validate_arguments("status", &json!([])).is_err());
        assert!(validate_arguments("unknown", &json!({})).is_err());
    }

    /// DRIFT GUARD: hand-written `docs/MCP-TOOLS.md` must track catalog names + descriptions.
    #[test]
    fn mcp_tools_doc_matches_catalog_descriptions() {
        // Normalize whitespace (doc wraps) and strip backticks before comparing.
        fn normalize(s: &str) -> String {
            s.replace('`', "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        }
        let doc = normalize(include_str!("../../../../docs/MCP-TOOLS.md"));
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
        assert!(v.tts_voices.system.is_empty());
        mentions(
            SET_CONFIG_TTS_VOICES,
            "system: []` uses the OS default",
            "tts_voices.system",
        );
        mentions(
            SET_CONFIG_TTS_RATE,
            &format!("{:.1} = normal", v.rate),
            "rate",
        );

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
        assert_eq!(v.clear_on_input, vec![CancelSpeechScope::Current]);
    }

    #[test]
    fn catalog_is_a_nonempty_array_of_named_tools() {
        let c = catalog();
        let arr = c.as_array().expect("catalog is a JSON array");
        let expected = if DIARIZATION_ENABLED { 10 } else { 8 };
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
        let catalog = catalog();
        let tools = catalog.as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            if matches!(name, "status" | "usage" | "voices") {
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

    /// UI params are ordered (MCP `properties` can't convey order).
    #[test]
    fn catalog_ui_params_are_ordered() {
        let ui = catalog_ui();
        let arr = ui.as_array().expect("ui catalog is an array");
        let expected = if DIARIZATION_ENABLED { 10 } else { 8 };
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
            ["text", "voice", "rate"],
            "speak params keep their authored order"
        );
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

        let cat = catalog();
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
        use crate::{SetConfigArgs, TtsVoiceUpdates};
        use ds_config::{
            CancelSpeechScope, CaptureGain, DiarizerProvider, Provider, SttEngine, TrayKind,
            TtsEngine, TtsModel,
        };

        let populated = SetConfigArgs {
            rate: Some(1.25),
            tts_voices: Some(TtsVoiceUpdates {
                system: Some(vec!["Samantha".to_string()]),
                kokoro: Some(vec!["af_sarah".to_string()]),
                chatterbox: Some(vec!["default".to_string()]),
                qwen: Some(vec!["sohee".to_string()]),
                omnivoice: Some(vec!["whisper".to_string()]),
            }),
            tts_model: Some(TtsModel::Kokoro),
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
        };
        let args = serde_json::to_value(&populated).expect("SetConfigArgs serializes");
        let fields = args.as_object().expect("serializes to an object");

        let cat = catalog();
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
