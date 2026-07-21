//! Typed `model_status` — engine → app contract (single source).
//!
//! Engine builds [`ModelStatus`]; UIs hand-mirror closed-set tokens. Unknown enum
//! values fail closed. `Option<_>` → JSON `null` (always present).

mod dictation_state;
mod engines;
mod state;
mod tray;
mod tts_model;
pub use dictation_state::DictationState;
pub use engines::{StatusSttEngine, StatusTtsEngine};
pub use state::EngineState;
pub use tray::{StatusTrayKind, TrayIconKind, tray_icon_kind};
pub use tts_model::StatusTtsModel;

/// NaN/Infinity → 0.0 (non-optional float DTOs).
mod finite_f64_or_zero {
    pub fn serialize<S: serde::Serializer>(value: &f64, ser: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&sanitize(*value), ser)
    }

    fn sanitize(v: f64) -> f64 {
        if v.is_finite() { v } else { 0.0 }
    }
}

/// Engine row. `state` = [`EngineState`] wire token.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EngineStatus {
    pub state: EngineState,
    /// Download fraction 0..1; `0.0` unless downloading.
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub progress: f64,
    pub error: Option<String>,
}

/// One active model download. Byte totals come from the live transfer; rate is the
/// whole-target average and ETA is absent until enough progress has been observed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DownloadStatus {
    pub target: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub bytes_per_second: Option<u64>,
    pub eta_seconds: Option<u64>,
}

/// Diagnostic attached when a language-specific voice does not own the detected language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UtteranceWarning {
    VoiceLanguageMismatch,
}

/// Voice resolution for an utterance that reached playback.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UtteranceStatus {
    pub voice: String,
    pub language: String,
    pub warning: Option<UtteranceWarning>,
}

/// Selected TTS engine, its realized provider, and its lifecycle status.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TtsStatus {
    pub engine: StatusTtsEngine,
    /// Selected built-in model; `null` for system/off.
    pub model: Option<StatusTtsModel>,
    /// Resolved built-in model language; `null` for system/off.
    pub language: Option<String>,
    /// `null` for system (`say`) / off engines.
    pub provider: Option<String>,
    /// `null` when speech is off.
    pub status: Option<EngineStatus>,
    /// Most recent utterance that reached playback; retained while idle.
    pub last_utterance: Option<UtteranceStatus>,
}

/// Selected STT engine, its realized provider, and its lifecycle status.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SttStatus {
    pub engine: StatusSttEngine,
    /// `null` for system/claude_code/off engines.
    pub provider: Option<String>,
    /// `null` when dictation is off.
    pub status: Option<EngineStatus>,
    /// Bound Claude Code voice key; `null` for other engines or an unusable binding.
    pub voice_key: Option<String>,
}

/// Diarization lifecycle and UI details in one domain object.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiarizationStatus {
    pub status: EngineStatus,
    pub enabled: bool,
    pub provider: String,
    pub speakers: Vec<String>,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub activity_threshold: f64,
}

/// Live app activity; names match what hosts render instead of implementation details.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Activity {
    pub caps: bool,
    pub caps_active: bool,
    pub recording: bool,
    pub speaking: bool,
    /// Wireable client for the in-flight TTS utterance. `null` when idle or the producer
    /// is not a Usage agent (greet / unknown / DontSpeak).
    pub speaker: Option<ds_client::ClientSource>,
    /// Resolved voice for the in-flight utterance; `null` while idle.
    pub voice: Option<String>,
    /// Detected language for the in-flight utterance; `null` while idle.
    pub language: Option<String>,
    /// Per-utterance quality diagnostic; `null` while idle or when no mismatch is known.
    pub warning: Option<UtteranceWarning>,
    pub muted: bool,
}

/// Dictation confirm-panel content and canonical [`DictationState`] mode.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Dictation {
    /// Panel shown when not [`DictationState::Hidden`].
    pub state: DictationState,
    pub text: String,
    pub can_paste: bool,
}

/// Live TTS RTF / TTFA stats (`stats.tts`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TtsSnapshot {
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_min: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_avg: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_max: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub ttfa_min_ms: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub ttfa_avg_ms: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub ttfa_max_ms: f64,
    pub utterances: u64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub audio_secs: f64,
    pub failures: u64,
    /// Utterances still outstanding: those waiting plus the one being spoken. Cues are not
    /// counted, so this is "how much is left to say" — `0` exactly when speech has stopped.
    /// Instantaneous, unlike its cumulative siblings; the engine fills it from the TTS queue,
    /// not from `TtsStats::snapshot`.
    pub queued: u64,
}

/// Live STT RTF stats (`stats.stt`).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SttSnapshot {
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_min: f64,
    #[serde(serialize_with = "finite_f64_or_zero::serialize")]
    pub rtf_avg: f64,
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

/// `stats` sub-object (TTS/STT realtime and lifetime totals).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub tts: TtsSnapshot,
    pub stt: SttSnapshot,
    pub lifetime: LifetimeSnapshot,
}

/// Full `model_status` payload — engine → app status contract.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelStatus {
    /// Push sequence echoed by `WaitModelStatus` clients.
    pub seq: u64,
    pub activity: Activity,
    pub tts: TtsStatus,
    pub stt: SttStatus,
    pub diarization: DiarizationStatus,
    pub dictation: Dictation,
    pub stats: Stats,
    pub tray: Vec<StatusTrayKind>,
    /// Active transfers only, sorted by stable target token.
    pub downloads: Vec<DownloadStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_none() -> EngineStatus {
        EngineStatus {
            state: EngineState::Missing,
            progress: 0.0,
            error: None,
        }
    }

    fn sample() -> ModelStatus {
        ModelStatus {
            seq: 0,
            activity: Activity {
                caps: false,
                caps_active: false,
                recording: false,
                speaking: false,
                speaker: None,
                voice: None,
                language: None,
                warning: None,
                muted: false,
            },
            tts: TtsStatus {
                engine: StatusTtsEngine::System,
                model: None,
                language: None,
                provider: None,
                status: Some(engine_none()),
                last_utterance: None,
            },
            stt: SttStatus {
                engine: StatusSttEngine::BuiltIn,
                provider: None,
                status: Some(engine_none()),
                voice_key: None,
            },
            diarization: DiarizationStatus {
                status: engine_none(),
                enabled: false,
                provider: "mlx".to_string(),
                speakers: vec![],
                activity_threshold: 0.5,
            },
            dictation: Dictation {
                state: DictationState::Hidden,
                text: String::new(),
                can_paste: true,
            },
            stats: Stats {
                tts: TtsSnapshot::default(),
                stt: SttSnapshot::default(),
                lifetime: LifetimeSnapshot::default(),
            },
            tray: vec![StatusTrayKind::Stt, StatusTrayKind::Tts],
            downloads: vec![],
        }
    }

    /// Pin wire byte-shape: nullable fields → `null` (never omitted), stats nested, round-trip.
    #[test]
    fn json_contract_round_trips() {
        let v = serde_json::to_value(sample()).unwrap();

        let root = v.as_object().unwrap();
        assert_eq!(root.len(), 9, "no duplicated root-level engine fields");
        for key in [
            "seq",
            "activity",
            "tts",
            "stt",
            "diarization",
            "dictation",
            "stats",
            "tray",
            "downloads",
        ] {
            assert!(root.contains_key(key), "missing root field {key}");
        }
        assert_eq!(v["activity"].as_object().unwrap().len(), 9);
        assert_eq!(v["stats"]["tts"].as_object().unwrap().len(), 10);
        assert_eq!(v["stats"]["tts"]["queued"], 0);
        assert_eq!(v["tts"].as_object().unwrap().len(), 6);
        assert_eq!(v["stt"].as_object().unwrap().len(), 4);
        assert_eq!(v["diarization"].as_object().unwrap().len(), 5);
        assert_eq!(v["dictation"].as_object().unwrap().len(), 3);
        assert_eq!(v["stats"].as_object().unwrap().len(), 3);

        for path in [["tts", "status"], ["stt", "status"]] {
            assert_eq!(v[path[0]][path[1]]["state"], "missing");
            assert!(v[path[0]][path[1]]["error"].is_null());
        }
        assert_eq!(v["diarization"]["status"]["state"], "missing");
        assert_eq!(v["tts"]["engine"], "system");
        assert_eq!(v["stt"]["engine"], "built_in");
        assert!(v["tts"]["model"].is_null());
        assert!(v["tts"]["language"].is_null());
        assert!(v["tts"]["provider"].is_null());
        assert!(v["stt"]["provider"].is_null());
        assert!(v["stt"]["voice_key"].is_null());
        assert_eq!(v["dictation"]["state"], "hidden");
        assert!(
            v["activity"]["speaker"].is_null(),
            "activity.speaker null when idle"
        );
        assert!(v["activity"]["voice"].is_null());
        assert!(v["activity"]["language"].is_null());
        assert!(v["activity"]["warning"].is_null());
        assert!(v["tts"]["last_utterance"].is_null());
        assert!(v["downloads"].as_array().unwrap().is_empty());
        assert_eq!(v["tray"], serde_json::json!(["stt", "tts"]));
        assert!(v["seq"].is_u64());
        assert!(v["stats"]["tts"]["rtf_avg"].is_f64());
        assert!(v["stats"]["stt"]["transcriptions"].is_u64());
        assert!(v["stats"]["lifetime"]["tts_secs"].is_u64());
        assert!(v["diarization"]["speakers"].is_array());

        let back: ModelStatus = serde_json::from_value(v).unwrap();
        assert_eq!(back.stt.engine, StatusSttEngine::BuiltIn);
        assert_eq!(back.tts.engine, StatusTtsEngine::System);
        assert_eq!(back.dictation.state, DictationState::Hidden);
        assert!(back.stt.provider.is_none());
    }

    #[test]
    fn non_finite_numbers_preserve_numeric_wire_shape() {
        let mut s = sample();
        s.stats.tts.rtf_avg = f64::NAN;
        s.stats.tts.rtf_min = f64::NEG_INFINITY;
        s.stats.tts.audio_secs = f64::INFINITY;
        s.stats.stt.rtf_max = f64::NAN;
        s.tts.status.as_mut().unwrap().progress = f64::INFINITY;
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["stats"]["tts"]["rtf_avg"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["tts"]["rtf_min"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["tts"]["audio_secs"].as_f64().unwrap(), 0.0);
        assert_eq!(v["stats"]["stt"]["rtf_max"].as_f64().unwrap(), 0.0);
        assert_eq!(v["tts"]["status"]["progress"].as_f64().unwrap(), 0.0);
    }

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

    fn utterance_warning_strategy() -> impl Strategy<Value = UtteranceWarning> {
        Just(UtteranceWarning::VoiceLanguageMismatch)
    }

    prop_compose! {
        fn utterance_status_strategy()(
            voice in short_string(),
            language in short_string(),
            warning in prop::option::of(utterance_warning_strategy()),
        ) -> UtteranceStatus {
            UtteranceStatus { voice, language, warning }
        }
    }

    prop_compose! {
        fn download_status_strategy()(
            target in short_string(),
            done_bytes in any::<u64>(),
            total_bytes in any::<u64>(),
            bytes_per_second in prop::option::of(any::<u64>()),
            eta_seconds in prop::option::of(any::<u64>()),
        ) -> DownloadStatus {
            DownloadStatus {
                target,
                done_bytes,
                total_bytes,
                bytes_per_second,
                eta_seconds,
            }
        }
    }

    fn short_string_vec() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec(short_string(), 0..4)
    }

    fn engine_state_strategy() -> impl Strategy<Value = EngineState> {
        prop::sample::select(EngineState::ALL.to_vec())
    }

    fn status_stt_strategy() -> impl Strategy<Value = StatusSttEngine> {
        prop::sample::select(StatusSttEngine::ALL.to_vec())
    }

    fn status_tts_strategy() -> impl Strategy<Value = StatusTtsEngine> {
        prop::sample::select(StatusTtsEngine::ALL.to_vec())
    }

    fn status_tts_model_strategy() -> impl Strategy<Value = StatusTtsModel> {
        prop::sample::select(StatusTtsModel::ALL.to_vec())
    }

    fn dictation_state_strategy() -> impl Strategy<Value = DictationState> {
        prop::sample::select(DictationState::ALL.to_vec())
    }

    fn tray_kind_strategy() -> impl Strategy<Value = StatusTrayKind> {
        prop::sample::select(StatusTrayKind::ALL.to_vec())
    }

    fn client_source_strategy() -> impl Strategy<Value = ds_client::ClientSource> {
        prop::sample::select(vec![
            ds_client::ClientSource::ClaudeCode,
            ds_client::ClientSource::Codex,
            ds_client::ClientSource::QwenCode,
            ds_client::ClientSource::Grok,
            ds_client::ClientSource::KimiCode,
            ds_client::ClientSource::Hermes,
            ds_client::ClientSource::DontSpeak,
            ds_client::ClientSource::Unknown,
        ])
    }

    prop_compose! {
        fn engine_status_strategy()(
            state in engine_state_strategy(),
            progress in unit_f64(),
            error in opt_short_string(),
        ) -> EngineStatus {
            EngineStatus { state, progress, error }
        }
    }

    prop_compose! {
        fn activity_strategy()(
            caps in any::<bool>(),
            caps_active in any::<bool>(),
            recording in any::<bool>(),
            speaking in any::<bool>(),
            speaker in prop::option::of(client_source_strategy()),
            voice in opt_short_string(),
            language in opt_short_string(),
            warning in prop::option::of(utterance_warning_strategy()),
            muted in any::<bool>(),
        ) -> Activity {
            Activity {
                caps,
                caps_active,
                recording,
                speaking,
                speaker,
                voice,
                language,
                warning,
                muted,
            }
        }
    }

    prop_compose! {
        fn dictation_strategy()(
            state in dictation_state_strategy(),
            text in short_string(),
            can_paste in any::<bool>(),
        ) -> Dictation {
            Dictation {
                state,
                text,
                can_paste,
            }
        }
    }

    prop_compose! {
        fn tts_status_strategy()(
            engine in status_tts_strategy(),
            model in prop::option::of(status_tts_model_strategy()),
            language in opt_short_string(),
            provider in opt_short_string(),
            status in prop::option::of(engine_status_strategy()),
            last_utterance in prop::option::of(utterance_status_strategy()),
        ) -> TtsStatus {
            TtsStatus { engine, model, language, provider, status, last_utterance }
        }
    }

    prop_compose! {
        fn stt_status_strategy()(
            engine in status_stt_strategy(),
            provider in opt_short_string(),
            status in prop::option::of(engine_status_strategy()),
            voice_key in opt_short_string(),
        ) -> SttStatus {
            SttStatus { engine, provider, status, voice_key }
        }
    }

    prop_compose! {
        fn diarization_status_strategy()(
            status in engine_status_strategy(),
            enabled in any::<bool>(),
            provider in short_string(),
            speakers in short_string_vec(),
            activity_threshold in unit_f64(),
        ) -> DiarizationStatus {
            DiarizationStatus {
                status,
                enabled,
                provider,
                speakers,
                activity_threshold,
            }
        }
    }

    prop_compose! {
        fn tts_snapshot_strategy()(
            rtf_min in finite_f64(),
            rtf_avg in finite_f64(),
            rtf_max in finite_f64(),
            ttfa_min_ms in finite_f64(),
            ttfa_avg_ms in finite_f64(),
            ttfa_max_ms in finite_f64(),
            utterances in any::<u64>(),
            audio_secs in finite_f64(),
            failures in any::<u64>(),
            queued in any::<u64>(),
        ) -> TtsSnapshot {
            TtsSnapshot {
                rtf_min,
                rtf_avg,
                rtf_max,
                ttfa_min_ms,
                ttfa_avg_ms,
                ttfa_max_ms,
                utterances,
                audio_secs,
                failures,
                queued,
            }
        }
    }

    prop_compose! {
        fn stt_snapshot_strategy()(
            rtf_min in finite_f64(),
            rtf_avg in finite_f64(),
            rtf_max in finite_f64(),
            transcriptions in any::<u64>(),
            audio_secs in finite_f64(),
            failures in any::<u64>(),
        ) -> SttSnapshot {
            SttSnapshot {
                rtf_min,
                rtf_avg,
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
        ) -> Stats {
            Stats { tts, stt, lifetime }
        }
    }

    prop_compose! {
        fn model_status_strategy()(
            seq in any::<u64>(),
            activity in activity_strategy(),
            tts in tts_status_strategy(),
            stt in stt_status_strategy(),
            diarization in diarization_status_strategy(),
            dictation in dictation_strategy(),
            stats in stats_strategy(),
            tray in prop::collection::vec(tray_kind_strategy(), 0..4),
            downloads in prop::collection::vec(download_status_strategy(), 0..4),
        ) -> ModelStatus {
            ModelStatus {
                seq,
                activity,
                tts,
                stt,
                diarization,
                dictation,
                stats,
                tray,
                downloads,
            }
        }
    }

    proptest! {
        /// Same wire contract as `json_contract_round_trips`, over generated values.
        #[test]
        fn json_contract_round_trips_arbitrary_values(status in model_status_strategy()) {
            let v = serde_json::to_value(status.clone()).unwrap();

            prop_assert!(v["tts"].get("status").is_some());
            prop_assert!(v["stt"].get("status").is_some());
            prop_assert!(v["diarization"]["status"]["state"].is_string());
            prop_assert!(v["tts"].get("provider").is_some());
            prop_assert!(v["stt"].get("provider").is_some());
            prop_assert!(v["stt"].get("voice_key").is_some());
            prop_assert!(v["seq"].is_u64());
            prop_assert!(v["stats"]["tts"]["rtf_avg"].is_f64());
            prop_assert!(v["stats"]["stt"]["transcriptions"].is_u64());
            prop_assert!(v["stats"]["lifetime"]["tts_secs"].is_u64());
            prop_assert!(v["diarization"]["speakers"].is_array());
            prop_assert!(v["downloads"].is_array());

            let back: ModelStatus = serde_json::from_value(v).unwrap();
            prop_assert_eq!(back, status);
        }
    }
}
