//! Typed `model_status` schema — single source for the engine → app status contract.
//!
//! Engine (`dontspeakd::status`) builds [`ModelStatus`]; `ds_core` ships the JSON; platform
//! UIs hand-mirror the shape (winui `Native.cs`, macOS Swift). Round-trip test pins wire
//! byte-shape (no codegen for a ~20-fn surface).
//!
//! serde field names are wire keys. `Option<String>` → JSON `null` (never omitted); apps
//! read every key unconditionally.

mod dictation_state;
mod selection;
mod state;
mod tray;
pub use dictation_state::DictationState;
pub use selection::{ActiveSttSlot, ActiveTtsSlot};
pub use state::EngineState;
pub use tray::{TrayIconKind, tray_icon_kind};

/// f64 → JSON number; NaN/Infinity become 0.0. Default serde_json would emit `null`, which
/// violates this numeric wire contract and breaks apps' non-optional float DTOs.
mod finite_f64_or_zero {
    pub fn serialize<S: serde::Serializer>(value: &f64, ser: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&sanitize(*value), ser)
    }

    fn sanitize(v: f64) -> f64 {
        if v.is_finite() { v } else { 0.0 }
    }
}

/// One engine row. `state` is an [`EngineState`] wire token (status-dot mapping).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EngineObj {
    pub present: bool,
    pub removable: bool,
    pub state: String,
    /// Download fraction 0..1, byte-weighted across the whole model set (not per-file).
    /// `0.0` unless `downloading`.
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub progress: f64,
    pub error: Option<String>,
}

/// Flat "running" map for MCP `status`/`model_status`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Dictation confirm-panel fields (booleans + canonical [`DictationState`] token).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dictation {
    pub recording: bool,
    pub awaiting_confirm: bool,
    pub text: String,
    pub target: Option<String>,
    pub local_stt: bool,
    pub has_paste_target: bool,
    pub prompt_glow: bool,
    /// START refused (engine can't transcribe yet). Short window after Caps tap; same warning
    /// glow as `has_paste_target == false`. `#[serde(default)]` for older engines → false.
    #[serde(default)]
    pub refused: bool,
    /// [`DictationState`] token; panel shown when not `"hidden"`. Absent key → `""` (legacy
    /// boolean fallback). `#[serde(default)]` for older engines.
    #[serde(default)]
    pub state: String,
}

/// Models resident in the warm helper.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Loaded {
    pub tts: bool,
    pub stt: bool,
}

/// Diarization stats (Settings expansion).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiarStats {
    pub enabled: bool,
    pub present: bool,
    pub runtime: String,
    pub speakers: Vec<String>,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub clustering_threshold: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub speaker_threshold: f64,
}

/// Live TTS RTF / TTFA stats (`stats.tts`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TtsSnapshot {
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_avg: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_min: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_max: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub first_avg_ms: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub first_min_ms: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub first_max_ms: f64,
    pub utterances: u64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub audio_secs: f64,
    pub failures: u64,
}

/// Live STT RTF stats (`stats.stt`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SttSnapshot {
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_avg: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_min: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_max: f64,
    pub transcriptions: u64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub audio_secs: f64,
    pub failures: u64,
}

/// Lifetime usage totals (`stats.lifetime`): whole seconds spoken + heard.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LifetimeSnapshot {
    pub tts_secs: u64,
    pub stt_secs: u64,
}

/// `stats` sub-object (RTF, lifetime, loaded models, diarization).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub tts: TtsSnapshot,
    pub stt: SttSnapshot,
    pub lifetime: LifetimeSnapshot,
    pub loaded: Loaded,
    pub diarization: DiarStats,
}

/// Caps-trigger event for the live status panel. `kind`: press/release/start/stop/reset.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CapsEvent {
    pub ts: u64,
    pub kind: String,
}

/// Full `model_status` payload — engine → app status contract.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelStatus {
    pub kokoro: EngineObj,
    pub parakeet: EngineObj,
    pub diarization: EngineObj,
    pub system: EngineObj,
    pub claude_code: EngineObj,
    pub tts_system: EngineObj,
    pub stt_engine: String,
    /// `null` for system/claude_code engines.
    pub stt_provider: Option<String>,
    pub tts_engine: String,
    /// `null` for system (`say`) / off engines.
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
                refused: false,
                state: DictationState::Hidden.as_str().to_string(),
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

    /// Pin wire byte-shape: nullable fields → `null` (never omitted), stats nested, round-trip.
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
        assert!(v["dictation"]["state"].is_string(), "dictation.state");
        assert!(v["seq"].is_u64());
        assert!(v["stats"]["tts"]["rtf_avg"].is_f64());
        assert!(v["stats"]["stt"]["transcriptions"].is_u64());
        assert!(v["stats"]["lifetime"]["tts_secs"].is_u64());
        assert!(v["stats"]["diarization"]["speakers"].is_array());
        assert!(v["caps_events"][0]["kind"].is_string());

        // Additive fields: absent key must still parse (`#[serde(default)]`).
        let mut old = v.clone();
        old["dictation"].as_object_mut().unwrap().remove("refused");
        let old: ModelStatus = serde_json::from_value(old).unwrap();
        assert!(
            !old.dictation.refused,
            "absent dictation.refused reads false"
        );

        let mut old = v.clone();
        old["dictation"].as_object_mut().unwrap().remove("state");
        let old: ModelStatus = serde_json::from_value(old).unwrap();
        assert!(
            old.dictation.state.is_empty(),
            "absent dictation.state reads \"\""
        );

        let back: ModelStatus = serde_json::from_value(v).unwrap();
        assert_eq!(back.stt_engine, "built_in");
        assert!(back.stt_provider.is_none());
        assert_eq!(back.caps_events.len(), 1);
    }

    #[test]
    fn non_finite_numbers_preserve_numeric_wire_shape() {
        let mut s = sample();
        s.stats.tts.rtf_avg = f64::NAN;
        s.stats.tts.rtf_min = f64::NEG_INFINITY;
        s.stats.tts.audio_secs = f64::INFINITY;
        s.stats.stt.rtf_max = f64::NAN;
        s.kokoro.progress = f64::INFINITY;
        // Guarded fields stay numeric (serde_json would emit null for non-finite).
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["stats"]["tts"]["rtf_avg"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["tts"]["rtf_min"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["tts"]["audio_secs"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["stt"]["rtf_max"].as_f64().unwrap(), 0.0);
        assert_eq!(v["kokoro"]["progress"].as_f64().unwrap(), 0.0);
    }

    // Property tests: same null-not-omitted + round-trip as above, over many values.
    // Strategies stay finite (serde_json can't represent NaN/Infinity).
    use proptest::prelude::*;

    fn finite_f64() -> impl Strategy<Value = f64> {
        -1.0e6..1.0e6
    }

    fn unit_f64() -> impl Strategy<Value = f64> {
        0.0..=1.0
    }

    fn short_string() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_ -]{0,16}"
    }

    fn opt_short_string() -> impl Strategy<Value = Option<String>> {
        prop::option::of(short_string())
    }

    fn short_string_vec() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(short_string(), 0..4)
    }

    prop_compose! {
        fn engine_obj_strategy()(
            present in any::<bool>(),
            removable in any::<bool>(),
            state in short_string(),
            progress in unit_f64(),
            error in opt_short_string(),
        ) -> EngineObj {
            EngineObj { present, removable, state, progress, error }
        }
    }

    prop_compose! {
        fn running_strategy()(
            caps in any::<bool>(),
            caps_wanted in any::<bool>(),
            stt_active in any::<bool>(),
            tts_active in any::<bool>(),
            muted in any::<bool>(),
            kokoro in any::<bool>(),
            tts_system in any::<bool>(),
            parakeet in any::<bool>(),
            system in any::<bool>(),
            claude_code in any::<bool>(),
        ) -> Running {
            Running {
                caps,
                caps_wanted,
                stt_active,
                tts_active,
                muted,
                kokoro,
                tts_system,
                parakeet,
                system,
                claude_code,
            }
        }
    }

    prop_compose! {
        fn dictation_strategy()(
            recording in any::<bool>(),
            awaiting_confirm in any::<bool>(),
            text in short_string(),
            target in opt_short_string(),
            local_stt in any::<bool>(),
            has_paste_target in any::<bool>(),
            prompt_glow in any::<bool>(),
            refused in any::<bool>(),
            state in short_string(),
        ) -> Dictation {
            Dictation {
                recording,
                awaiting_confirm,
                text,
                target,
                local_stt,
                has_paste_target,
                prompt_glow,
                refused,
                state,
            }
        }
    }

    prop_compose! {
        fn loaded_strategy()(tts in any::<bool>(), stt in any::<bool>()) -> Loaded {
            Loaded { tts, stt }
        }
    }

    prop_compose! {
        fn diar_stats_strategy()(
            enabled in any::<bool>(),
            present in any::<bool>(),
            runtime in short_string(),
            speakers in short_string_vec(),
            clustering_threshold in unit_f64(),
            speaker_threshold in unit_f64(),
        ) -> DiarStats {
            DiarStats {
                enabled,
                present,
                runtime,
                speakers,
                clustering_threshold,
                speaker_threshold,
            }
        }
    }

    prop_compose! {
        fn tts_snapshot_strategy()(
            rtf_avg in finite_f64(),
            rtf_min in finite_f64(),
            rtf_max in finite_f64(),
            first_avg_ms in finite_f64(),
            first_min_ms in finite_f64(),
            first_max_ms in finite_f64(),
            utterances in any::<u64>(),
            audio_secs in finite_f64(),
            failures in any::<u64>(),
        ) -> TtsSnapshot {
            TtsSnapshot {
                rtf_avg,
                rtf_min,
                rtf_max,
                first_avg_ms,
                first_min_ms,
                first_max_ms,
                utterances,
                audio_secs,
                failures,
            }
        }
    }

    prop_compose! {
        fn stt_snapshot_strategy()(
            rtf_avg in finite_f64(),
            rtf_min in finite_f64(),
            rtf_max in finite_f64(),
            transcriptions in any::<u64>(),
            audio_secs in finite_f64(),
            failures in any::<u64>(),
        ) -> SttSnapshot {
            SttSnapshot {
                rtf_avg,
                rtf_min,
                rtf_max,
                transcriptions,
                audio_secs,
                failures,
            }
        }
    }

    prop_compose! {
        fn lifetime_snapshot_strategy()(
            tts_secs in any::<u64>(),
            stt_secs in any::<u64>(),
        ) -> LifetimeSnapshot {
            LifetimeSnapshot { tts_secs, stt_secs }
        }
    }

    prop_compose! {
        fn stats_strategy()(
            tts in tts_snapshot_strategy(),
            stt in stt_snapshot_strategy(),
            lifetime in lifetime_snapshot_strategy(),
            loaded in loaded_strategy(),
            diarization in diar_stats_strategy(),
        ) -> Stats {
            Stats {
                tts,
                stt,
                lifetime,
                loaded,
                diarization,
            }
        }
    }

    prop_compose! {
        fn caps_event_strategy()(
            ts in any::<u64>(),
            kind in short_string(),
        ) -> CapsEvent {
            CapsEvent { ts, kind }
        }
    }

    prop_compose! {
        fn model_status_strategy()(
            kokoro in engine_obj_strategy(),
            parakeet in engine_obj_strategy(),
            diarization in engine_obj_strategy(),
            system in engine_obj_strategy(),
            claude_code in engine_obj_strategy(),
            tts_system in engine_obj_strategy(),
            stt_engine in short_string(),
            stt_provider in opt_short_string(),
            tts_engine in short_string(),
            tts_provider in opt_short_string(),
            claude_code_key in opt_short_string(),
            running in running_strategy(),
            dictation in dictation_strategy(),
            tray_indicator in short_string_vec(),
            stats in stats_strategy(),
            caps_events in prop::collection::vec(caps_event_strategy(), 0..4),
            build_id in short_string(),
            seq in any::<u64>(),
        ) -> ModelStatus {
            ModelStatus {
                kokoro,
                parakeet,
                diarization,
                system,
                claude_code,
                tts_system,
                stt_engine,
                stt_provider,
                tts_engine,
                tts_provider,
                claude_code_key,
                running,
                dictation,
                tray_indicator,
                stats,
                caps_events,
                build_id,
                seq,
            }
        }
    }

    proptest! {
        /// Same wire contract as `json_contract_round_trips`, over generated values.
        #[test]
        fn json_contract_round_trips_arbitrary_values(status in model_status_strategy()) {
            let v = serde_json::to_value(status.clone()).unwrap();

            for eng in [
                "kokoro",
                "parakeet",
                "diarization",
                "system",
                "claude_code",
                "tts_system",
            ] {
                prop_assert!(v[eng]["state"].is_string(), "{eng}.state");
                prop_assert!(v[eng].get("error").is_some(), "{eng}.error present");
            }
            prop_assert!(v.get("stt_provider").is_some(), "stt_provider present");
            prop_assert!(v.get("tts_provider").is_some(), "tts_provider present");
            prop_assert!(v.get("claude_code_key").is_some(), "claude_code_key present");
            prop_assert!(
                v["dictation"].get("target").is_some(),
                "dictation.target present"
            );
            prop_assert!(v["seq"].is_u64());
            prop_assert!(v["stats"]["tts"]["rtf_avg"].is_f64());
            prop_assert!(v["stats"]["stt"]["transcriptions"].is_u64());
            prop_assert!(v["stats"]["lifetime"]["tts_secs"].is_u64());
            prop_assert!(v["stats"]["diarization"]["speakers"].is_array());

            let back: ModelStatus = serde_json::from_value(v).unwrap();
            prop_assert_eq!(back, status);
        }
    }
}
