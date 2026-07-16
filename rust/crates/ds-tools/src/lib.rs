//! ds-tools — the SINGLE source of truth for DontSpeak's tool catalog.
//!
//! Tools + their parameters are authored ONCE here as structured data (`TOOLS`, in
//! display order), and BOTH consumer shapes are GENERATED from it so they can't drift:
//!
//! * [`catalog`] — MCP tool definitions with input/output schemas and behavioral annotations.
//! * [`catalog_ui`] — `{ name, description, params: [ … ] }` with the params as an
//!   ORDERED ARRAY, the form the app-facing FFI (`ds-core::ds_tools_json`)
//!   hands the SwiftUI Tools window. An array (not the unordered JSON-Schema `properties`
//!   object) so the authored order survives to the UI.
//!
//! The dispatch (actually running a tool) lives in the MCP server; this crate is the
//! catalog only — pure data, no I/O. It depends on `ds-config` for the typed
//! `SetConfigArgs` surface and the enum tokens it pins its authored strings against.

use serde_json::{Map, Value, json};

// The description strings live in ONE separate file (no structure, no logic) so they're easy to
// read/edit in isolation; `TOOLS` below references them by name.
mod descriptions;
mod set_config;
use descriptions::*;

pub use set_config::SetConfigArgs;

/// Diarization (`diarize`/`manage_speakers` + set_config's 4 diarization params) is
/// implemented but hidden pending the validation tracked in issue #77. This is the one
/// toggle that hides it from every user-facing surface (MCP
/// tools/list, the set_config schema, and — via ds_tools_json — the macOS/Windows Tools
/// windows). Dispatch/config/engine keep working end-to-end when called directly
/// regardless of this flag (see dontspeak::tools::tools_call) — this is a VISIBILITY
/// gate, not a functional rip-out.
pub const DIARIZATION_ENABLED: bool = false;

const HIDDEN_TOOLS: &[&str] = &["diarize", "manage_speakers"];
const HIDDEN_SET_CONFIG_PARAMS: &[&str] = &[
    "diarizer_provider",
    "clustering_threshold",
    "speaker_threshold",
    "stt_speaker_lock",
];

/// The JSON-Schema shape of a tool parameter.
enum PType {
    Str,
    Enum(&'static [&'static str]),
    Num(f64, f64),
    Int(i64, i64),
    Bool,
    StrArray,
    EnumArray(&'static [&'static str]),
    /// `capture_gain`: `"auto"` OR a number `0.5–20` (JSON-Schema `oneOf`).
    Gain,
}

/// One tool parameter — authored once, in display order.
struct Param {
    name: &'static str,
    ty: PType,
    required: bool,
    description: &'static str,
}

/// One tool. `min_one` ⇒ `minProperties: 1` (for `set_config`).
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

/// The whole catalog, in display order — the ONE source both consumer shapes generate
/// from, and the exact order the Tools window shows. Ordered to lead with the two core
/// actions (speak · listen) so the highest-frequency tools sit first (primacy), then the
/// output-control pair (stop_speech · mute), then read-only introspection (get_status · list_voices), then
/// speaker diarization (diarize · manage_speakers — the voiceprint library it labels with),
/// and finally the rare admin tool (set_config) in the low-attention tail.
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
        name: "stop_speech",
        description: STOP_SPEECH,
        params: &[],
        min_one: false,
        annotations: annotations(false, true, true),
        output: None,
    },
    // Global mute — same switch the app drives.
    Tool {
        name: "mute",
        description: MUTE,
        params: &[p("on", PType::Bool, true, MUTE_ON)],
        min_one: false,
        annotations: annotations(false, false, true),
        output: None,
    },
    Tool {
        name: "get_status",
        description: GET_STATUS,
        params: &[p("detail", PType::Bool, false, STATUS_DETAIL)],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Status),
    },
    Tool {
        name: "list_voices",
        description: LIST_VOICES,
        params: &[p(
            "tts_engine",
            PType::Enum(&["built_in", "system"]),
            false,
            LIST_VOICES_ENGINE,
        )],
        min_one: false,
        annotations: annotations(true, false, true),
        output: Some(Output::Voices),
    },
    // Hidden when `DIARIZATION_ENABLED` is false (visibility only — see that flag).
    Tool {
        name: "diarize",
        description: DIARIZE,
        params: &[p("seconds", PType::Int(1, 60), false, DIARIZE_SECONDS)],
        min_one: false,
        annotations: annotations(true, false, false),
        output: None,
    },
    // One action-dispatched tool (list / enroll / forget) instead of three.
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
        // Grouped by concern — this order is what the Tools window shows.
        params: &[
            // ── TTS output ──
            p(
                "tts_engine",
                PType::Enum(&["built_in", "system", "off"]),
                false,
                SET_CONFIG_TTS_ENGINE,
            ),
            p(
                "tts_built_in_voices",
                PType::StrArray,
                false,
                SET_CONFIG_TTS_VOICES,
            ),
            p(
                "tts_system_voice",
                PType::Str,
                false,
                SET_CONFIG_TTS_SYSTEM_VOICE,
            ),
            p("tts_rate", PType::Num(0.5, 2.0), false, SET_CONFIG_TTS_RATE),
            // ── Narration ──
            p(
                "narrate",
                PType::EnumArray(&["shorts", "digests"]),
                false,
                SET_CONFIG_NARRATE,
            ),
            p("greet_on_open", PType::Bool, false, SET_CONFIG_GREET),
            p(
                "input_clears",
                PType::EnumArray(&["current", "other"]),
                false,
                SET_CONFIG_INPUT_CLEARS,
            ),
            p(
                "pause_in_background",
                PType::Bool,
                false,
                SET_CONFIG_PAUSE_BG,
            ),
            // ── Earcons ──
            p(
                "earcon_reply_sound",
                PType::Str,
                false,
                SET_CONFIG_EARCON_REPLY,
            ),
            p(
                "earcon_needs_input_sound",
                PType::Str,
                false,
                SET_CONFIG_EARCON_INPUT,
            ),
            // ── STT / dictation ──
            p("caps_enabled", PType::Bool, false, SET_CONFIG_CAPS),
            p(
                "stt_engine",
                PType::Enum(&["built_in", "system", "claude_code", "off"]),
                false,
                SET_CONFIG_STT_ENGINE,
            ),
            p("capture_gain", PType::Gain, false, SET_CONFIG_CAPTURE_GAIN),
            p(
                "double_tap_submits",
                PType::Bool,
                false,
                SET_CONFIG_DOUBLE_TAP_SUBMITS,
            ),
            p(
                "paste_submit_delay_ms",
                PType::Int(0, 5000),
                false,
                SET_CONFIG_PASTE_SUBMIT_DELAY_MS,
            ),
            p("full_duplex", PType::Bool, false, SET_CONFIG_FULL_DUPLEX),
            // ── Compute backend ──
            p(
                "provider",
                PType::EnumArray(&["ane", "cuda", "coreml", "cpu"]),
                false,
                SET_CONFIG_PROVIDER,
            ),
            // ── Diarization (hidden when gate is off) ──
            p(
                "diarizer_provider",
                PType::EnumArray(&["apple_native"]),
                false,
                SET_CONFIG_DIARIZER,
            ),
            p(
                "clustering_threshold",
                PType::Num(0.5, 0.9),
                false,
                SET_CONFIG_CLUSTERING,
            ),
            p(
                "speaker_threshold",
                PType::Num(0.0, 1.0),
                false,
                SET_CONFIG_SPEAKER_THRESH,
            ),
            p(
                "stt_speaker_lock",
                PType::Bool,
                false,
                SET_CONFIG_SPEAKER_LOCK,
            ),
            // ── UI ──
            p(
                "tray_indicator",
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

/// Catalog (display) order. MCP dispatch pins against this via
/// `router_handles_every_catalog_tool` in `dontspeak::tools`.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().filter(|t| is_visible(t)).map(|t| t.name)
}

/// Validate against the advertised `inputSchema`. Unknown/hidden tools → unavailable.
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
        PType::StrArray
            if value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)) =>
        {
            Ok(())
        }
        PType::StrArray => Err("must be an array of strings".into()),
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
        PType::Gain if value.as_str() == Some("auto") || number_in(value, 0.5, 20.0) => Ok(()),
        PType::Gain => Err("must be `auto` or a number from 0.5 to 20".into()),
    }
}

pub fn catalog() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t))
            .map(tool_schema)
            .collect(),
    )
}

fn tool_schema(t: &Tool) -> Value {
    let mut tool = json!({
        "name": t.name,
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

fn output_schema_for(output: Output) -> Value {
    match output {
        Output::Status => json!({
            "type": "object",
            "properties": {
                "engine": { "type": "string", "enum": ["kokoro", "system", "off"] },
                "voice": { "type": "string" },
                "voices": { "type": "array", "items": { "type": "string" } },
                "rate": { "type": "number" },
                "state": {
                    "type": "object",
                    "properties": {
                        "running": { "type": "boolean" },
                        "tts_active": { "type": "boolean" },
                        "queued": { "type": "integer", "minimum": 0 },
                        "paused": { "type": "boolean" },
                        "muted": { "type": "boolean" },
                        "note": { "type": "string" }
                    },
                    "required": ["running"],
                    "additionalProperties": false
                },
                "models": { "type": "object" }
            },
            "required": ["engine", "voice", "voices", "rate", "state"],
            "additionalProperties": false
        }),
        Output::Voices => json!({
            "type": "object",
            "properties": {
                "engine": { "type": "string", "enum": ["kokoro", "system"] },
                "language": { "type": "string", "enum": ["en"] },
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
                                        "engine": { "type": "string", "enum": ["kokoro", "system"] },
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
            "required": ["engine", "language", "languages"],
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

/// RAW `inputSchema` by name, ignoring `DIARIZATION_ENABLED` — so hidden-tool
/// schema/dispatch parity tests don't go through filtered `catalog()`.
pub fn raw_input_schema(name: &str) -> Option<Value> {
    TOOLS.iter().find(|t| t.name == name).map(input_schema)
}

/// App/UI catalog: params as an ORDERED array (authored order), not JSON-Schema
/// `properties` object key order. Tools window renders this directly.
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
        PType::StrArray => {
            json!({ "type": "array", "items": { "type": "string" }, "description": d })
        }
        PType::EnumArray(vals) => {
            json!({ "type": "array", "items": { "type": "string", "enum": vals }, "description": d })
        }
        // No top-level `type` — `oneOf` of the two accepted shapes.
        PType::Gain => json!({
            "description": d,
            "oneOf": [ { "type": "string", "enum": ["auto"] }, { "type": "number", "minimum": 0.5, "maximum": 20.0 } ],
        }),
    }
}

/// UI param object for the ordered `params` array (type + enum/range for Tools window).
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
        PType::StrArray => {
            o.insert("type".into(), json!("array"));
        }
        PType::EnumArray(vals) => {
            o.insert("type".into(), json!("array"));
            o.insert("enum".into(), json!(vals));
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
            ("list_voices", json!({"tts_engine": "built_in"}), true),
            ("list_voices", json!({"tts_engine": "off"}), false),
            ("set_config", json!({"narrate": ["shorts"]}), true),
            ("set_config", json!({"narrate": ["other"]}), false),
            (
                "set_config",
                json!({"tts_built_in_voices": ["af_sarah"]}),
                true,
            ),
            ("set_config", json!({"tts_built_in_voices": [7]}), false),
            ("set_config", json!({"tts_rate": 0.49}), false),
            ("set_config", json!({"capture_gain": "auto"}), true),
            ("set_config", json!({"capture_gain": 20.1}), false),
            ("set_config", json!({}), false),
            ("get_status", json!({"extra": true}), false),
        ];

        for (tool, arguments, valid) in cases {
            assert_eq!(
                validate_arguments(tool, &arguments).is_ok(),
                valid,
                "{tool} {arguments}"
            );
        }
        assert!(validate_arguments("get_status", &json!([])).is_err());
        assert!(validate_arguments("unknown", &json!({})).is_err());
    }

    /// DRIFT GUARD: `docs/MCP-TOOLS.md` is hand-written, not generated, so nothing else
    /// forces it to track the catalog. Embed it at compile time and assert every tool's
    /// name and description appear verbatim — catches a description edited here without
    /// the doc following, or a tool added/removed without the doc updating to match.
    #[test]
    fn mcp_tools_doc_matches_catalog_descriptions() {
        // Normalize whitespace (the doc hand-wraps prose at ~90 cols, so a description
        // spanning a wrap has a newline where the source has a space) and strip backticks
        // (the doc's own markdown formatting, not part of the authored text) before comparing.
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

    /// DRIFT GUARD: every default stated by a `set_config` parameter description must agree
    /// with the object used when `config.toml` is absent. This deliberately bridges the
    /// authored catalog and `ds-config`; tests in either crate alone cannot catch that split.
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
        assert!(v.tts_system_voice.is_empty());
        mentions(
            SET_CONFIG_TTS_SYSTEM_VOICE,
            "empty = OS default",
            "tts_system_voice",
        );
        mentions(
            SET_CONFIG_TTS_RATE,
            &format!("{:.1} = normal", v.tts_rate),
            "tts_rate",
        );

        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        mentions(SET_CONFIG_NARRATE, "Default both", "narrate");
        mentions(
            SET_CONFIG_GREET,
            if v.greet_on_open {
                "Default on"
            } else {
                "Default off"
            },
            "greet_on_open",
        );
        mentions(
            SET_CONFIG_INPUT_CLEARS,
            &format!(
                "Default {}",
                serde_json::to_string(&v.input_clears).unwrap()
            ),
            "input_clears",
        );
        mentions(
            SET_CONFIG_PAUSE_BG,
            if v.pause_in_background {
                "Default true"
            } else {
                "Default false"
            },
            "pause_in_background",
        );

        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        assert!(!v.earcon_reply_sound.is_empty());
        mentions(
            SET_CONFIG_EARCON_REPLY,
            "Default: OS chime",
            "earcon_reply_sound",
        );
        assert!(v.earcon_needs_input_sound.is_empty());
        mentions(
            SET_CONFIG_EARCON_INPUT,
            "Default off",
            "earcon_needs_input_sound",
        );

        mentions(
            SET_CONFIG_CAPS,
            if v.caps_enabled {
                "Default on"
            } else {
                "Default off"
            },
            "caps_enabled",
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
            if v.double_tap_submits {
                "Default true"
            } else {
                "Default false"
            },
            "double_tap_submits",
        );
        mentions(
            SET_CONFIG_PASTE_SUBMIT_DELAY_MS,
            &format!("Default {}", v.paste_submit_delay_ms),
            "paste_submit_delay_ms",
        );

        assert_eq!(
            v.provider,
            vec![Provider::Ane, Provider::OrtCuda, Provider::OrtCpu]
        );
        mentions(
            SET_CONFIG_PROVIDER,
            &format!("Default {}", serde_json::to_string(&v.provider).unwrap()),
            "provider",
        );
        assert!(v.diarizer_provider.is_empty());
        mentions(
            SET_CONFIG_DIARIZER,
            "[] = off (default)",
            "diarizer_provider",
        );
        mentions(
            SET_CONFIG_CLUSTERING,
            &format!("Default {}", v.clustering_threshold),
            "clustering_threshold",
        );
        mentions(
            SET_CONFIG_SPEAKER_THRESH,
            &format!("Default {}", v.speaker_threshold),
            "speaker_threshold",
        );
        mentions(
            SET_CONFIG_SPEAKER_LOCK,
            if v.stt_speaker_lock {
                "Default on"
            } else {
                "Default off"
            },
            "stt_speaker_lock",
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
        assert_eq!(v.tray_indicator, vec![TrayKind::Stt, TrayKind::TtsAnimated]);
        mentions(
            SET_CONFIG_TRAY,
            &format!(
                "Default {}",
                serde_json::to_string(&v.tray_indicator).unwrap()
            ),
            "tray_indicator",
        );
        assert_eq!(v.input_clears, vec![CancelSpeechScope::Current]);
    }

    #[test]
    fn catalog_is_a_nonempty_array_of_named_tools() {
        let c = catalog();
        let arr = c.as_array().expect("catalog is a JSON array");
        let expected = if DIARIZATION_ENABLED { 9 } else { 7 };
        assert_eq!(arr.len(), expected, "expected {expected} visible tools");
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
            assert_eq!(annotations["openWorldHint"], false);
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
            if matches!(name, "get_status" | "list_voices") {
                assert_eq!(tool["outputSchema"]["type"], "object");
                assert_eq!(output_schema(name), Some(tool["outputSchema"].clone()));
            } else {
                assert!(tool.get("outputSchema").is_none(), "{name}");
                assert!(output_schema(name).is_none(), "{name}");
            }
        }
    }

    /// UI catalog params are an ORDERED array (MCP `properties` can't convey order).
    #[test]
    fn catalog_ui_params_are_ordered() {
        let ui = catalog_ui();
        let arr = ui.as_array().expect("ui catalog is an array");
        let expected = if DIARIZATION_ENABLED { 9 } else { 7 };
        assert_eq!(arr.len(), expected, "same visible tools as the MCP catalog");

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

    /// PARITY GUARD: every `set_config` enum must list EXACTLY the tokens of its backing
    /// ds_config enum, so the authored strings can't silently drift from the Rust types.
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
            schema_item_enum("provider"),
            toks(Provider::ALL, Provider::as_str)
        );
        // `diarizer_provider` is one of the hidden diarization params (see
        // `HIDDEN_SET_CONFIG_PARAMS`) — pin that it's actually absent from the wire
        // schema while the gate is off, rather than just skipping the assertion.
        if DIARIZATION_ENABLED {
            assert_eq!(
                schema_item_enum("diarizer_provider"),
                toks(DiarizerProvider::ALL, DiarizerProvider::as_str)
            );
        } else {
            assert!(
                props.get("diarizer_provider").is_none(),
                "diarizer_provider should be hidden from the schema while DIARIZATION_ENABLED is false"
            );
        }
        assert_eq!(
            schema_item_enum("tray_indicator"),
            toks(TrayKind::ALL, TrayKind::as_str)
        );
        assert_eq!(
            schema_item_enum("input_clears"),
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
        use crate::SetConfigArgs;
        use ds_config::{
            CancelSpeechScope, CaptureGain, DiarizerProvider, Provider, SttEngine, TrayKind,
            TtsEngine,
        };

        let populated = SetConfigArgs {
            tts_rate: Some(1.25),
            tts_built_in_voices: Some(vec!["af_sarah".to_string()]),
            tts_system_voice: Some("Samantha".to_string()),
            tts_engine: Some(vec![TtsEngine::Kokoro]),
            stt_engine: Some(vec![SttEngine::ClaudeCode]),
            provider: Some(vec![Provider::Ane, Provider::OrtCuda, Provider::OrtCpu]),
            diarizer_provider: Some(vec![DiarizerProvider::AppleNative]),
            clustering_threshold: Some(0.7),
            speaker_threshold: Some(0.65),
            stt_speaker_lock: Some(false),
            full_duplex: Some(true),
            narrate: Some(vec![ds_config::NarrateKind::Digests]),
            caps_enabled: Some(true),
            greet_on_open: Some(true),
            tray_indicator: Some(vec![TrayKind::Stt, TrayKind::Tts]),
            capture_gain: Some(CaptureGain::Manual(2.0)),
            double_tap_submits: Some(true),
            paste_submit_delay_ms: Some(100),
            input_clears: Some(vec![CancelSpeechScope::Current, CancelSpeechScope::Other]),
            pause_in_background: Some(true),
            earcon_reply_sound: Some("Tink".to_string()),
            earcon_needs_input_sound: Some("Funk".to_string()),
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
