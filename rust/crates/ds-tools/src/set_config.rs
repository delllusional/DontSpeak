//! `SetConfigArgs` — typed `set_config` surface + apply-onto-`VoiceConfig`.

use serde::{Deserialize, Serialize};

use ds_config::{
    CancelSpeechScope, CaptureGain, DiarizerProvider, NarrateKind, Provider, SttEngine, TrayKind,
    TtsEngine, VoiceConfig, de_opt_pref_stt_engine, de_opt_pref_tts_engine, default_provider,
    normalize_tray_indicator,
};

/// SINGLE source for the `set_config` surface (schema / parse / apply can't drift —
/// once `greet_on_open` was in `VoiceConfig` but unsettable). Guards:
///   • PARSE  — deserialize into this; `deny_unknown_fields` + strict enums (`strict_de!`).
///   • APPLY  — destructures EVERY field with no `..` → new field is a compile error.
///   • SCHEMA — `set_config_schema_matches_args` asserts property names match.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetConfigArgs {
    pub tts_rate: Option<f32>,
    pub tts_built_in_voices: Option<Vec<String>>,
    pub tts_system_voice: Option<String>,
    #[serde(deserialize_with = "de_opt_pref_tts_engine")]
    pub tts_engine: Option<Vec<TtsEngine>>,
    #[serde(deserialize_with = "de_opt_pref_stt_engine")]
    pub stt_engine: Option<Vec<SttEngine>>,
    pub diarizer_provider: Option<Vec<DiarizerProvider>>,
    pub clustering_threshold: Option<f32>,
    pub speaker_threshold: Option<f32>,
    pub stt_speaker_lock: Option<bool>,
    pub full_duplex: Option<bool>,
    pub provider: Option<Vec<Provider>>,
    pub narrate: Option<Vec<NarrateKind>>,
    pub caps_enabled: Option<bool>,
    pub greet_on_open: Option<bool>,
    pub tray_indicator: Option<Vec<TrayKind>>,
    pub capture_gain: Option<CaptureGain>,
    pub double_tap_submits: Option<bool>,
    pub paste_submit_delay_ms: Option<u64>,
    pub input_clears: Option<Vec<CancelSpeechScope>>,
    pub pause_in_background: Option<bool>,
    pub earcon_reply_sound: Option<String>,
    pub earcon_needs_input_sound: Option<String>,
}

impl SetConfigArgs {
    /// Merge provided fields onto `cfg`; returns `key=value` change tokens. Clamps
    /// rate/`Manual` gain; rejects empty voices. NO `..` — new fields fail to compile.
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
            full_duplex,
            provider,
            narrate,
            caps_enabled,
            greet_on_open,
            tray_indicator,
            capture_gain,
            double_tap_submits,
            paste_submit_delay_ms,
            input_clears,
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
                return Err("`tts_built_in_voices` needs non-empty voice ids".into());
            }
            // English-only: Kokoro language family is the leading id char (`a`/`b`).
            // Gate for the persistent pool (`list_voices` surfaces English only).
            if let Some(bad) = vs
                .iter()
                .find(|s| !matches!(s.chars().next(), Some('a') | Some('b')))
            {
                return Err(format!(
                    "`{bad}` is not English (ids start with a/b); see list_voices"
                ));
            }
            changes.push(format!("tts_built_in_voices=[{}]", vs.join(", ")));
            cfg.tts_built_in_voices = vs;
        }
        if let Some(v) = tts_system_voice {
            // Empty = OS default; don't reject.
            changes.push(format!("tts_system_voice={v}"));
            cfg.tts_system_voice = v;
        }
        if let Some(pref) = tts_engine {
            // PREFERENCE not ladder: Some([]) = `"off"`, Some([engine]) = force that one.
            // Unusable → REJECT (not silently persisted to resolve to off later).
            if let Some(engine) = pref.first().copied() {
                if !engine.is_tts_usable() {
                    return Err(format!(
                        "`{}` not usable here — see get_status",
                        engine.as_str()
                    ));
                }
                changes.push(format!("tts_engine={}", engine.as_str()));
            } else {
                changes.push("tts_engine=off".to_string());
            }
            cfg.tts_engine = Some(pref);
        }
        if let Some(rungs) = provider {
            // Priority ladder; de-dup. Empty/all-unknown → default (always a backend).
            let mut uniq: Vec<Provider> = Vec::new();
            for p in rungs {
                if !uniq.contains(&p) {
                    uniq.push(p);
                }
            }
            if uniq.is_empty() {
                uniq = default_provider();
            }
            let toks: Vec<&str> = uniq.iter().map(|p| p.as_str()).collect();
            changes.push(format!("provider=[{}]", toks.join(",")));
            cfg.provider = uniq;
        }
        if let Some(pref) = stt_engine {
            // PREFERENCE not ladder: Some([]) = `"off"`, Some([engine]) = force.
            // Static usability here; `system` also needs dynamic avail/auth at MCP
            // (`call_set_config` probes) — both gates required.
            if let Some(engine) = pref.first().copied() {
                if !engine.is_stt_usable() {
                    return Err(format!(
                        "`{}` not usable here — see get_status",
                        engine.as_str()
                    ));
                }
                changes.push(format!("stt_engine={}", engine.as_str()));
            } else {
                changes.push("stt_engine=off".to_string());
            }
            cfg.stt_engine = Some(pref);
        }
        if let Some(rungs) = diarizer_provider {
            // Empty ladder = diarization off. De-dup, preserve order.
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
        if let Some(b) = full_duplex {
            cfg.full_duplex = b;
            changes.push(format!("full_duplex={b}"));
        }
        if let Some(kinds) = narrate {
            // Array IS the setting (`[]` = none). De-dup, preserve order.
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
            // One token per state (animated wins); `[]` = never color.
            let norm = normalize_tray_indicator(kinds);
            let toks: Vec<&str> = norm.iter().map(|k| k.as_str()).collect();
            changes.push(format!("tray_indicator=[{}]", toks.join(",")));
            cfg.tray_indicator = norm;
        }
        if let Some(g) = capture_gain {
            let g = match g {
                CaptureGain::Auto => CaptureGain::Auto,
                CaptureGain::Manual(v) => CaptureGain::Manual(v.clamp(0.5, 20.0)),
            };
            cfg.capture_gain = g;
            changes.push(match g {
                CaptureGain::Auto => "capture_gain=auto".to_string(),
                CaptureGain::Manual(v) => format!("capture_gain={v}"),
            });
        }
        if let Some(b) = double_tap_submits {
            cfg.double_tap_submits = b;
            changes.push(format!("double_tap_submits={b}"));
        }
        if let Some(d) = paste_submit_delay_ms {
            let d = d.clamp(0, 5000);
            cfg.paste_submit_delay_ms = d;
            changes.push(format!("paste_submit_delay_ms={d}"));
        }
        if let Some(scopes) = input_clears {
            // Array IS the setting (`[]` = never cancel). De-dup, preserve order.
            let mut uniq: Vec<CancelSpeechScope> = Vec::new();
            for k in scopes {
                if !uniq.contains(&k) {
                    uniq.push(k);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|k| k.as_str()).collect();
            changes.push(format!("input_clears=[{}]", toks.join(",")));
            cfg.input_clears = uniq;
        }
        if let Some(b) = pause_in_background {
            cfg.pause_in_background = b;
            changes.push(format!("pause_in_background={b}"));
        }
        if let Some(s) = earcon_reply_sound {
            // Sound IS on/off (empty = off). Resolution/fail-quiet is the engine's.
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
        let mut cfg = VoiceConfig {
            tts_rate: 1.0,
            tts_engine: Some(Vec::new()), // off
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
        assert_eq!(
            cfg.tts_engine,
            Some(Vec::new()),
            "an unprovided field is left untouched"
        );
        assert_eq!(changes.len(), 3);
        assert!(changes.contains(&"greet_on_open=false".to_string()));
        assert!(changes.contains(&"narrate=[digests,shorts]".to_string()));
        assert!(changes.contains(&"tts_rate=1.5".to_string()));
    }

    #[test]
    fn set_config_args_reject_unknown_field() {
        // deny_unknown_fields: typo → hard error, not silent no-op.
        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({
            "greet_on_opne": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("unknown field"), "got: {err}");
    }

    #[test]
    fn set_config_args_strict_enum_errors_on_bad_token() {
        // Strict (unlike config-file fail-open): bad scalar + non-string shape both reject.
        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({
            "stt_engine": "deepgram"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({
            "stt_engine": ["deepgram"]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("must be a string"), "got: {err}");
    }

    #[test]
    fn set_config_narrate_array_parses_and_rejects_bad_token() {
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "narrate": ["shorts", "digests"] }))
                .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        assert_eq!(changes, vec!["narrate=[shorts,digests]".to_string()]);

        // Strict (unlike fail-open config file).
        let err =
            serde_json::from_value::<SetConfigArgs>(serde_json::json!({ "narrate": ["loud"] }))
                .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // `[]` = narrate nothing.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "narrate": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.narrate.is_empty());
        assert_eq!(ch, vec!["narrate=[]".to_string()]);
    }

    #[test]
    fn set_config_tray_indicator_array_parses_and_rejects_bad_token() {
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tray_indicator": ["tts", "stt"] }))
                .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tray_indicator, vec![TrayKind::Stt, TrayKind::Tts]);
        assert_eq!(changes, vec!["tray_indicator=[stt,tts]".to_string()]);

        // `_animated` wins if both forms of a state appear.
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

        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "tray_indicator": ["both"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // `[]` = never color.
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
    fn set_config_args_apply_accepts_stt_preference_scalar() {
        // SCALAR preference, not a ladder. `claude_code` is always usable → host-stable.
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "stt_engine": "claude_code" })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.stt_engine, Some(vec![SttEngine::ClaudeCode]));
        assert_eq!(changes, vec!["stt_engine=claude_code".to_string()]);

        // Arrays rejected (old ladder shape is gone).
        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "stt_engine": ["built_in", "claude_code"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be a string"), "got: {err}");

        // `"off"` is user-facing wire token — no `Off` enum variant; deserializer handles it.
        let mut c3 = VoiceConfig::default();
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "stt_engine": "off" })).unwrap();
        let ch = off.apply(&mut c3).unwrap();
        assert_eq!(c3.stt_engine, Some(Vec::new()));
        assert_eq!(ch, vec!["stt_engine=off".to_string()]);

        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({ "stt_engine": [] }))
            .unwrap_err();
        assert!(err.to_string().contains("must be a string"), "got: {err}");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn set_config_args_apply_accepts_stt_system_on_macos() {
        // Static usable only on macOS; runtime avail/auth is a separate MCP-layer probe.
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "stt_engine": "system" })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.stt_engine, Some(vec![SttEngine::System]));
        assert_eq!(changes, vec!["stt_engine=system".to_string()]);
    }

    #[test]
    fn set_config_args_apply_rejects_unusable_engine_choice() {
        // Unusable choice is REJECTED — never silently persisted to resolve to off.
        #[cfg(not(target_os = "macos"))]
        {
            let mut cfg = VoiceConfig::default();
            let args: SetConfigArgs =
                serde_json::from_value(serde_json::json!({ "stt_engine": "system" })).unwrap();
            let err = args.apply(&mut cfg).unwrap_err();
            assert!(err.contains("not usable"), "got: {err}");
            assert_eq!(cfg.stt_engine, None);
        }
    }

    #[test]
    fn set_config_input_clears_array_parses_dedups_and_rejects_bad_token() {
        let args: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "input_clears": ["other", "current", "current"] }),
        )
        .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(
            cfg.input_clears,
            vec![CancelSpeechScope::Other, CancelSpeechScope::Current]
        );
        assert_eq!(changes, vec!["input_clears=[other,current]".to_string()]);

        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "input_clears": ["any_input"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // `[]` = never cancel.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "input_clears": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.input_clears.is_empty());
        assert_eq!(ch, vec!["input_clears=[]".to_string()]);
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
        // English-only: non-`a`/`b` Kokoro ids rejected.
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
