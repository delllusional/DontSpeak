//! The typed `model_status` schema — THE single source of truth for the engine → app
//! status contract.
//!
//! The engine (`dontspeakd::status`) BUILDS a [`ModelStatus`] and serializes it to the
//! `model_status` JSON. The C ABI (`ds_core`) ships that JSON to each platform's UI,
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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Dictation confirm-panel state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dictation {
    pub recording: bool,
    pub awaiting_confirm: bool,
    pub text: String,
    /// `null` when no paste target was captured.
    pub target: Option<String>,
    pub local_stt: bool,
    pub has_paste_target: bool,
    pub prompt_glow: bool,
    /// A dictation START was just REFUSED because the selected engine can't transcribe yet
    /// (model missing / still downloading / warm helper loading). True for a short window
    /// after the refused Caps tap; every overlay shows the panel washed in the same warning
    /// glow as `has_paste_target == false` — the shared "this didn't work" cue. `default`
    /// so a payload from an older engine still parses (reads as false).
    #[serde(default)]
    pub refused: bool,
}

/// Which models are currently resident in the warm helper.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Loaded {
    pub tts: bool,
    pub stt: bool,
}

/// Diarization stats for the Settings row's expansion.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiarStats {
    pub enabled: bool,
    pub present: bool,
    pub runtime: String,
    pub speakers: Vec<String>,
    pub clustering_threshold: f64,
    pub speaker_threshold: f64,
}

/// Live TTS realtime-factor / time-to-first-audio stats (`stats.tts`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LifetimeSnapshot {
    pub tts_secs: u64,
    pub stt_secs: u64,
}

/// The `stats` sub-object: TTS/STT realtime factors, lifetime totals, which models are
/// resident in the warm helper, and diarization settings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub tts: TtsSnapshot,
    pub stt: SttSnapshot,
    pub lifetime: LifetimeSnapshot,
    pub loaded: Loaded,
    pub diarization: DiarStats,
}

/// A single caps-trigger event for the app's live status panel. `kind` is a stable
/// machine token: "press" / "release" / "start" / "stop" / "reset".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CapsEvent {
    pub ts: u64,
    pub kind: String,
}

/// The full `model_status` payload — the engine → app status contract.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
                refused: false,
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

        // `dictation.refused` was added AFTER the first release: a payload from an older
        // engine omits the key, so it must still parse (the `#[serde(default)]` contract)
        // and read as false — the fail-quiet direction (no spurious refusal glow).
        let mut old = v.clone();
        old["dictation"].as_object_mut().unwrap().remove("refused");
        let old: ModelStatus = serde_json::from_value(old).unwrap();
        assert!(
            !old.dictation.refused,
            "absent dictation.refused reads false"
        );

        // A deserialize off the same bytes reconstructs the value (the FFI path).
        let back: ModelStatus = serde_json::from_value(v).unwrap();
        assert_eq!(back.stt_engine, "built_in");
        assert!(back.stt_provider.is_none());
        assert_eq!(back.caps_events.len(), 1);
    }

    // ── Property-based round-trip over the generated domain ─────────────────────
    //
    // `sample()`/`json_contract_round_trips` above pin ONE hand-picked value. The
    // strategies below generate many more `ModelStatus` values (bounded so
    // `serde_json` never sees a NaN/Infinity float — a JSON-representability limit,
    // not a real wire-contract gap) and assert the same round-trip + null-not-omitted
    // shape holds across that domain.
    use proptest::prelude::*;

    /// Numeric range wide enough to exercise real values (negative RTF deltas, large
    /// counters as floats, etc.) while staying finite for `serde_json`.
    fn finite_f64() -> impl Strategy<Value = f64> {
        -1.0e6..1.0e6
    }

    /// `progress`/threshold-shaped fields are documented fractions.
    fn unit_f64() -> impl Strategy<Value = f64> {
        0.0..=1.0
    }

    /// Short bounded strings — enough alphabet to catch encoding edge cases without
    /// making each generated case slow.
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
        /// Same byte-shape + round-trip contract as `json_contract_round_trips`, but
        /// over the generated domain above instead of one hand-picked sample.
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
                // Present unconditionally (null or string), never omitted.
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

            // A deserialize off the same bytes reconstructs the value (the FFI path).
            let back: ModelStatus = serde_json::from_value(v).unwrap();
            prop_assert_eq!(back, status);
        }
    }
}
