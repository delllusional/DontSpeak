//! ds-tools — the SINGLE source of truth for DontSpeak's tool catalog.
//!
//! Tools + their parameters are authored ONCE here as structured data (`TOOLS`, in
//! display order), and BOTH consumer shapes are GENERATED from it so they can't drift:
//!
//! * [`catalog`] — `{ name, description, inputSchema }` (JSON-Schema 2020-12), the MCP
//!   form the `dontspeak` server exposes to Claude.
//! * [`catalog_ui`] — `{ name, description, params: [ … ] }` with the params as an
//!   ORDERED ARRAY, the form the app-facing FFI (`ds-core::ds_tools_json`)
//!   hands the SwiftUI Tools window. An array (not the unordered JSON-Schema `properties`
//!   object) so the authored order survives to the UI.
//!
//! The dispatch (actually running a tool) lives in the MCP server; this crate is the
//! catalog only — pure data, no I/O. It is a BUILD LEAF (no `ds-config` dep); the
//! enum tokens are authored as strings and a dev-only test pins them to the config types.

use serde_json::{Map, Value, json};

// The description strings live in ONE separate file (no structure, no logic) so they're easy to
// read/edit in isolation; `TOOLS` below references them by name.
mod descriptions;
use descriptions::*;

/// The JSON-Schema shape of a tool parameter.
enum PType {
    /// A free-form string.
    Str,
    /// A string constrained to a fixed set of tokens.
    Enum(&'static [&'static str]),
    /// A number with an inclusive `[min, max]`.
    Num(f64, f64),
    /// An integer with an inclusive `[min, max]`.
    Int(i64, i64),
    /// A boolean flag.
    Bool,
    /// An array of strings.
    StrArray,
    /// An array whose items are constrained to a fixed token set (e.g. `narrate`).
    EnumArray(&'static [&'static str]),
    /// `capture_gain`: the string `"auto"` OR a number `0.5–20` (a JSON-Schema `oneOf`).
    Gain,
}

/// One tool parameter — authored once, in display order.
struct Param {
    name: &'static str,
    ty: PType,
    required: bool,
    description: &'static str,
}

/// One tool: name, description, its ordered params, and whether at least one property is
/// required (`minProperties: 1`, for `set_config`).
struct Tool {
    name: &'static str,
    description: &'static str,
    params: &'static [Param],
    min_one: bool,
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
/// from, and the exact order the Tools window shows. Ordered so related tools sit
/// together: spoken output (speak · stop_speak · list_voices · set_voice), then voice
/// input (listen), then runtime state (status), then speaker diarization (diarize ·
/// enroll · forget_speaker · list_speakers), then config (set_config) and the setup
/// tool that writes config / wires clients (wire).
static TOOLS: &[Tool] = &[
    // Spoken output: say something, then the voice it's said in.
    Tool {
        name: "speak",
        description: SPEAK,
        params: &[
            p("text", PType::Str, true, SPEAK_TEXT),
            p("voice", PType::Str, false, SPEAK_VOICE),
            p("rate", PType::Num(0.5, 2.0), false, SPEAK_RATE),
        ],
        min_one: false,
    },
    // Spoken output: halt playback, then discover/choose/clear the voice replies use.
    Tool {
        name: "stop_speak",
        description: STOP_SPEAK,
        params: &[],
        min_one: false,
    },
    Tool {
        name: "list_voices",
        description: LIST_VOICES,
        params: &[
            p(
                "tts_engine",
                PType::Enum(&["built_in", "system"]),
                false,
                LIST_VOICES_ENGINE,
            ),
            p("language", PType::Str, false, LIST_VOICES_LANGUAGE),
        ],
        min_one: false,
    },
    Tool {
        name: "set_voice",
        description: SET_VOICE,
        params: &[
            p("voice", PType::Str, false, SET_VOICE_VOICE),
            p(
                "tts_engine",
                PType::Enum(&["built_in", "system"]),
                false,
                SET_VOICE_ENGINE,
            ),
        ],
        min_one: false,
    },
    // Voice input (dictation).
    Tool {
        name: "listen",
        description: LISTEN,
        params: &[p("seconds", PType::Int(1, 60), false, LISTEN_SECONDS)],
        min_one: false,
    },
    // Runtime introspection.
    Tool {
        name: "status",
        description: STATUS,
        params: &[p("detail", PType::Bool, false, STATUS_DETAIL)],
        min_one: false,
    },
    // ── Speaker diarization (who spoke when) + voiceprint enrollment ──
    Tool {
        name: "diarize",
        description: DIARIZE,
        params: &[p("seconds", PType::Int(1, 60), false, DIARIZE_SECONDS)],
        min_one: false,
    },
    Tool {
        name: "enroll",
        description: ENROLL,
        params: &[
            p("name", PType::Str, true, ENROLL_NAME),
            p("seconds", PType::Int(1, 60), false, ENROLL_SECONDS),
        ],
        min_one: false,
    },
    Tool {
        name: "forget_speaker",
        description: FORGET_SPEAKER,
        params: &[p("name", PType::Str, true, FORGET_SPEAKER_NAME)],
        min_one: false,
    },
    Tool {
        name: "list_speakers",
        description: LIST_SPEAKERS,
        params: &[],
        min_one: false,
    },
    // Persistent settings, then one-time client wiring.
    Tool {
        name: "set_config",
        description: SET_CONFIG,
        // Grouped by concern (TTS output · narration · STT/dictation · compute · diarization ·
        // UI) so related knobs sit together — this order is what the Tools window shows.
        params: &[
            // ── TTS output ──
            p(
                "tts_engine",
                PType::EnumArray(&["off", "built_in", "system"]),
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
                "drop_speech_on",
                PType::EnumArray(&["voice", "text"]),
                false,
                SET_CONFIG_DROP_SPEECH,
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
                PType::EnumArray(&["off", "built_in", "system", "claude_code"]),
                false,
                SET_CONFIG_STT_ENGINE,
            ),
            p("capture_gain", PType::Gain, false, SET_CONFIG_CAPTURE_GAIN),
            p("auto_submit", PType::Bool, false, SET_CONFIG_AUTO_SUBMIT),
            // ── Compute backend ──
            p(
                "provider",
                PType::EnumArray(&["ane", "ort_cuda", "ort_coreml", "ort_cpu"]),
                false,
                SET_CONFIG_PROVIDER,
            ),
            // ── Diarization ──
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
    },
    Tool {
        name: "wire",
        description: WIRE,
        params: &[
            p(
                "target",
                PType::Enum(&["narration_spec", "claude_code", "claude_desktop", "codex"]),
                true,
                WIRE_TARGET,
            ),
            p("enabled", PType::Bool, true, WIRE_ENABLED),
        ],
        min_one: false,
    },
];

/// The MCP catalog: `[{ name, description, inputSchema }]`, generated from `TOOLS`.
pub fn catalog() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": input_schema(t) }))
            .collect(),
    )
}

/// The app/UI catalog: `[{ name, description, params: [...] }]` with the params as an
/// ORDERED ARRAY (authored order), generated from `TOOLS`. The SwiftUI Tools window
/// renders this directly so argument order is the authored order — not whatever a JSON
/// object's key iteration yields.
pub fn catalog_ui() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "params": Value::Array(t.params.iter().map(param_ui).collect()),
                })
            })
            .collect(),
    )
}

/// Build one tool's JSON-Schema `inputSchema` from its params.
fn input_schema(t: &Tool) -> Value {
    let mut schema = Map::new();
    schema.insert("type".into(), json!("object"));
    if !t.params.is_empty() {
        let mut props = Map::new();
        let mut required = Vec::new();
        for param in t.params {
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

/// One param's JSON-Schema property object (for `inputSchema.properties`).
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

/// One param's UI object (for the ordered `params` array): the raw type + constraints the
/// Tools window needs to render a name/type/required line and a detail (enum / range).
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
            // "auto" or a 0.5–20 multiplier — show the numeric range as the detail.
            o.insert("type".into(), json!("number"));
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
    fn catalog_is_a_nonempty_array_of_named_tools() {
        let c = catalog();
        let arr = c.as_array().expect("catalog is a JSON array");
        assert_eq!(arr.len(), 12, "expected 12 tools");
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
        }
    }

    /// The UI catalog mirrors the MCP catalog tool-for-tool, but carries params as an
    /// ORDERED array — the authored order, which is the whole point (the MCP inputSchema's
    /// `properties` object can't convey order).
    #[test]
    fn catalog_ui_params_are_ordered() {
        let ui = catalog_ui();
        let arr = ui.as_array().expect("ui catalog is an array");
        assert_eq!(arr.len(), 12, "same 12 tools as the MCP catalog");

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
            DiarizerProvider, DropSpeechKind, Provider, SttEngine, TrayKind, TtsEngine,
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
        // All these fields are SET / ladder arrays now → tokens live at `items.enum`.
        let schema_item_enum = |field: &str| -> Vec<String> {
            props[field]["items"]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{field} should have an items.enum array"))
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect()
        };

        // tts_engine / stt_engine are now preference-ladder ARRAYS (tokens at items.enum),
        // like provider / diarizer_provider. `Enum::ALL` (incl. `off`) stays the single source.
        assert_eq!(
            schema_item_enum("tts_engine"),
            toks(TtsEngine::ALL, TtsEngine::as_str)
        );
        assert_eq!(
            schema_item_enum("stt_engine"),
            toks(SttEngine::ALL, SttEngine::as_str)
        );
        assert_eq!(
            schema_item_enum("provider"),
            toks(Provider::ALL, Provider::as_str)
        );
        assert_eq!(
            schema_item_enum("diarizer_provider"),
            toks(DiarizerProvider::ALL, DiarizerProvider::as_str)
        );
        assert_eq!(
            schema_item_enum("tray_indicator"),
            toks(TrayKind::ALL, TrayKind::as_str)
        );
        assert_eq!(
            schema_item_enum("drop_speech_on"),
            toks(DropSpeechKind::ALL, DropSpeechKind::as_str)
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
    /// `ds_config::SetConfigArgs` — the struct the handler deserializes into — by NAME and
    /// declared TYPE. The fully-populated literal is exhaustive (no `..`), so a NEW struct
    /// field breaks this at COMPILE time; the names come from serde, so this can't go stale.
    #[test]
    fn set_config_schema_matches_args() {
        use ds_config::{
            CaptureGain, DiarizerProvider, DropSpeechKind, Provider, SetConfigArgs, SttEngine,
            TrayKind, TtsEngine,
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
            narrate: Some(vec![ds_config::NarrateKind::Digests]),
            caps_enabled: Some(true),
            greet_on_open: Some(true),
            tray_indicator: Some(vec![TrayKind::Stt, TrayKind::Tts]),
            capture_gain: Some(CaptureGain::Manual(2.0)),
            auto_submit: Some(false),
            drop_speech_on: Some(vec![DropSpeechKind::Voice, DropSpeechKind::Text]),
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

        // Name parity.
        let mut schema_keys: Vec<&String> = props.keys().collect();
        let mut struct_keys: Vec<&String> = fields.keys().collect();
        schema_keys.sort();
        struct_keys.sort();
        assert_eq!(
            schema_keys, struct_keys,
            "set_config inputSchema properties and SetConfigArgs fields are out of sync"
        );

        // Type parity, for every property declaring a scalar `type`. `capture_gain` uses
        // `oneOf` (no top-level `type`), so it is name-checked only.
        for (name, prop) in props {
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
