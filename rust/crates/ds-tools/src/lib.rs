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
/// implemented but not ready for general users yet — flip to `true` when it's tested
/// enough to ship. The ONE toggle that hides it from every user-facing surface (MCP
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
/// from, and the exact order the Tools window shows. Ordered to lead with the two core
/// actions (speak · listen) so the highest-frequency tools sit first (primacy), then the
/// output-control pair (stop_speech · mute), then read-only introspection (get_status · list_voices), then
/// speaker diarization (diarize · manage_speakers — the voiceprint library it labels with),
/// and finally the rare admin tools (set_config, then the one-time client wiring
/// setup_integration) in the low-attention tail.
static TOOLS: &[Tool] = &[
    // Core action: say something.
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
    // Core action: hear something back (dictation).
    Tool {
        name: "listen",
        description: LISTEN,
        params: &[p("seconds", PType::Int(1, 60), false, LISTEN_SECONDS)],
        min_one: false,
    },
    // Interrupt spoken output.
    Tool {
        name: "stop_speech",
        description: STOP_SPEECH,
        params: &[],
        min_one: false,
    },
    // Persistent silence toggle for all spoken output (the global mute the app also drives).
    Tool {
        name: "mute",
        description: MUTE,
        params: &[p("on", PType::Bool, true, MUTE_ON)],
        min_one: false,
    },
    // Read-only introspection: current runtime state, then the voices replies can use (the
    // voice itself is a persistent setting — see `set_config`'s tts_built_in_voices).
    Tool {
        name: "get_status",
        description: GET_STATUS,
        params: &[p("detail", PType::Bool, false, STATUS_DETAIL)],
        min_one: false,
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
    },
    // ── Speaker diarization (who spoke when) + voiceprint enrollment ──
    Tool {
        name: "diarize",
        description: DIARIZE,
        params: &[p("seconds", PType::Int(1, 60), false, DIARIZE_SECONDS)],
        min_one: false,
    },
    // Manage the enrolled-voiceprint library that diarize uses to put names to speakers:
    // one action-dispatched tool (list / enroll / forget) instead of three.
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
        name: "setup_integration",
        description: SETUP_INTEGRATION,
        params: &[
            p(
                "target",
                PType::Enum(&["narration_spec", "claude_code", "codex", "qwen_code", "grok"]),
                true,
                WIRE_TARGET,
            ),
            p("enabled", PType::Bool, true, WIRE_ENABLED),
        ],
        min_one: false,
    },
];

/// Whether `t` should appear on user-facing surfaces (tools/list, catalog_ui, the schema)
/// given the current `DIARIZATION_ENABLED` gate.
fn is_visible(t: &Tool) -> bool {
    DIARIZATION_ENABLED || !HIDDEN_TOOLS.contains(&t.name)
}

/// `t`'s params filtered to what's visible on user-facing surfaces: everything, unless a
/// param is one of the hidden diarization knobs and the gate is off.
fn visible_params(t: &Tool) -> Vec<&Param> {
    t.params
        .iter()
        .filter(|p| DIARIZATION_ENABLED || !HIDDEN_SET_CONFIG_PARAMS.contains(&p.name))
        .collect()
}

/// The canonical tool names, in catalog (display) order. The single accessor the MCP
/// dispatch router pins itself against (see the `router_handles_every_catalog_tool` drift
/// test in `dontspeak::tools`) so a tool added/renamed here can't silently go unrouted.
pub fn tool_names() -> impl Iterator<Item = &'static str> {
    TOOLS.iter().filter(|t| is_visible(t)).map(|t| t.name)
}

/// The MCP catalog: `[{ name, description, inputSchema }]`, generated from `TOOLS`.
pub fn catalog() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .filter(|t| is_visible(t))
            .map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": input_schema(t) }))
            .collect(),
    )
}

/// The RAW `inputSchema` for one tool by name, looked up directly in `TOOLS` — ignoring
/// `DIARIZATION_ENABLED`/`HIDDEN_TOOLS` entirely. Exists so callers that need to verify
/// schema/dispatch parity for a tool REGARDLESS of whether it's currently hidden from
/// user-facing surfaces (e.g. the `diarize`/`manage_speakers` regression coverage in
/// `dontspeak::tools`) don't have to go through the filtered `catalog()`.
pub fn raw_input_schema(name: &str) -> Option<Value> {
    TOOLS.iter().find(|t| t.name == name).map(input_schema)
}

/// The app/UI catalog: `[{ name, description, params: [...] }]` with the params as an
/// ORDERED ARRAY (authored order), generated from `TOOLS`. The SwiftUI Tools window
/// renders this directly so argument order is the authored order — not whatever a JSON
/// object's key iteration yields.
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

/// Build one tool's JSON-Schema `inputSchema` from its params.
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

    #[test]
    fn catalog_is_a_nonempty_array_of_named_tools() {
        let c = catalog();
        let arr = c.as_array().expect("catalog is a JSON array");
        let expected = if DIARIZATION_ENABLED { 10 } else { 8 };
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
        }
    }

    /// The UI catalog mirrors the MCP catalog tool-for-tool, but carries params as an
    /// ORDERED array — the authored order, which is the whole point (the MCP inputSchema's
    /// `properties` object can't convey order).
    #[test]
    fn catalog_ui_params_are_ordered() {
        let ui = catalog_ui();
        let arr = ui.as_array().expect("ui catalog is an array");
        let expected = if DIARIZATION_ENABLED { 10 } else { 8 };
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

    /// PARITY GUARD: the `setup_integration` tool's `target` enum must list EXACTLY the tokens
    /// of `ds_config::WireTarget` (the type the dispatch handler matches on), so the authored
    /// schema strings can't silently drift from the canonical set.
    #[test]
    fn setup_integration_target_enum_matches_config_type() {
        use ds_config::WireTarget;

        fn toks<T: Copy>(all: &[T], as_str: fn(T) -> &'static str) -> Vec<String> {
            all.iter().map(|&v| as_str(v).to_string()).collect()
        }

        let cat = catalog();
        let setup_integration = cat
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "setup_integration")
            .expect("setup_integration in catalog");
        let schema_enum: Vec<String> =
            setup_integration["inputSchema"]["properties"]["target"]["enum"]
                .as_array()
                .expect("setup_integration target has an enum array")
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
        assert_eq!(schema_enum, toks(WireTarget::ALL, WireTarget::as_str));
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
