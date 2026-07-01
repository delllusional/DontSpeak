//! The typed `model_status` schema — THE single source of truth for the engine → app
//! status contract.
//!
//! The engine ([`dontspeakd::status`]) BUILDS a [`ModelStatus`] and serializes it to the
//! `model_status` JSON. The C ABI ([`ds_core`]) ships that JSON to each platform's UI,
//! which deserializes it into ITS OWN hand-written DTOs (winui `Native.cs`, macOS) that mirror
//! THIS shape. So the Rust side has one definition; the per-platform mirrors are hand-kept in
//! lockstep with it (reviewed against this file), with the round-trip contract test below
//! pinning the wire byte-shape — a deliberately small, dependency-free boundary for a
//! ~20-function surface, instead of a codegen toolchain.
//!
//! serde field names ARE the wire keys. `Option<String>` serializes to JSON `null`
//! (never omitted): the apps read every key unconditionally.

mod state;
pub use state::EngineState;

/// One engine row (Kokoro / Parakeet / diarization / system / claude_code /
/// tts_system). `state` is the lifecycle token the app maps 1:1 to a status dot; its
/// canonical vocabulary is [`EngineState`] (the producer stores `EngineState::as_str`
/// here, Rust consumers route the token back through [`EngineState::parse`]).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct EngineObj {
    pub present: bool,
    pub removable: bool,
    pub state: String,
    /// Overall download fraction 0..1 — byte-weighted across the WHOLE model set (a single
    /// global percent, NOT per-file). `0.0` unless the row is `downloading`.
    pub progress: f64,
    /// `null` when there is no error.
    pub error: Option<String>,
}

/// The flat "running" map the MCP `status`/`model_status` tools read.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Running {
    pub caps: bool,
    pub caps_wanted: bool,
    pub stt_active: bool,
    pub tts_active: bool,
    pub muted: bool,
    pub kokoro: bool,
    pub tts_system: bool,
    pub parakeet: bool,
    pub system: bool,
    pub claude_code: bool,
}

/// Dictation confirm-panel state.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Dictation {
    pub recording: bool,
    pub awaiting_confirm: bool,
    pub text: String,
    /// `null` when no paste target was captured.
    pub target: Option<String>,
    pub local_stt: bool,
    pub has_paste_target: bool,
    pub prompt_glow: bool,
}

/// Which models are currently resident in the warm helper.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Loaded {
    pub tts: bool,
    pub stt: bool,
}

/// Diarization stats for the Settings row's expansion.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DiarStats {
    pub enabled: bool,
    pub present: bool,
    pub runtime: String,
    pub speakers: Vec<String>,
    pub clustering_threshold: f64,
    pub speaker_threshold: f64,
}

/// Live TTS realtime-factor / time-to-first-audio stats (`stats.tts`).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TtsSnapshot {
    pub rtf_avg: f64,
    pub rtf_min: f64,
    pub rtf_max: f64,
    pub first_avg_ms: f64,
    pub first_min_ms: f64,
    pub first_max_ms: f64,
    pub utterances: u64,
    pub audio_secs: f64,
    pub failures: u64,
}

/// Live Parakeet STT realtime-factor stats (`stats.stt`).
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SttSnapshot {
    pub rtf_avg: f64,
    pub rtf_min: f64,
    pub rtf_max: f64,
    pub transcriptions: u64,
    pub audio_secs: f64,
    pub failures: u64,
}

/// Persisted lifetime usage totals (`stats.lifetime`): whole seconds spoken + heard,
/// summed across every session.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct LifetimeSnapshot {
    pub tts_secs: u64,
    pub stt_secs: u64,
}

/// The `stats` sub-object: TTS/STT realtime factors, lifetime totals, which models are
/// resident in the warm helper, and diarization settings.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub tts: TtsSnapshot,
    pub stt: SttSnapshot,
    pub lifetime: LifetimeSnapshot,
    pub loaded: Loaded,
    pub diarization: DiarStats,
}

/// A single caps-trigger event for the app's live status panel. `kind` is a stable
/// machine token: "press" / "release" / "start" / "stop" / "reset".
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CapsEvent {
    pub ts: u64,
    pub kind: String,
}

/// The full `model_status` payload — the engine → app status contract.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelStatus {
    pub kokoro: EngineObj,
    pub parakeet: EngineObj,
    pub diarization: EngineObj,
    pub system: EngineObj,
    pub claude_code: EngineObj,
    pub tts_system: EngineObj,
    pub stt_engine: String,
    /// `null` for the system/claude_code engines.
    pub stt_provider: Option<String>,
    pub tts_engine: String,
    /// `null` for the system (`say`) / off engines.
    pub tts_provider: Option<String>,
    /// `null` unless claude_code is selected and usable.
    pub claude_code_key: Option<String>,
    pub running: Running,
    pub dictation: Dictation,
    pub tray_indicator: Vec<String>,
    pub stats: Stats,
    pub caps_events: Vec<CapsEvent>,
    pub build_id: String,
    pub seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_none() -> EngineObj {
        EngineObj {
            present: false,
            removable: false,
            state: EngineState::Missing.as_str().to_string(),
            progress: 0.0,
            error: None,
        }
    }

    fn sample() -> ModelStatus {
        ModelStatus {
            kokoro: engine_none(),
            parakeet: engine_none(),
            diarization: engine_none(),
            system: engine_none(),
            claude_code: engine_none(),
            tts_system: engine_none(),
            stt_engine: "built_in".to_string(),
            stt_provider: None,
            tts_engine: "system".to_string(),
            tts_provider: None,
            claude_code_key: None,
            running: Running {
                caps: false,
                caps_wanted: false,
                stt_active: false,
                tts_active: false,
                muted: false,
                kokoro: false,
                tts_system: true,
                parakeet: false,
                system: false,
                claude_code: false,
            },
            dictation: Dictation {
                recording: false,
                awaiting_confirm: false,
                text: String::new(),
                target: None,
                local_stt: false,
                has_paste_target: true,
                prompt_glow: false,
            },
            tray_indicator: vec!["stt".to_string(), "tts".to_string()],
            stats: Stats {
                tts: TtsSnapshot::default(),
                stt: SttSnapshot::default(),
                lifetime: LifetimeSnapshot::default(),
                loaded: Loaded {
                    tts: false,
                    stt: false,
                },
                diarization: DiarStats {
                    enabled: false,
                    present: false,
                    runtime: "ane".to_string(),
                    speakers: vec![],
                    clustering_threshold: 0.7,
                    speaker_threshold: 0.5,
                },
            },
            caps_events: vec![CapsEvent {
                ts: 1,
                kind: "press".to_string(),
            }],
            build_id: "test".to_string(),
            seq: 0,
        }
    }

    /// Round-trip the schema through JSON and assert the byte-shape: every nullable
    /// field serializes to `null` (never omitted — the apps read keys unconditionally),
    /// the stats nest under `stats`, and a deserialize reconstructs an equal value.
    /// Guards the wire contract against drift now that there is ONE definition.
    #[test]
    fn json_contract_round_trips() {
        let v = serde_json::to_value(sample()).unwrap();

        for eng in [
            "kokoro",
            "parakeet",
            "diarization",
            "system",
            "claude_code",
            "tts_system",
        ] {
            assert!(v[eng]["state"].is_string(), "{eng}.state");
            assert!(v[eng]["error"].is_null(), "{eng}.error null when None");
        }
        assert!(v["stt_provider"].is_null(), "stt_provider null when None");
        assert!(v["tts_provider"].is_null(), "tts_provider null when None");
        assert!(
            v["claude_code_key"].is_null(),
            "claude_code_key null when None"
        );
        assert!(
            v["dictation"]["target"].is_null(),
            "dictation.target null when None"
        );
        assert!(v["seq"].is_u64());
        assert!(v["stats"]["tts"]["rtf_avg"].is_f64());
        assert!(v["stats"]["stt"]["transcriptions"].is_u64());
        assert!(v["stats"]["lifetime"]["tts_secs"].is_u64());
        assert!(v["stats"]["diarization"]["speakers"].is_array());
        assert!(v["caps_events"][0]["kind"].is_string());

        // A deserialize off the same bytes reconstructs the value (the FFI path).
        let back: ModelStatus = serde_json::from_value(v).unwrap();
        assert_eq!(back.stt_engine, "built_in");
        assert!(back.stt_provider.is_none());
        assert_eq!(back.caps_events.len(), 1);
    }
}
