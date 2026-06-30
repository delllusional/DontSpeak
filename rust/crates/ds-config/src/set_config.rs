//! `SetConfigArgs` — the typed surface of the `set_config` MCP tool, plus its
//! apply-onto-`VoiceConfig` logic.

use serde::{Deserialize, Serialize};

use crate::{
    CaptureGain, DiarizerProvider, DropSpeechKind, NarrateKind, Provider, SttEngine, TrayKind,
    TtsEngine, VoiceConfig,
};

/// The fields settable through the `set_config` MCP tool — the SINGLE source of
/// truth for that tool's surface, so the schema, the parse, and the apply can never
/// silently disagree (the drift that once left `greet_on_open` in `VoiceConfig` but
/// unsettable). Three guards, one per drift direction:
///   • PARSE  — the inbound JSON args deserialize straight into this struct;
///              `deny_unknown_fields` rejects typos, and enum/`CaptureGain` values are
///              validated STRICTLY (unknown token → error, via the `strict_de!` macro). Adding
///              a field here makes it parseable automatically.
///   • APPLY  — [`SetConfigArgs::apply`] destructures EVERY field with no `..`, so a
///              newly-added field fails to COMPILE until it is wired through.
///   • SCHEMA — a CI test (`set_config_schema_matches_args` in ds-tools) asserts the
///              JSON-Schema property set equals these field names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetConfigArgs {
    pub tts_rate: Option<f32>,
    pub tts_built_in_voices: Option<Vec<String>>,
    pub tts_system_voice: Option<String>,
    #[serde(deserialize_with = "crate::enums::de_opt_set_tts_engine")]
    pub tts_engine: Option<Vec<TtsEngine>>,
    #[serde(deserialize_with = "crate::enums::de_opt_set_stt_engine")]
    pub stt_engine: Option<Vec<SttEngine>>,
    pub diarizer_provider: Option<Vec<DiarizerProvider>>,
    pub clustering_threshold: Option<f32>,
    pub speaker_threshold: Option<f32>,
    pub stt_speaker_lock: Option<bool>,
    pub provider: Option<Vec<Provider>>,
    pub narrate: Option<Vec<NarrateKind>>,
    pub caps_enabled: Option<bool>,
    pub greet_on_open: Option<bool>,
    pub tray_indicator: Option<Vec<TrayKind>>,
    pub capture_gain: Option<CaptureGain>,
    pub auto_submit: Option<bool>,
    pub drop_speech_on: Option<Vec<DropSpeechKind>>,
    pub pause_in_background: Option<bool>,
    pub earcon_reply_sound: Option<String>,
    pub earcon_needs_input_sound: Option<String>,
}

impl SetConfigArgs {
    /// Merge the provided `VoiceConfig` fields onto `cfg`, returning a human-readable
    /// summary of each change (one `key=value` token) for the tool's reply. `rate` and
    /// any `Manual` `capture_gain` are range-clamped; `voices` is rejected if empty.
    ///
    /// Destructured with NO `..` ON PURPOSE: a new `SetConfigArgs` field is a compile
    /// error here until handled.
    pub fn apply(self, cfg: &mut VoiceConfig) -> Result<Vec<String>, String> {
        let SetConfigArgs {
            tts_rate,
            tts_built_in_voices,
            tts_system_voice,
            tts_engine,
            stt_engine,
            diarizer_provider,
            clustering_threshold,
            speaker_threshold,
            stt_speaker_lock,
            provider,
            narrate,
            caps_enabled,
            greet_on_open,
            tray_indicator,
            capture_gain,
            auto_submit,
            drop_speech_on,
            pause_in_background,
            earcon_reply_sound,
            earcon_needs_input_sound,
        } = self;

        let mut changes = Vec::new();
        if let Some(r) = tts_rate {
            let r = r.clamp(0.5, 2.0);
            cfg.tts_rate = r;
            changes.push(format!("tts_rate={r}"));
        }
        if let Some(vs) = tts_built_in_voices {
            if vs.is_empty() || vs.iter().any(|s| s.trim().is_empty()) {
                return Err(
                    "`tts_built_in_voices` must be a non-empty array of non-empty voice ids".into(),
                );
            }
            // English-only build: Kokoro encodes the language family in the id's leading char
            // (`a` American + `b` British English). Reject any non-English id so the persistent
            // pool can't bypass the English-only gate that set_voice/list_voices enforce.
            if let Some(bad) = vs
                .iter()
                .find(|s| !matches!(s.as_bytes().first(), Some(b'a') | Some(b'b')))
            {
                return Err(format!(
                    "`{bad}` is not an English Kokoro voice. This version supports English only (ids starting `a`/`b`); see list_voices."
                ));
            }
            changes.push(format!("tts_built_in_voices=[{}]", vs.join(", ")));
            cfg.tts_built_in_voices = vs;
        }
        if let Some(v) = tts_system_voice {
            // A single voice name for the System (`say`) engine; EMPTY is allowed and means
            // "use the OS default voice", so don't reject it.
            changes.push(format!("tts_system_voice={v}"));
            cfg.tts_system_voice = v;
        }
        if let Some(rungs) = tts_engine {
            // An ORDERED preference ladder ([] = spoken replies off); the strict deserializer
            // already dropped `off`/dupes and rejected bad tokens.
            let toks: Vec<&str> = rungs.iter().map(|e| e.as_str()).collect();
            changes.push(format!("tts_engine=[{}]", toks.join(",")));
            cfg.tts_engine = rungs;
        }
        if let Some(rungs) = provider {
            // Ordered priority ladder; de-dup preserving order. Empty/all-unknown falls back
            // to the default ladder (there is always a compute backend).
            let mut uniq: Vec<Provider> = Vec::new();
            for p in rungs {
                if !uniq.contains(&p) {
                    uniq.push(p);
                }
            }
            if uniq.is_empty() {
                uniq = crate::enums::default_provider();
            }
            let toks: Vec<&str> = uniq.iter().map(|p| p.as_str()).collect();
            changes.push(format!("provider=[{}]", toks.join(",")));
            cfg.provider = uniq;
        }
        if let Some(rungs) = stt_engine {
            // An ORDERED preference ladder ([] = dictation off); the strict deserializer already
            // dropped `off`/dupes and rejected bad tokens. When `system` is among the rungs it
            // is verified for AVAILABILITY + authorization at the MCP layer (call_set_config
            // probes the running engine) BEFORE this applies, so an unusable `system` is refused
            // there rather than persisted. This pure apply just records the chosen ladder.
            let toks: Vec<&str> = rungs.iter().map(|e| e.as_str()).collect();
            changes.push(format!("stt_engine=[{}]", toks.join(",")));
            cfg.stt_engine = rungs;
        }
        if let Some(rungs) = diarizer_provider {
            // The ladder IS the on/off: empty = diarization off. De-dup, preserve order.
            let mut uniq: Vec<DiarizerProvider> = Vec::new();
            for p in rungs {
                if !uniq.contains(&p) {
                    uniq.push(p);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|p| p.as_str()).collect();
            changes.push(format!("diarizer_provider=[{}]", toks.join(",")));
            cfg.diarizer_provider = uniq;
        }
        if let Some(t) = clustering_threshold {
            let t = t.clamp(0.5, 0.9);
            cfg.clustering_threshold = t;
            changes.push(format!("clustering_threshold={t}"));
        }
        if let Some(t) = speaker_threshold {
            let t = t.clamp(0.0, 1.0);
            cfg.speaker_threshold = t;
            changes.push(format!("speaker_threshold={t}"));
        }
        if let Some(b) = stt_speaker_lock {
            cfg.stt_speaker_lock = b;
            changes.push(format!("stt_speaker_lock={b}"));
        }
        if let Some(kinds) = narrate {
            // De-dup, preserving the caller's order (the array IS the setting — `[]` = none).
            let mut uniq: Vec<NarrateKind> = Vec::new();
            for k in kinds {
                if !uniq.contains(&k) {
                    uniq.push(k);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|k| k.as_str()).collect();
            changes.push(format!("narrate=[{}]", toks.join(",")));
            cfg.narrate = uniq;
        }
        if let Some(b) = caps_enabled {
            cfg.caps_enabled = b;
            changes.push(format!("caps_enabled={b}"));
        }
        if let Some(b) = greet_on_open {
            cfg.greet_on_open = b;
            changes.push(format!("greet_on_open={b}"));
        }
        if let Some(kinds) = tray_indicator {
            // Normalize to one token per state (animated form wins); `[]` = never color.
            let norm = crate::enums::normalize_tray_indicator(kinds);
            let toks: Vec<&str> = norm.iter().map(|k| k.as_str()).collect();
            changes.push(format!("tray_indicator=[{}]", toks.join(",")));
            cfg.tray_indicator = norm;
        }
        if let Some(g) = capture_gain {
            cfg.capture_gain = g;
            changes.push(match g {
                CaptureGain::Auto => "capture_gain=auto".to_string(),
                CaptureGain::Manual(v) => format!("capture_gain={v}"),
            });
        }
        if let Some(b) = auto_submit {
            cfg.auto_submit = b;
            changes.push(format!("auto_submit={b}"));
        }
        if let Some(kinds) = drop_speech_on {
            // De-dup, preserving order (the array IS the setting — `[]` = never drop).
            let mut uniq: Vec<DropSpeechKind> = Vec::new();
            for k in kinds {
                if !uniq.contains(&k) {
                    uniq.push(k);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|k| k.as_str()).collect();
            changes.push(format!("drop_speech_on=[{}]", toks.join(",")));
            cfg.drop_speech_on = uniq;
        }
        if let Some(b) = pause_in_background {
            cfg.pause_in_background = b;
            changes.push(format!("pause_in_background={b}"));
        }
        if let Some(s) = earcon_reply_sound {
            // The sound IS the on/off: empty turns the reply ding off; a bundled name or an
            // absolute path turns it on. Resolution + fail-quiet are the engine's.
            changes.push(format!("earcon_reply_sound={s}"));
            cfg.earcon_reply_sound = s;
        }
        if let Some(s) = earcon_needs_input_sound {
            changes.push(format!("earcon_needs_input_sound={s}"));
            cfg.earcon_needs_input_sound = s;
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_config_args_apply_merges_only_provided_fields() {
        // A partial JSON payload (as set_config receives over MCP) applies onto a base
        // config, touching ONLY the provided fields and reporting each in the summary.
        let mut cfg = VoiceConfig {
            tts_rate: 1.0,
            tts_engine: Vec::new(), // off
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs = serde_json::from_value(serde_json::json!({
            "greet_on_open": false,
            "narrate": ["digests", "shorts"],
            "tts_rate": 1.5,
        }))
        .expect("valid args deserialize");
        let changes = args.apply(&mut cfg).expect("apply succeeds");

        assert!(!cfg.greet_on_open);
        assert_eq!(cfg.narrate, vec![NarrateKind::Digests, NarrateKind::Shorts]);
        assert_eq!(cfg.tts_rate, 1.5);
        assert!(
            cfg.tts_engine.is_empty(),
            "an unprovided field is left untouched"
        );
        assert_eq!(changes.len(), 3);
        assert!(changes.contains(&"greet_on_open=false".to_string()));
        assert!(changes.contains(&"narrate=[digests,shorts]".to_string()));
        assert!(changes.contains(&"tts_rate=1.5".to_string()));
    }

    #[test]
    fn set_config_args_reject_unknown_field() {
        // deny_unknown_fields turns a typo'd key into a hard error (not a silent no-op).
        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({
            "greet_on_opne": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn set_config_args_strict_enum_errors_on_bad_token() {
        // Unlike the config-file fail-open path, set_config rejects an invalid token.
        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({
            "stt_engine": "deepgram"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");
    }

    #[test]
    fn set_config_narrate_array_parses_and_rejects_bad_token() {
        // Valid tokens parse into the set (canonical tokens, in array order)...
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "narrate": ["shorts", "digests"] }))
                .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        assert_eq!(changes, vec!["narrate=[shorts,digests]".to_string()]);

        // ...an unknown token is REJECTED (strict, unlike the fail-open config file).
        let err =
            serde_json::from_value::<SetConfigArgs>(serde_json::json!({ "narrate": ["loud"] }))
                .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // An empty array is valid — it means narrate nothing.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "narrate": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.narrate.is_empty());
        assert_eq!(ch, vec!["narrate=[]".to_string()]);
    }

    #[test]
    fn set_config_tray_indicator_array_parses_and_rejects_bad_token() {
        // Valid tokens normalize to one-per-state, canonical order (stt, then tts)...
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tray_indicator": ["tts", "stt"] }))
                .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tray_indicator, vec![TrayKind::Stt, TrayKind::Tts]);
        assert_eq!(changes, vec!["tray_indicator=[stt,tts]".to_string()]);

        // The `_animated` form colors AND breathes; it WINS if both forms of a state appear.
        let anim: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tray_indicator": ["stt_animated", "tts", "tts_animated"] }),
        )
        .unwrap();
        let mut c3 = VoiceConfig::default();
        anim.apply(&mut c3).unwrap();
        assert_eq!(
            c3.tray_indicator,
            vec![TrayKind::SttAnimated, TrayKind::TtsAnimated]
        );

        // ...an unknown token is REJECTED (strict, unlike the fail-open config file).
        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "tray_indicator": ["both"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // An empty array is valid — it means never color the icon.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tray_indicator": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.tray_indicator.is_empty());
        assert_eq!(ch, vec!["tray_indicator=[]".to_string()]);
    }

    #[test]
    fn set_config_args_rate_is_clamped() {
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_rate": 9.0 })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tts_rate, 2.0);
        assert_eq!(changes, vec!["tts_rate=2".to_string()]);
    }

    #[test]
    fn set_config_args_apply_accepts_stt_system() {
        // `system` is now a settable engine: pure `apply` records it (the availability +
        // authorization gate lives at the MCP layer in call_set_config, which probes the
        // running engine before persisting — so an unavailable `system` is refused there,
        // not here).
        let mut cfg = VoiceConfig::default();
        // A scalar string is still accepted (back-compat) and becomes a one-rung ladder.
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "stt_engine": "system" })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.stt_engine, vec![SttEngine::System]);
        assert_eq!(changes, vec!["stt_engine=[system]".to_string()]);

        // The array form is the canonical ladder; `off` and dupes drop, order is kept.
        let mut c2 = VoiceConfig::default();
        let arr: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "stt_engine": ["built_in", "claude_code", "built_in"] }),
        )
        .unwrap();
        let ch = arr.apply(&mut c2).unwrap();
        assert_eq!(
            c2.stt_engine,
            vec![SttEngine::BuiltIn, SttEngine::ClaudeCode]
        );
        assert_eq!(ch, vec!["stt_engine=[built_in,claude_code]".to_string()]);

        // An empty array (or `["off"]`) disables dictation.
        let mut c3 = VoiceConfig::default();
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "stt_engine": ["off"] })).unwrap();
        off.apply(&mut c3).unwrap();
        assert!(c3.stt_engine.is_empty());
    }

    #[test]
    fn set_config_drop_speech_on_array_parses_dedups_and_rejects_bad_token() {
        // Valid tokens parse into the set, de-duped, in array order.
        let args: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "drop_speech_on": ["text", "voice", "voice"] }),
        )
        .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(
            cfg.drop_speech_on,
            vec![DropSpeechKind::Text, DropSpeechKind::Voice]
        );
        assert_eq!(changes, vec!["drop_speech_on=[text,voice]".to_string()]);

        // An unknown token is REJECTED (strict, unlike the fail-open config file).
        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "drop_speech_on": ["any_input"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // An empty array is valid — it means never drop.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "drop_speech_on": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.drop_speech_on.is_empty());
        assert_eq!(ch, vec!["drop_speech_on=[]".to_string()]);
    }

    #[test]
    fn set_config_args_empty_voices_rejected() {
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_built_in_voices": [] })).unwrap();
        assert!(args.apply(&mut cfg).is_err());
    }

    #[test]
    fn set_config_args_non_english_voices_rejected() {
        // English-only build: a Spanish Kokoro id (`ef_dora`) must be rejected, while English
        // ids (`a`/`b` families) are accepted.
        let mut cfg = VoiceConfig::default();
        let bad: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_built_in_voices": ["ef_dora"] }))
                .unwrap();
        assert!(bad.apply(&mut cfg).is_err());

        let good: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_built_in_voices": ["af_sarah", "bm_george"] }),
        )
        .unwrap();
        assert!(good.apply(&mut cfg).is_ok());
    }
}
