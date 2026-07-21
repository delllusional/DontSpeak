//! Typed `set_config` surface + apply onto `VoiceConfig`.

use serde::{Deserialize, Serialize};

use ds_config::{
    CancelSpeechScope, CaptureGain, DiarizerProvider, NarrateKind, Provider, SttEngine, TrayKind,
    TtsEngine, TtsModel, VoiceConfig, de_opt_pref_stt_engine, de_opt_pref_tts_engine,
    default_provider, normalize_tray,
};

fn de_opt_tts_model<'de, D>(deserializer: D) -> Result<Option<TtsModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<String>::deserialize(deserializer)?;
    value
        .map(|token| {
            TtsModel::parse(&token).ok_or_else(|| {
                D::Error::custom("tts_model must be kokoro, chatterbox, qwen, or omnivoice")
            })
        })
        .transpose()
}

/// Partial update for the nested `tts_voices` config object.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsVoiceUpdates {
    pub system: Option<Vec<String>>,
    pub kokoro: Option<Vec<String>>,
    pub chatterbox: Option<Vec<String>>,
    pub qwen: Option<Vec<String>>,
    pub omnivoice: Option<Vec<String>>,
}

struct TtsVoiceUpdateParts {
    system: Option<Vec<String>>,
    models: [(TtsModel, Option<Vec<String>>); 4],
}

impl TtsVoiceUpdates {
    fn is_empty(&self) -> bool {
        self.system.is_none()
            && self.kokoro.is_none()
            && self.chatterbox.is_none()
            && self.qwen.is_none()
            && self.omnivoice.is_none()
    }

    fn into_updates(self) -> TtsVoiceUpdateParts {
        TtsVoiceUpdateParts {
            system: self.system,
            models: [
                (TtsModel::Kokoro, self.kokoro),
                (TtsModel::Chatterbox, self.chatterbox),
                (TtsModel::Qwen, self.qwen),
                (TtsModel::OmniVoice, self.omnivoice),
            ],
        }
    }
}

/// Single source for set_config (schema/parse/apply can't drift). Guards:
/// parse (`deny_unknown_fields` + strict enums); apply (no `..`); schema name parity test.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetConfigArgs {
    pub rate: Option<f32>,
    pub tts_voices: Option<TtsVoiceUpdates>,
    #[serde(deserialize_with = "de_opt_tts_model")]
    pub tts_model: Option<TtsModel>,
    #[serde(deserialize_with = "de_opt_pref_tts_engine")]
    pub tts_engine: Option<Vec<TtsEngine>>,
    #[serde(deserialize_with = "de_opt_pref_stt_engine")]
    pub stt_engine: Option<Vec<SttEngine>>,
    pub diarizer: Option<Vec<DiarizerProvider>>,
    pub activity_threshold: Option<f32>,
    pub match_threshold: Option<f32>,
    pub speaker_lock: Option<bool>,
    pub full_duplex: Option<bool>,
    pub provider: Option<Vec<Provider>>,
    pub narrate: Option<Vec<NarrateKind>>,
    pub caps: Option<bool>,
    pub greet: Option<bool>,
    pub tray: Option<Vec<TrayKind>>,
    pub capture_gain: Option<CaptureGain>,
    pub double_tap_submit: Option<bool>,
    pub paste_delay_ms: Option<u64>,
    pub clear_on_input: Option<Vec<CancelSpeechScope>>,
    pub pause_bg: Option<bool>,
    pub earcon_reply: Option<String>,
    pub earcon_input: Option<String>,
}

impl SetConfigArgs {
    /// Merge fields onto `cfg`; `key=value` tokens. Clamps rate/gain; rejects empty voices.
    pub fn apply(self, cfg: &mut VoiceConfig) -> Result<Vec<String>, String> {
        // Enumerate Kokoro disk ids only when this call changes the Kokoro voice pool —
        // non-Kokoro applies and rejected-before-validation shapes stay disk-free.
        let needs_ids = self
            .tts_voices
            .as_ref()
            .and_then(|voices| voices.kokoro.as_ref())
            .is_some_and(|voices| !voices.is_empty());
        let kokoro_ids = needs_ids
            .then(ds_voices::enumerate::kokoro_disk_voice_ids)
            .flatten();
        self.apply_with(cfg, kokoro_ids.as_deref())
    }

    /// [`apply`](Self::apply) with the Kokoro voice-id source injected (hermetic tests).
    /// `Some(ids)` = enumerated disk voices: pool ids must be members AND speak a language
    /// this build routes. `None` = fresh install (no voices bin / MLX voices dir yet): skip
    /// membership so lazily-downloaded pack voices aren't rejected against the static
    /// fallback, but still require a routed language.
    /// Exhaustive destructure (no `..`) — new fields fail at compile.
    pub fn apply_with(
        self,
        cfg: &mut VoiceConfig,
        kokoro_ids: Option<&[String]>,
    ) -> Result<Vec<String>, String> {
        let SetConfigArgs {
            rate,
            tts_voices,
            tts_model,
            tts_engine,
            stt_engine,
            diarizer,
            activity_threshold,
            match_threshold,
            speaker_lock,
            full_duplex,
            provider,
            narrate,
            caps,
            greet,
            tray,
            capture_gain,
            double_tap_submit,
            paste_delay_ms,
            clear_on_input,
            pause_bg,
            earcon_reply,
            earcon_input,
        } = self;

        let mut changes = Vec::new();
        // The call's FINAL engine: an explicit `tts_engine` arg wins, else the config
        // resolution. System TTS consumes `rate` (and ignores the model), so the
        // model-descriptor gates below apply only when the final engine is `built_in`.
        let final_engine = match &tts_engine {
            Some(pref) => pref.first().copied(),
            None => cfg.resolved_tts(),
        };
        if let Some(model) = tts_model {
            cfg.tts_model = model;
            changes.push(format!("tts_model={}", model.as_str()));
        }
        if let Some(r) = rate {
            if final_engine == Some(TtsEngine::BuiltIn) && !cfg.tts_model.descriptor().supports_rate
            {
                return Err(format!(
                    "rate is not supported by {}",
                    cfg.tts_model.as_str()
                ));
            }
            let r = r.clamp(0.5, 2.0);
            cfg.rate = r;
            changes.push(format!("rate={r}"));
        }
        if let Some(voice_updates) = tts_voices {
            if voice_updates.is_empty() {
                return Err("`tts_voices` needs at least one engine or model".into());
            }
            let TtsVoiceUpdateParts { system, models } = voice_updates.into_updates();
            if let Some(system) = system {
                if system.iter().any(|voice| voice.trim().is_empty()) {
                    return Err("`tts_voices.system` needs non-empty voice names".into());
                }
                changes.push(format!("tts_voices.system=[{}]", system.join(", ")));
                cfg.tts_voices.system = system;
            }
            for (model, voices) in models {
                let Some(voices) = voices else { continue };
                let key = format!("tts_voices.{}", model.as_str());
                if voices.is_empty() || voices.iter().any(|voice| voice.trim().is_empty()) {
                    return Err(format!("`{key}` needs non-empty voice ids"));
                }
                let descriptor = model.descriptor();
                if let Some(bad) = voices.iter().find(|voice| {
                    if model != TtsModel::Kokoro {
                        return !descriptor.voices.contains(&voice.as_str());
                    }
                    kokoro_ids.is_some_and(|known| !known.contains(voice))
                }) {
                    return Err(format!(
                        "`{bad}` is not a {} voice; see voices",
                        model.as_str()
                    ));
                }
                // Kokoro publishes voices whose frontend this build does not ship (German; and
                // Japanese/Mandarin since those pipelines were dropped). They are real ids, so
                // membership passes and only the language rules them out. Voices for the routed
                // languages are admitted whatever their family: playback narrows the pool to the
                // detected language, so a non-English voice is only ever picked for its own.
                if model == TtsModel::Kokoro
                    && let Some(bad) = voices.iter().find(|voice| {
                        !descriptor
                            .languages
                            .contains(&ds_voices::enumerate::kokoro_language(voice))
                    })
                {
                    return Err(format!(
                        "`{bad}` speaks a language this build cannot route; see voices"
                    ));
                }
                changes.push(format!("{key}=[{}]", voices.join(", ")));
                *cfg.voices_for_mut(model) = voices;
            }
        }
        if let Some(pref) = tts_engine {
            // PREFERENCE not ladder: Some([]) = `"off"`, Some([engine]) = force that one.
            // Unusable → REJECT (not silently persisted to resolve to off later).
            if let Some(engine) = pref.first().copied() {
                if !engine.is_tts_usable() {
                    return Err(format!(
                        "`{}` not usable here — see status",
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
                        "`{}` not usable here — see status",
                        engine.as_str()
                    ));
                }
                changes.push(format!("stt_engine={}", engine.as_str()));
            } else {
                changes.push("stt_engine=off".to_string());
            }
            cfg.stt_engine = Some(pref);
        }
        if let Some(rungs) = diarizer {
            // Empty ladder = diarization off. De-dup, preserve order.
            let mut uniq: Vec<DiarizerProvider> = Vec::new();
            for p in rungs {
                if !uniq.contains(&p) {
                    uniq.push(p);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|p| p.as_str()).collect();
            changes.push(format!("diarizer=[{}]", toks.join(",")));
            cfg.diarizer = uniq;
        }
        if let Some(t) = activity_threshold {
            let t = t.clamp(0.1, 0.9);
            cfg.activity_threshold = t;
            changes.push(format!("activity_threshold={t}"));
        }
        if let Some(t) = match_threshold {
            let t = t.clamp(0.0, 1.0);
            cfg.match_threshold = t;
            changes.push(format!("match_threshold={t}"));
        }
        if let Some(b) = speaker_lock {
            cfg.speaker_lock = b;
            changes.push(format!("speaker_lock={b}"));
        }
        if let Some(b) = full_duplex {
            if b && final_engine == Some(TtsEngine::BuiltIn)
                && !cfg.tts_model.descriptor().supports_full_duplex
            {
                return Err(format!(
                    "full_duplex is not supported by {}",
                    cfg.tts_model.as_str()
                ));
            }
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
        if let Some(b) = caps {
            cfg.caps = b;
            changes.push(format!("caps={b}"));
        }
        if let Some(b) = greet {
            cfg.greet = b;
            changes.push(format!("greet={b}"));
        }
        if let Some(kinds) = tray {
            // One token per state (animated wins); `[]` = never color.
            let norm = normalize_tray(kinds);
            let toks: Vec<&str> = norm.iter().map(|k| k.as_str()).collect();
            changes.push(format!("tray=[{}]", toks.join(",")));
            cfg.tray = norm;
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
        if let Some(b) = double_tap_submit {
            cfg.double_tap_submit = b;
            changes.push(format!("double_tap_submit={b}"));
        }
        if let Some(d) = paste_delay_ms {
            let d = d.clamp(0, 5000);
            cfg.paste_delay_ms = d;
            changes.push(format!("paste_delay_ms={d}"));
        }
        if let Some(scopes) = clear_on_input {
            // Array IS the setting (`[]` = never cancel). De-dup, preserve order.
            let mut uniq: Vec<CancelSpeechScope> = Vec::new();
            for k in scopes {
                if !uniq.contains(&k) {
                    uniq.push(k);
                }
            }
            let toks: Vec<&str> = uniq.iter().map(|k| k.as_str()).collect();
            changes.push(format!("clear_on_input=[{}]", toks.join(",")));
            cfg.clear_on_input = uniq;
        }
        if let Some(b) = pause_bg {
            cfg.pause_bg = b;
            changes.push(format!("pause_bg={b}"));
        }
        if let Some(s) = earcon_reply {
            // Sound IS on/off (empty = off). Resolution/fail-quiet is the engine's.
            changes.push(format!("earcon_reply={s}"));
            cfg.earcon_reply = s;
        }
        if let Some(s) = earcon_input {
            changes.push(format!("earcon_input={s}"));
            cfg.earcon_input = s;
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
            rate: 1.0,
            tts_engine: Some(Vec::new()), // off
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs = serde_json::from_value(serde_json::json!({
            "greet": false,
            "narrate": ["digests", "shorts"],
            "rate": 1.5,
        }))
        .expect("valid args deserialize");
        let changes = args.apply(&mut cfg).expect("apply succeeds");

        assert!(!cfg.greet);
        assert_eq!(cfg.narrate, vec![NarrateKind::Digests, NarrateKind::Shorts]);
        assert_eq!(cfg.rate, 1.5);
        assert_eq!(
            cfg.tts_engine,
            Some(Vec::new()),
            "an unprovided field is left untouched"
        );
        assert_eq!(changes.len(), 3);
        assert!(changes.contains(&"greet=false".to_string()));
        assert!(changes.contains(&"narrate=[digests,shorts]".to_string()));
        assert!(changes.contains(&"rate=1.5".to_string()));
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
        // MCP set_config is strict; config-file deserialize is fail-open.
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
    fn set_config_tray_array_parses_and_rejects_bad_token() {
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tray": ["tts", "stt"] })).unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tray, vec![TrayKind::Stt, TrayKind::Tts]);
        assert_eq!(changes, vec!["tray=[stt,tts]".to_string()]);

        // `_animated` wins if both forms of a state appear.
        let anim: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tray": ["stt_animated", "tts", "tts_animated"] }),
        )
        .unwrap();
        let mut c3 = VoiceConfig::default();
        anim.apply(&mut c3).unwrap();
        assert_eq!(c3.tray, vec![TrayKind::SttAnimated, TrayKind::TtsAnimated]);

        let err = serde_json::from_value::<SetConfigArgs>(serde_json::json!({ "tray": ["bogus"] }))
            .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // `[]` = never color.
        let off: SetConfigArgs = serde_json::from_value(serde_json::json!({ "tray": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.tray.is_empty());
        assert_eq!(ch, vec!["tray=[]".to_string()]);
    }

    #[test]
    fn set_config_args_rate_is_clamped() {
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "rate": 9.0 })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.rate, 2.0);
        assert_eq!(changes, vec!["rate=2".to_string()]);
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
    fn set_config_clear_on_input_array_parses_dedups_and_rejects_bad_token() {
        let args: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "clear_on_input": ["other", "current", "current"] }),
        )
        .unwrap();
        let mut cfg = VoiceConfig::default();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(
            cfg.clear_on_input,
            vec![CancelSpeechScope::Other, CancelSpeechScope::Current]
        );
        assert_eq!(changes, vec!["clear_on_input=[other,current]".to_string()]);

        let err = serde_json::from_value::<SetConfigArgs>(
            serde_json::json!({ "clear_on_input": ["any_input"] }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must be one of"), "got: {err}");

        // `[]` = never cancel.
        let off: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "clear_on_input": [] })).unwrap();
        let mut c2 = VoiceConfig::default();
        let ch = off.apply(&mut c2).unwrap();
        assert!(c2.clear_on_input.is_empty());
        assert_eq!(ch, vec!["clear_on_input=[]".to_string()]);
    }

    #[test]
    fn set_config_model_and_voice_follow_the_registry() {
        let mut cfg = VoiceConfig::default();
        let ok: SetConfigArgs = serde_json::from_value(serde_json::json!({
            "tts_model": "chatterbox",
            "tts_voices": { "chatterbox": ["default"] }
        }))
        .unwrap();
        let changes = ok.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tts_model, TtsModel::Chatterbox);
        assert_eq!(cfg.tts_voices.chatterbox, ["default"]);
        assert_eq!(
            changes,
            vec!["tts_model=chatterbox", "tts_voices.chatterbox=[default]"]
        );

        let bad: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_voices": { "chatterbox": ["ru_boris"] } }),
        )
        .unwrap();
        let err = bad.apply(&mut cfg).unwrap_err();
        assert!(
            err.contains("ru_boris") && err.contains("chatterbox"),
            "{err}"
        );
    }

    #[test]
    fn set_config_system_voice_pool_accepts_names_and_os_default() {
        let mut cfg = VoiceConfig::default();
        let named: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_voices": { "system": ["Ava", "Samantha"] } }),
        )
        .unwrap();
        assert_eq!(
            named.apply(&mut cfg).unwrap(),
            ["tts_voices.system=[Ava, Samantha]"]
        );
        assert_eq!(cfg.tts_voices.system, ["Ava", "Samantha"]);

        let default_voice: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "system": [] } })).unwrap();
        assert_eq!(
            default_voice.apply(&mut cfg).unwrap(),
            ["tts_voices.system=[]"]
        );
        assert!(cfg.tts_voices.system.is_empty());
    }

    #[test]
    fn set_config_args_empty_voices_rejected() {
        let mut cfg = VoiceConfig::default();
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "kokoro": [] } })).unwrap();
        assert!(args.apply(&mut cfg).is_err());
    }

    #[test]
    fn set_config_args_admit_every_routed_language_but_reject_unroutable_ones() {
        // Hermetic: a FIXED enumerated-id list via apply_with (never the developer's live
        // model cache). Membership still binds, and the language rule now admits any language
        // the build routes — playback narrows the pool per utterance, so a non-English voice
        // can only ever be picked for its own language.
        let known: Vec<String> = ["af_sarah", "bm_george", "ef_dora", "if_sara", "jf_alpha"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut cfg = VoiceConfig::default();
        let mixed: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_voices": { "kokoro": ["af_sarah", "if_sara", "ef_dora"] } }),
        )
        .unwrap();
        assert!(mixed.apply_with(&mut cfg, Some(&known)).is_ok());
        assert_eq!(cfg.tts_voices.kokoro, ["af_sarah", "if_sara", "ef_dora"]);

        // Enumerated but unroutable: Japanese lost its frontend, so the voice cannot speak.
        let unroutable: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "kokoro": ["jf_alpha"] } }))
                .unwrap();
        assert!(unroutable.apply_with(&mut cfg, Some(&known)).is_err());

        // Absent from the enumeration: still rejected, whatever its language.
        let unknown: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "kokoro": ["af_nova"] } }))
                .unwrap();
        assert!(unknown.apply_with(&mut cfg, Some(&known)).is_err());
    }

    #[test]
    fn set_config_args_fresh_install_accepts_routed_kokoro_ids() {
        // No enumeration source (None = fresh install, no voices bin / MLX dir yet):
        // lazily-downloadable pack ids are accepted instead of being rejected against the
        // static fallback list, provided their language is one this build routes.
        let mut cfg = VoiceConfig::default();
        let ok: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_voices": { "kokoro": ["af_nova", "if_sara"] } }),
        )
        .unwrap();
        let changes = ok.apply_with(&mut cfg, None).expect("pack ids accepted");
        assert_eq!(cfg.tts_voices.kokoro, ["af_nova", "if_sara"]);
        assert_eq!(
            changes,
            vec!["tts_voices.kokoro=[af_nova, if_sara]".to_string()]
        );

        // German ships no frontend at all, so its family is never admissible.
        let unroutable: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "kokoro": ["df_anna"] } }))
                .unwrap();
        assert!(unroutable.apply_with(&mut cfg, None).is_err());

        let unknown_family: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_voices": { "kokoro": ["xq_bogus"] } }))
                .unwrap();
        assert!(unknown_family.apply_with(&mut cfg, None).is_err());
    }

    #[test]
    fn same_model_set_leaves_pool_untouched() {
        let mut cfg = VoiceConfig {
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec!["af_heart".into()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_model": "kokoro" })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tts_voices.kokoro, ["af_heart"]);
        assert_eq!(changes, vec!["tts_model=kokoro".to_string()]);
    }

    #[test]
    fn model_change_preserves_per_model_pools() {
        let mut cfg = VoiceConfig {
            tts_voices: ds_config::TtsVoicePools {
                kokoro: vec!["af_heart".into()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_model": "omnivoice" })).unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.tts_model, TtsModel::OmniVoice);
        assert_eq!(cfg.tts_voices.kokoro, ["af_heart"]);
        assert_eq!(cfg.tts_voices.omnivoice, ["warm, clear female voice"]);
        assert_eq!(changes, vec!["tts_model=omnivoice"]);
    }

    /// Forced `built_in` in the same call: model-descriptor gates apply — even before the
    /// (later) engine-usability check runs for the rate arm.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    #[test]
    fn rate_and_full_duplex_reject_for_built_in_chatterbox() {
        let mut cfg = VoiceConfig {
            tts_model: TtsModel::Chatterbox,
            ..VoiceConfig::default()
        };
        let rate: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_engine": "built_in", "rate": 1.3 }))
                .unwrap();
        let err = rate.apply(&mut cfg).unwrap_err();
        assert!(err.contains("rate is not supported"), "{err}");

        let duplex: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_engine": "built_in", "full_duplex": true }),
        )
        .unwrap();
        let err = duplex.apply(&mut cfg).unwrap_err();
        assert!(err.contains("full_duplex is not supported"), "{err}");
    }

    /// With the call's FINAL engine off, `rate`/`full_duplex` are inert persisted config —
    /// settable regardless of the built-in model's descriptor (System consumes cfg.rate,
    /// off consumes nothing).
    #[test]
    fn rate_and_full_duplex_are_settable_when_the_final_engine_is_not_built_in() {
        let mut cfg = VoiceConfig {
            tts_model: TtsModel::Chatterbox,
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs = serde_json::from_value(
            serde_json::json!({ "tts_engine": "off", "rate": 1.3, "full_duplex": true }),
        )
        .unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.rate, 1.3);
        assert!(cfg.full_duplex);
        assert!(changes.contains(&"rate=1.3".to_string()), "{changes:?}");
        assert!(
            changes.contains(&"tts_engine=off".to_string()),
            "{changes:?}"
        );
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn rate_is_settable_when_the_call_switches_to_system() {
        let mut cfg = VoiceConfig {
            tts_model: TtsModel::Chatterbox,
            ..VoiceConfig::default()
        };
        let args: SetConfigArgs =
            serde_json::from_value(serde_json::json!({ "tts_engine": "system", "rate": 1.3 }))
                .unwrap();
        let changes = args.apply(&mut cfg).unwrap();
        assert_eq!(cfg.rate, 1.3);
        assert!(changes.contains(&"rate=1.3".to_string()), "{changes:?}");
        assert!(
            changes.contains(&"tts_engine=system".to_string()),
            "{changes:?}"
        );
    }
}
