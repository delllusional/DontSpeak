//! `VoiceConfig` — typed speech config from `config.toml`.

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use serde::{Deserialize, Deserializer, Serialize};

use crate::enums::{
    de_clear_on_input, de_diarizer, de_exclude_clients, de_listen_mode, de_narrate, de_provider,
    de_stt_engine_ladder, de_stt_engine_pref, de_tray, de_tts_engine_ladder, de_tts_engine_pref,
    default_clear_on_input, default_diarizer, default_narrate, default_provider,
    default_stt_engine_ladder, default_tray, default_tts_engine_ladder, se_stt_engine_pref,
    se_tts_engine_pref,
};
use crate::tts_model::{SYSTEM_TTS_PARAMS, de_tts_model, validate_tts_param};
use ds_log::{LogLevel, log};

use crate::{
    CancelSpeechScope, DiarizerProvider, ListenMode, NarrateKind, Paths, Provider, SttEngine,
    TrayKind, TtsEngine, TtsModel, TtsModelDescriptor, TtsParamMap, TtsParamValue, WiredAgent,
};

/// Hands-free phrases. START fuzzy; submit/cancel exact. Shelved (STT quality).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandsFreePhrases {
    pub start: String,
    pub submit: String,
    pub cancel: String,
}

impl Default for HandsFreePhrases {
    fn default() -> Self {
        Self {
            start: "computer".to_string(),
            submit: "submit".to_string(),
            cancel: "cancel".to_string(),
        }
    }
}

/// Voice pools keyed by the engine/model that consumes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TtsVoicePools {
    /// Empty selects the operating-system default voice.
    pub system: Vec<String>,
    pub kokoro: Vec<String>,
    pub chatterbox: Vec<String>,
    pub qwen: Vec<String>,
    pub omnivoice: Vec<String>,
}

impl Default for TtsVoicePools {
    fn default() -> Self {
        Self {
            system: Vec::new(),
            kokoro: default_model_voices(TtsModel::Kokoro),
            chatterbox: default_model_voices(TtsModel::Chatterbox),
            qwen: default_model_voices(TtsModel::Qwen),
            omnivoice: default_model_voices(TtsModel::OmniVoice),
        }
    }
}

impl TtsVoicePools {
    pub fn for_model(&self, model: TtsModel) -> &[String] {
        match model {
            TtsModel::Kokoro => &self.kokoro,
            TtsModel::Chatterbox => &self.chatterbox,
            TtsModel::Qwen => &self.qwen,
            TtsModel::OmniVoice => &self.omnivoice,
        }
    }

    pub fn for_model_mut(&mut self, model: TtsModel) -> &mut Vec<String> {
        match model {
            TtsModel::Kokoro => &mut self.kokoro,
            TtsModel::Chatterbox => &mut self.chatterbox,
            TtsModel::Qwen => &mut self.qwen,
            TtsModel::OmniVoice => &mut self.omnivoice,
        }
    }
}

/// Sparse `[tts_params.<target>]` overrides. Descriptor defaults fill on resolution;
/// blocks survive model switches.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TtsParamPools {
    #[serde(skip_serializing_if = "TtsParamMap::is_empty")]
    pub system: TtsParamMap,
    #[serde(skip_serializing_if = "TtsParamMap::is_empty")]
    pub kokoro: TtsParamMap,
    #[serde(skip_serializing_if = "TtsParamMap::is_empty")]
    pub chatterbox: TtsParamMap,
    #[serde(skip_serializing_if = "TtsParamMap::is_empty")]
    pub qwen: TtsParamMap,
    #[serde(skip_serializing_if = "TtsParamMap::is_empty")]
    pub omnivoice: TtsParamMap,
}

impl TtsParamPools {
    pub fn is_empty(&self) -> bool {
        self.system.is_empty()
            && self.kokoro.is_empty()
            && self.chatterbox.is_empty()
            && self.qwen.is_empty()
            && self.omnivoice.is_empty()
    }

    pub fn for_model(&self, model: TtsModel) -> &TtsParamMap {
        match model {
            TtsModel::Kokoro => &self.kokoro,
            TtsModel::Chatterbox => &self.chatterbox,
            TtsModel::Qwen => &self.qwen,
            TtsModel::OmniVoice => &self.omnivoice,
        }
    }

    pub fn for_model_mut(&mut self, model: TtsModel) -> &mut TtsParamMap {
        match model {
            TtsModel::Kokoro => &mut self.kokoro,
            TtsModel::Chatterbox => &mut self.chatterbox,
            TtsModel::Qwen => &mut self.qwen,
            TtsModel::OmniVoice => &mut self.omnivoice,
        }
    }

    fn from_value(value: &serde_json::Value) -> Self {
        let mut pools = Self::default();
        let Some(models) = value.as_object() else {
            return pools;
        };
        for (token, entries) in models {
            let Some(entries) = entries.as_object() else {
                continue;
            };
            if token == "system" {
                for (key, raw) in entries {
                    let Ok(raw) = serde_json::from_value(raw.clone()) else {
                        continue;
                    };
                    if let Ok(value) = validate_tts_param("system", SYSTEM_TTS_PARAMS, key, &raw) {
                        pools.system.insert(key.clone(), value);
                    }
                }
                continue;
            }
            let Some(model) = TtsModel::parse(token) else {
                continue;
            };
            let descriptor = model.descriptor();
            for (key, raw) in entries {
                let Ok(raw) = serde_json::from_value(raw.clone()) else {
                    continue;
                };
                if let Ok(value) = descriptor.validate_param(key, &raw) {
                    pools.for_model_mut(model).insert(key.clone(), value);
                }
            }
        }
        pools
    }
}

impl<'de> serde::Deserialize<'de> for TtsParamPools {
    /// Drop invalid entries independently so valid siblings survive.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d).unwrap_or_default();
        Ok(Self::from_value(&value))
    }
}

/// Speech config from `config.toml`. CC voice is read-only. Absent field = default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Per-engine/model voice pools (stable per-agent).
    #[serde(default)]
    pub tts_voices: TtsVoicePools,
    /// Per-engine/model TTS setting overrides; empty = descriptor defaults.
    #[serde(default, skip_serializing_if = "TtsParamPools::is_empty")]
    pub tts_params: TtsParamPools,
    /// Model hosted by the built-in engine.
    #[serde(default, deserialize_with = "de_tts_model")]
    pub tts_model: TtsModel,
    /// SessionStart greet (default on).
    #[serde(default = "default_enabled")]
    pub greet: bool,

    /// Narrate set (default both). Empty = off.
    #[serde(default = "default_narrate", deserialize_with = "de_narrate")]
    pub narrate: Vec<NarrateKind>,

    /// Caps hold ≥ ms → force-reset idle.
    #[serde(default = "default_long_press_ms")]
    pub long_press_ms: u64,

    /// Pref: None→ladder; Some(empty)=off; Some(one engine)=force. See `resolved_stt`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "se_stt_engine_pref",
        deserialize_with = "de_stt_engine_pref"
    )]
    pub stt_engine: Option<Vec<SttEngine>>,
    /// Ladder when pref unset (default system→built_in→claude_code). Config-file only.
    #[serde(
        default = "default_stt_engine_ladder",
        deserialize_with = "de_stt_engine_ladder"
    )]
    pub stt_engine_ladder: Vec<SttEngine>,

    /// Diarizer ladder; empty = off.
    #[serde(default = "default_diarizer", deserialize_with = "de_diarizer")]
    pub diarizer: Vec<DiarizerProvider>,
    /// Sortformer speaker-activity cutoff 0.1–0.9. Default 0.5.
    #[serde(default = "default_activity_threshold")]
    pub activity_threshold: f32,
    /// Enrolled-voiceprint cosine cutoff. Default 0.65.
    #[serde(default = "default_match_threshold")]
    pub match_threshold: f32,
    /// Enrolled speakers only when diarization on (fail-open).
    #[serde(default)]
    pub speaker_lock: bool,

    /// TTS pref tri-state (same shape as `stt_engine`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "se_tts_engine_pref",
        deserialize_with = "de_tts_engine_pref"
    )]
    pub tts_engine: Option<Vec<TtsEngine>>,
    /// Ladder when pref unset (default built_in→system). Config-file only.
    #[serde(
        default = "default_tts_engine_ladder",
        deserialize_with = "de_tts_engine_ladder"
    )]
    pub tts_engine_ladder: Vec<TtsEngine>,
    /// Compute ladder (default MLX→CUDA→CPU). Always has a backend.
    #[serde(default = "default_provider", deserialize_with = "de_provider")]
    pub provider: Vec<Provider>,

    // On/off = resolved engine; caps independent.
    /// Caps loop (dictation + silence/cancel). Default on.
    #[serde(default = "default_enabled")]
    pub caps: bool,
    /// Menu-bar color set. Empty = never color.
    #[serde(default = "default_tray", deserialize_with = "de_tray")]
    pub tray: Vec<TrayKind>,

    /// `record_submit` (PTT) | `always`.
    #[serde(default, deserialize_with = "de_listen_mode")]
    pub listen_mode: ListenMode,
    #[serde(default)]
    pub hands_free: HandsFreePhrases,
    /// Silence after stopword before submit (ms). Default 1000.
    #[serde(default = "default_submit_confirm_ms")]
    pub submit_confirm_ms: u64,
    /// Trailing silence for always-mode (ms). Default 700.
    #[serde(default = "default_endpoint_silence_ms")]
    pub endpoint_silence_ms: u64,

    /// Mic open during TTS with AEC. Default off.
    #[serde(default)]
    pub full_duplex: bool,
    /// Mic make-up gain; next dictation.
    #[serde(default = "default_capture_gain")]
    pub capture_gain: CaptureGain,

    /// false: stop-tap submits, double-tap inserts. True swaps.
    #[serde(default)]
    pub double_tap_submit: bool,

    /// ms paste→Enter (async paste can drop back-to-back Enter). Default 100.
    #[serde(default = "default_paste_delay_ms")]
    pub paste_delay_ms: u64,

    /// Whose speech a submit cancels (default `["current"]`; `[]` = never).
    #[serde(
        default = "default_clear_on_input",
        deserialize_with = "de_clear_on_input"
    )]
    pub clear_on_input: Vec<CancelSpeechScope>,

    /// Pause when no terminal frontmost. Default false.
    #[serde(default)]
    pub pause_bg: bool,

    /// Reply-done ding; empty = off. Default OS chime.
    #[serde(default = "default_earcon_reply")]
    pub earcon_reply: String,
    /// Needs-input cue. Empty = off.
    #[serde(default)]
    pub earcon_input: String,

    /// Codex mid-turn (re-read each loop). Default on; inert without daemon.
    #[serde(default = "default_enabled")]
    pub codex_stream: bool,
    /// Lazy-start app-server. Default off.
    #[serde(default)]
    pub codex_daemon: bool,
    /// App-server endpoint; empty = default socket; `ws://…` for TCP.
    #[serde(default)]
    pub codex_app_server_url: String,
    #[serde(default = "default_codex_bin")]
    pub codex_bin: String,

    /// Grok file-tail mid-turn. Default on; inert without `~/.grok` + session.
    #[serde(default = "default_enabled")]
    pub grok_stream: bool,

    /// Extra terminal ids (OS-native). Frontmost/pause/claude_code leak. #14.
    #[serde(default)]
    pub extra_terminals: Vec<String>,

    /// Extra custom-text-editor ids (OS-native). Linux ignores. #14/#15.
    #[serde(default)]
    pub extra_editors: Vec<String>,

    /// Persistent opt-out unwire list. None/`[]` = wire all.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_exclude_clients"
    )]
    pub exclude_clients: Option<Vec<WiredAgent>>,

    /// Agents feature gate: Agents tab + `usage` tool. Opt-in because usage
    /// probing may touch the macOS keychain.
    #[serde(default)]
    pub agents: bool,
}

/// Warm-subsystem delta for surgical set_config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConfigChange {
    pub caps_toggled: bool,
    /// Resolved TTS changed (incl. off).
    pub tts_toggled: bool,
    pub stt_changed: bool,
    pub listen_mode_changed: bool,
    /// Compute ladder changed — restart warm child.
    pub provider_changed: bool,
}

impl ConfigChange {
    pub fn is_noop(&self) -> bool {
        *self == ConfigChange::default()
    }
}

fn default_enabled() -> bool {
    true
}
/// Default reply earcon by platform name (`ds_earcon::resolve_cue`): Tink / ding / message.
fn default_earcon_reply() -> String {
    if cfg!(target_os = "macos") {
        "Tink".to_string()
    } else if cfg!(target_os = "windows") {
        "ding".to_string()
    } else if cfg!(target_os = "linux") {
        "message".to_string()
    } else {
        String::new()
    }
}
fn default_model_voices(model: TtsModel) -> Vec<String> {
    model
        .descriptor()
        .default_voices
        .iter()
        .map(|voice| (*voice).to_string())
        .collect()
}
fn default_long_press_ms() -> u64 {
    600
}
fn default_activity_threshold() -> f32 {
    0.5
}
fn default_match_threshold() -> f32 {
    0.65
}
fn default_submit_confirm_ms() -> u64 {
    1000
}
fn default_endpoint_silence_ms() -> u64 {
    700
}
fn default_paste_delay_ms() -> u64 {
    100
}
fn default_capture_gain() -> CaptureGain {
    CaptureGain::Auto
}
fn default_codex_bin() -> String {
    WiredAgent::Codex.as_str().to_string()
}

/// Mic make-up gain. `Auto` per-utterance; `Manual(g)` fixed. Wire: `"auto"` or number.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CaptureGain {
    #[default]
    Auto,
    Manual(f32),
}

impl CaptureGain {
    /// Fixed multiplier for `Manual`; `None` for `Auto`.
    pub fn manual(self) -> Option<f32> {
        match self {
            CaptureGain::Manual(g) => Some(g),
            CaptureGain::Auto => None,
        }
    }
}

impl serde::Serialize for CaptureGain {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CaptureGain::Auto => s.serialize_str("auto"),
            CaptureGain::Manual(g) => s.serialize_f32(*g),
        }
    }
}

impl<'de> serde::Deserialize<'de> for CaptureGain {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("auto") => Ok(CaptureGain::Auto),
            serde_json::Value::Number(n) => {
                let g = n
                    .as_f64()
                    .ok_or_else(|| Error::custom("capture_gain: invalid number"))?
                    as f32;
                Ok(CaptureGain::Manual(g.clamp(0.5, 20.0)))
            }
            _ => Ok(CaptureGain::Auto),
        }
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            tts_voices: TtsVoicePools::default(),
            tts_params: TtsParamPools::default(),
            tts_model: TtsModel::default(),
            greet: true,
            narrate: default_narrate(),
            long_press_ms: default_long_press_ms(),
            stt_engine: None,
            stt_engine_ladder: default_stt_engine_ladder(),
            diarizer: default_diarizer(),
            activity_threshold: default_activity_threshold(),
            match_threshold: default_match_threshold(),
            speaker_lock: false,
            tts_engine: None,
            tts_engine_ladder: default_tts_engine_ladder(),
            provider: default_provider(),
            caps: default_enabled(),
            tray: default_tray(),
            listen_mode: ListenMode::default(),
            hands_free: HandsFreePhrases::default(),
            submit_confirm_ms: default_submit_confirm_ms(),
            endpoint_silence_ms: default_endpoint_silence_ms(),
            full_duplex: false,
            capture_gain: default_capture_gain(),
            double_tap_submit: false,
            paste_delay_ms: default_paste_delay_ms(),
            clear_on_input: default_clear_on_input(),
            pause_bg: false,
            earcon_reply: default_earcon_reply(),
            earcon_input: String::new(),
            codex_stream: true,
            codex_daemon: false,
            codex_app_server_url: String::new(),
            codex_bin: default_codex_bin(),
            grok_stream: true,
            extra_terminals: Vec::new(),
            extra_editors: Vec::new(),
            exclude_clients: None,
            agents: false,
        }
    }
}

impl VoiceConfig {
    fn stored_rate(
        owner: &str,
        descriptors: &[crate::TtsParamDescriptor],
        stored: &TtsParamMap,
    ) -> f32 {
        let default = descriptors
            .iter()
            .find(|param| param.key == "rate")
            .and_then(|param| match param.default {
                crate::TtsParamDefault::Float(value) => Some(value),
                crate::TtsParamDefault::Int(value) => Some(value as f32),
                crate::TtsParamDefault::Choice(_) => None,
            })
            .unwrap_or(1.0);
        stored
            .get("rate")
            .and_then(|raw| validate_tts_param(owner, descriptors, "rate", raw).ok())
            .and_then(|value| match value {
                TtsParamValue::Float(value) => Some(value),
                TtsParamValue::Int(value) => Some(value as f32),
                TtsParamValue::Choice(_) => None,
            })
            .unwrap_or(default)
    }

    pub fn system_rate(&self) -> f32 {
        Self::stored_rate("system", SYSTEM_TTS_PARAMS, &self.tts_params.system)
    }

    pub fn model_rate(&self, model: TtsModel) -> f32 {
        let descriptor = model.descriptor();
        Self::stored_rate(
            descriptor.id,
            descriptor.config_params,
            self.tts_params.for_model(model),
        )
    }

    /// Spoken replies on ([`Self::resolved_tts`] is Some).
    pub fn is_tts_on(&self) -> bool {
        self.resolved_tts().is_some()
    }

    /// Pref wins (empty=off; force no auto-sub); else first usable ladder rung.
    pub fn resolved_tts(&self) -> Option<TtsEngine> {
        match &self.tts_engine {
            Some(pref) if pref.is_empty() => None,
            Some(pref) => pref.first().copied().filter(|e| e.is_tts_usable()),
            None => self
                .tts_engine_ladder
                .iter()
                .copied()
                .find(|e| e.is_tts_usable()),
        }
    }

    /// Same tri-state as TTS. Default ladder ends at always-usable `claude_code`.
    pub fn resolved_stt(&self) -> Option<SttEngine> {
        match &self.stt_engine {
            Some(pref) if pref.is_empty() => None,
            Some(pref) => pref.first().copied().filter(|e| e.is_stt_usable()),
            None => self
                .stt_engine_ladder
                .iter()
                .copied()
                .find(|e| e.is_stt_usable()),
        }
    }

    /// Warm-subsystem delta for surgical `set_config`.
    pub fn changes_since(&self, prev: &VoiceConfig) -> ConfigChange {
        ConfigChange {
            caps_toggled: self.caps != prev.caps,
            // Resolved TTS (not raw ladder) so reorder-only is no-op.
            tts_toggled: self.resolved_tts() != prev.resolved_tts()
                || self.tts_model != prev.tts_model,
            stt_changed: self.resolved_stt() != prev.resolved_stt()
                || self.provider != prev.provider,
            listen_mode_changed: self.listen_mode != prev.listen_mode,
            provider_changed: self.provider != prev.provider,
        }
    }
}

/// Warn from the raw table before fail-open deserialization discards invalid entries.
fn warn_dropped_tts_params(table: &toml::Table, paths: &Paths) {
    let warn = |message: &str| log(&paths.log_file, LogLevel::Warn, "config", message);
    let Some(params) = table.get("tts_params") else {
        return;
    };
    let Some(models) = params.as_table() else {
        return warn("tts_params is not a table; using defaults");
    };
    for (token, entries) in models {
        let Some(entries) = entries.as_table() else {
            warn(&format!("tts_params.{token} is not a table (ignored)"));
            continue;
        };
        let model = TtsModel::parse(token);
        if token != "system" && model.is_none() {
            warn(&format!(
                "unknown engine or model in [tts_params]: {token:?} (ignored)"
            ));
            continue;
        }
        for (key, raw) in entries {
            let value = match raw {
                toml::Value::Integer(value) => TtsParamValue::Int(*value),
                toml::Value::Float(value) => TtsParamValue::Float(*value as f32),
                toml::Value::String(value) => TtsParamValue::Choice(value.clone()),
                other => {
                    warn(&format!(
                        "tts_params.{token}.{key} has unsupported type {} (ignored)",
                        other.type_str()
                    ));
                    continue;
                }
            };
            let result = match model {
                Some(model) => model.descriptor().validate_param(key, &value),
                None => validate_tts_param("system", SYSTEM_TTS_PARAMS, key, &value),
            };
            if let Err(error) = result {
                warn(&format!("tts_params.{token}: {error} (ignored)"));
            }
        }
    }
}

/// config.toml as table. Fail-open → empty. Flat keys (VoiceConfig + MCP-HTTP).
pub(crate) fn read_config_table(paths: &Paths) -> toml::Table {
    std::fs::read_to_string(&paths.config_toml)
        .ok()
        .and_then(|s| toml::from_str::<toml::Table>(&s).ok())
        .unwrap_or_default()
}

/// Atomic write of a TOML table to config.toml.
pub(crate) fn write_config_table(paths: &Paths, table: &toml::Table) -> io::Result<()> {
    let text = toml::to_string_pretty(table).map_err(io::Error::other)?;
    crate::atomic_write_str(&paths.config_toml, &text)
}

/// Serde field names only (incl. skip_serializing_if).
struct StructFieldNames<'a>(&'a mut HashSet<String>);

impl<'de> serde::Deserializer<'de> for StructFieldNames<'_> {
    type Error = serde::de::value::Error;

    fn deserialize_any<V>(self, _visitor: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        Err(<Self::Error as serde::de::Error>::custom(
            "expected a derived struct",
        ))
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: serde::de::Visitor<'de>,
    {
        self.0
            .extend(fields.iter().map(|field| (*field).to_string()));
        Err(<Self::Error as serde::de::Error>::custom(
            "field names captured",
        ))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum
        identifier ignored_any
    }
}

impl VoiceConfig {
    /// Load config.toml; fail-open defaults. Unknown keys warned; numbers clamped.
    pub fn load(paths: &Paths) -> Self {
        let text = match std::fs::read_to_string(&paths.config_toml) {
            Ok(text) => text,
            Err(_) => {
                forget_config_diagnostics(&paths.config_toml);
                return Self::default();
            }
        };
        let emit_diagnostics = config_diagnostics_changed(&paths.config_toml, &text);
        let Ok(table) = toml::from_str::<toml::Table>(&text) else {
            if emit_diagnostics {
                log(
                    &paths.log_file,
                    LogLevel::Warn,
                    "config",
                    "config.toml is not valid TOML; using defaults",
                );
            }
            return Self::default();
        };
        if emit_diagnostics {
            // Warn on unknown keys (serde would drop typos silently).
            let known = Self::known_keys();
            for k in table.keys() {
                if !known.contains(k.as_str()) {
                    log(
                        &paths.log_file,
                        LogLevel::Warn,
                        "config",
                        &format!("unknown key in config.toml: {k:?} (ignored)"),
                    );
                }
            }
            warn_dropped_tts_params(&table, paths);
        }
        let mut cfg: VoiceConfig = toml::Value::Table(table).try_into().unwrap_or_default();
        cfg.clamp();
        cfg
    }

    /// Serde field list (incl. skip_serializing_if).
    fn known_keys() -> HashSet<String> {
        let mut keys = HashSet::new();
        let _ = VoiceConfig::deserialize(StructFieldNames(&mut keys));
        debug_assert!(!keys.is_empty(), "VoiceConfig must deserialize as a struct");
        keys
    }

    /// Clamp numerics so hand-edits can't feed the engine bad values.
    fn clamp(&mut self) {
        self.activity_threshold = self.activity_threshold.clamp(0.1, 0.9);
        self.match_threshold = self.match_threshold.clamp(0.0, 1.0);
        // Same 0..5000 as set_config.
        self.paste_delay_ms = self.paste_delay_ms.clamp(0, 5000);
        // 0 = long_press sentinel ("use default") — leave unclamped.
        if self.long_press_ms != 0 {
            self.long_press_ms = self.long_press_ms.clamp(100, 5000);
        }
        for model in TtsModel::ALL.iter().copied() {
            if model != TtsModel::Kokoro {
                let descriptor = model.descriptor();
                self.tts_voices
                    .for_model_mut(model)
                    .retain(|voice| descriptor.voices.contains(&voice.as_str()));
            }
            if self.tts_voices.for_model(model).is_empty() {
                *self.tts_voices.for_model_mut(model) = default_model_voices(model);
            }
        }
    }

    /// Built-in model + MLX provider (architecture only; runtime gates assets/shim).
    pub fn uses_mlx_model(&self) -> bool {
        self.resolved_tts() == Some(TtsEngine::BuiltIn)
            && cfg!(target_os = "macos")
            && self.resolved_tts_provider() == Provider::Mlx
    }

    /// First STT-usable provider rung, else CPU. Preference only — loader still falls back.
    pub fn resolved_stt_provider(&self) -> Provider {
        self.provider
            .iter()
            .copied()
            .find(|p| p.is_stt_usable())
            .unwrap_or(Provider::OrtCpu)
    }

    /// First provider supported by the selected model and platform, else CPU.
    pub fn resolved_tts_provider(&self) -> Provider {
        let descriptor = self.tts_model.descriptor();
        self.provider
            .iter()
            .copied()
            .find(|p| p.is_tts_usable() && descriptor.supports_provider(*p))
            .unwrap_or(Provider::OrtCpu)
    }

    /// `DONTSPEAK_PROVIDER` token for the warm child's TTS rung.
    pub fn tts_provider_token(&self) -> &'static str {
        self.resolved_tts_provider().as_str()
    }

    /// Non-empty diarizer ladder (gate for `diarize`/`enroll` + speaker-lock).
    pub fn is_diarization_on(&self) -> bool {
        !self.diarizer.is_empty()
    }

    /// First platform-usable diarizer rung, else `mlx`.
    pub fn resolved_diarizer(&self) -> DiarizerProvider {
        self.diarizer
            .iter()
            .copied()
            .find(|p| p.is_diarizer_usable())
            .unwrap_or(DiarizerProvider::Mlx)
    }

    /// Selected built-in model descriptor.
    pub fn tts_model_descriptor(&self) -> &'static TtsModelDescriptor {
        self.tts_model.descriptor()
    }

    /// Configured voice pool for a built-in model.
    pub fn voices_for(&self, model: TtsModel) -> &[String] {
        self.tts_voices.for_model(model)
    }

    /// Mutable configured voice pool for a built-in model.
    pub fn voices_for_mut(&mut self, model: TtsModel) -> &mut Vec<String> {
        self.tts_voices.for_model_mut(model)
    }

    /// Selected model's voice IDs; each backend loads its pinned representation.
    pub fn active_voices(&self) -> &[String] {
        self.voices_for(self.tts_model)
    }

    /// `Digests` gates blockquotes + injected spec; `Shorts` gates short whole replies.
    pub fn narrates(&self, kind: NarrateKind) -> bool {
        self.narrate.contains(&kind)
    }

    /// Compact `[digests,shorts]`-style list for logs.
    pub fn narrate_summary(&self) -> String {
        let toks: Vec<&str> = self.narrate.iter().map(|k| k.as_str()).collect();
        format!("[{}]", toks.join(","))
    }

    /// Excluded clients (empty when unset). Engine wires everyone else.
    pub fn excluded_clients(&self) -> Vec<WiredAgent> {
        self.exclude_clients.clone().unwrap_or_default()
    }
}

static CONFIG_DIAGNOSTICS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

/// Emit warnings once per distinct file content, not on every status poll.
fn config_diagnostics_changed(path: &std::path::Path, text: &str) -> bool {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let fingerprint = hasher.finish();
    let mut seen = CONFIG_DIAGNOSTICS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    seen.insert(path.to_path_buf(), fingerprint) != Some(fingerprint)
}

fn forget_config_diagnostics(path: &std::path::Path) {
    if let Some(seen) = CONFIG_DIAGNOSTICS.get() {
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(path);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{voice_from_value, voice_to_value, write_settings};

    #[test]
    fn capture_gain_accepts_auto_or_number() {
        // "auto" (any case) → Auto; a number → Manual, clamped to 0.5–20.
        assert_eq!(
            serde_json::from_str::<CaptureGain>("\"auto\"").unwrap(),
            CaptureGain::Auto
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("\"AUTO\"").unwrap(),
            CaptureGain::Auto
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("2.5").unwrap(),
            CaptureGain::Manual(2.5)
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("99").unwrap(),
            CaptureGain::Manual(20.0) // clamped
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("\"loud\"").unwrap(),
            CaptureGain::Auto
        );
        // Round-trips: Auto → "auto", Manual → number.
        assert_eq!(
            serde_json::to_string(&CaptureGain::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&CaptureGain::Manual(3.0)).unwrap(),
            "3.0"
        );
    }

    #[test]
    fn voice_defaults_when_absent() {
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        assert!(v.tts_voices.system.is_empty());
        assert_eq!(v.tts_voices.kokoro, vec!["af_sarah", "bf_emma"]);
        assert_eq!(v.tts_voices.chatterbox, vec!["default"]);
        assert_eq!(v.tts_voices.qwen, vec!["sohee"]);
        assert_eq!(v.tts_voices.omnivoice, vec!["young_woman"]);
        assert_eq!(v.tts_model, TtsModel::Kokoro);
        assert!(v.greet);
        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        assert!(v.narrates(NarrateKind::Digests) && v.narrates(NarrateKind::Shorts));
        assert_eq!(v.long_press_ms, 600);
        assert_eq!(v.tts_engine, None);
        assert_eq!(v.stt_engine, None);
        assert_eq!(
            v.tts_engine_ladder,
            vec![TtsEngine::BuiltIn, TtsEngine::System]
        );
        assert_eq!(
            v.stt_engine_ladder,
            vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode]
        );
        assert!(v.diarizer.is_empty());
        assert_eq!(v.activity_threshold, 0.5);
        assert_eq!(v.match_threshold, 0.65);
        assert!(!v.speaker_lock);
        assert_eq!(v.system_rate(), 1.0);
        assert_eq!(v.model_rate(TtsModel::Kokoro), 1.0);
        assert!(v.caps);
        assert_eq!(v.listen_mode, ListenMode::RecordSubmit);
        assert_eq!(v.hands_free.start, "computer");
        assert_eq!(v.hands_free.submit, "submit");
        assert_eq!(v.hands_free.cancel, "cancel");
        assert_eq!(v.submit_confirm_ms, 1000);
        assert_eq!(v.endpoint_silence_ms, 700);
        assert!(!v.full_duplex);
        assert_eq!(v.capture_gain, CaptureGain::Auto);
        assert!(!v.double_tap_submit);
        assert_eq!(v.paste_delay_ms, 100);
        assert_eq!(v.clear_on_input, vec![CancelSpeechScope::Current]);
        assert!(!v.pause_bg);
        #[cfg(target_os = "macos")]
        assert_eq!(v.earcon_reply, "Tink");
        #[cfg(target_os = "windows")]
        assert_eq!(v.earcon_reply, "ding");
        #[cfg(target_os = "linux")]
        assert_eq!(v.earcon_reply, "message");
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        assert!(v.earcon_reply.is_empty());
        assert!(v.earcon_input.is_empty());
        assert_eq!(
            v.provider,
            vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu]
        );
        assert_eq!(v.tray, vec![TrayKind::Stt, TrayKind::TtsAnimated]);
        assert!(v.codex_stream);
        assert!(!v.codex_daemon);
        assert!(v.codex_app_server_url.is_empty());
        assert_eq!(v.codex_bin, WiredAgent::Codex.as_str());
        assert!(v.grok_stream);
        assert!(v.extra_terminals.is_empty());
        assert!(v.extra_editors.is_empty());
        assert!(v.exclude_clients.is_none());
        assert!(!v.agents);
    }

    #[test]
    fn default_voice_pools_match_the_registry() {
        let cfg = VoiceConfig::default();
        for model in TtsModel::ALL.iter().copied() {
            assert_eq!(cfg.voices_for(model), model.descriptor().default_voices);
        }
    }

    #[test]
    fn tts_params_parse_per_entry_fail_open() {
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        assert!(v.tts_params.is_empty());
        assert!(VoiceConfig::known_keys().contains("tts_params"));
        let v: VoiceConfig = serde_json::from_str(
            r#"{"tts_params":{
                "system":{"rate":1.25,"language":"en"},
                "kokoro":{"rate":0.8,"language":"fr"},
                "chatterbox":{"exaggeration":1.5,"bogus":1.0},
                "qwen":{"repetition_penalty":9.0},
                "omnivoice":{"steps":8,"exaggeration":0.7},
                "wavenet":{"steps":8}
            }}"#,
        )
        .unwrap();
        assert_eq!(v.system_rate(), 1.25);
        assert_eq!(v.model_rate(TtsModel::Kokoro), 0.8);
        assert!(!v.tts_params.system.contains_key("language"));
        assert!(!v.tts_params.kokoro.contains_key("language"));
        assert_eq!(
            v.tts_params.chatterbox["exaggeration"],
            TtsParamValue::Float(1.5)
        );
        assert!(!v.tts_params.chatterbox.contains_key("bogus"));
        assert!(v.tts_params.qwen.is_empty(), "out-of-range drops");
        assert_eq!(v.tts_params.omnivoice["steps"], TtsParamValue::Int(8));
        assert!(!v.tts_params.omnivoice.contains_key("exaggeration"));
        for raw in [r#"{"tts_params":7}"#, r#"{"tts_params":{"qwen":"fast"}}"#] {
            let v: VoiceConfig = serde_json::from_str(raw).unwrap();
            assert!(v.tts_params.is_empty(), "{raw}");
        }
    }

    #[test]
    fn tts_params_round_trip_through_write_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let mut cfg = VoiceConfig::default();
        write_settings(&paths, &cfg).unwrap();
        let text = std::fs::read_to_string(&paths.config_toml).unwrap();
        assert!(!text.contains("tts_params"), "empty pools stay absent");
        cfg.tts_params
            .system
            .insert("rate".into(), TtsParamValue::Float(1.25));
        cfg.tts_params
            .for_model_mut(TtsModel::Kokoro)
            .insert("rate".into(), TtsParamValue::Float(0.8));
        cfg.tts_params
            .for_model_mut(TtsModel::Chatterbox)
            .insert("exaggeration".into(), TtsParamValue::Float(1.25));
        cfg.tts_params
            .for_model_mut(TtsModel::OmniVoice)
            .insert("steps".into(), TtsParamValue::Int(32));
        write_settings(&paths, &cfg).unwrap();
        let loaded = VoiceConfig::load(&paths);
        assert_eq!(loaded.tts_params, cfg.tts_params);
        let resolved = TtsModel::OmniVoice
            .descriptor()
            .resolve_params(loaded.tts_params.for_model(TtsModel::OmniVoice));
        assert_eq!(resolved.int(TtsModel::OmniVoice, "steps"), 32);
    }

    #[test]
    fn codex_stream_defaults_and_overrides() {
        // Defaults: the subscriber is ON (inert without ~/.codex + a running app-server),
        // lazy app-server start is OFF (no surprise spawns), endpoint = the default unix
        // control socket, binary = the canonical Codex launch command.
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        assert!(v.codex_stream);
        assert!(!v.codex_daemon);
        assert_eq!(v.codex_app_server_url, "");
        assert_eq!(v.codex_bin, WiredAgent::Codex.as_str());
        // All four are plain-typed overrides.
        let v: VoiceConfig = serde_json::from_str(
            r#"{"codex_stream":false,"codex_daemon":true,
                "codex_app_server_url":"ws://127.0.0.1:4550","codex_bin":"/opt/codex/bin/codex"}"#,
        )
        .unwrap();
        assert!(!v.codex_stream);
        assert!(v.codex_daemon);
        assert_eq!(v.codex_app_server_url, "ws://127.0.0.1:4550");
        assert_eq!(v.codex_bin, "/opt/codex/bin/codex");
        // The fields serialize, so `known_keys` covers them (no spurious unknown-key warns).
        let keys = VoiceConfig::known_keys();
        for k in [
            "codex_stream",
            "codex_daemon",
            "codex_app_server_url",
            "codex_bin",
        ] {
            assert!(keys.contains(k), "{k} must be a known config key");
        }
    }

    #[test]
    fn grok_stream_defaults_and_overrides() {
        // Default ON (inert without ~/.grok + a registered session); plain bool override.
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        assert!(v.grok_stream);
        let v: VoiceConfig = serde_json::from_str(r#"{"grok_stream":false}"#).unwrap();
        assert!(!v.grok_stream);
        assert!(
            VoiceConfig::known_keys().contains("grok_stream"),
            "grok_stream must be a known config key"
        );
        // Round-trip: non-default serializes and deserializes.
        let round = VoiceConfig {
            grok_stream: false,
            ..Default::default()
        };
        let toml = toml::to_string(&round).unwrap();
        assert!(toml.contains("grok_stream"));
        let back: VoiceConfig = toml::from_str(&toml).unwrap();
        assert!(!back.grok_stream);
    }

    #[test]
    fn agents_defaults_off_and_round_trips() {
        // Default OFF (opt-in: usage probing may touch the macOS keychain).
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        assert!(!v.agents);
        let v: VoiceConfig = serde_json::from_str(r#"{"agents":true}"#).unwrap();
        assert!(v.agents);
        assert!(
            VoiceConfig::known_keys().contains("agents"),
            "agents must be a known config key"
        );
        // Round-trips through write_settings + load (tempdir only).
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let cfg = VoiceConfig {
            agents: true,
            ..VoiceConfig::default()
        };
        write_settings(&paths, &cfg).unwrap();
        assert!(VoiceConfig::load(&paths).agents);
    }

    #[test]
    fn extra_terminals_and_custom_editors_default_empty_and_parse() {
        // Default: both empty (no escape-hatch entries out of the box).
        assert!(VoiceConfig::default().extra_terminals.is_empty());
        assert!(VoiceConfig::default().extra_editors.is_empty());

        // A flat pass-through list: no dedup/validation, order preserved.
        let v: VoiceConfig =
            serde_json::from_str(r#"{"extra_terminals":["foo"],"extra_editors":["bar.exe"]}"#)
                .unwrap();
        assert_eq!(v.extra_terminals, vec!["foo".to_string()]);
        assert_eq!(v.extra_editors, vec!["bar.exe".to_string()]);

        // Neither field is `skip_serializing_if`, so both appear in the default struct's
        // serialized table already — asserted explicitly so a future refactor that adds
        // `skip_serializing_if` here doesn't silently reintroduce a spurious "unknown key
        // in config.toml" warning.
        let keys = VoiceConfig::known_keys();
        assert!(keys.contains("extra_terminals"));
        assert!(keys.contains("extra_editors"));
    }

    #[test]
    fn exclude_clients_resolves_and_deserializes_fail_open() {
        // `excluded_clients`: absent (None) or explicit empty ⇒ exclude nothing; an explicit
        // single client ⇒ exactly that one excluded.
        assert_eq!(
            VoiceConfig::default().excluded_clients(),
            Vec::<WiredAgent>::new()
        );
        assert_eq!(
            VoiceConfig {
                exclude_clients: Some(vec![]),
                ..VoiceConfig::default()
            }
            .excluded_clients(),
            Vec::<WiredAgent>::new()
        );
        assert_eq!(
            VoiceConfig {
                exclude_clients: Some(vec![WiredAgent::ClaudeCode]),
                ..VoiceConfig::default()
            }
            .excluded_clients(),
            vec![WiredAgent::ClaudeCode]
        );

        // `de_exclude_clients` (via the config-file deserialize): a present ARRAY keeps known client
        // tokens in order, deduped, dropping unknown / non-client tokens.
        let wc = |j: &str| {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .exclude_clients
        };
        assert_eq!(
            wc(r#"{"exclude_clients":["claude","narration_spec","bogus","claude"]}"#),
            Some(vec![WiredAgent::ClaudeCode])
        );
        // A non-array value (or a bare string / number) degrades to None = exclude nothing.
        assert_eq!(wc(r#"{"exclude_clients":"claude"}"#), None);
        assert_eq!(wc(r#"{"exclude_clients":42}"#), None);
        // Absent ⇒ None via the serde default.
        assert_eq!(wc(r#"{}"#), None);

        // `known_keys()` covers it (it's skip_serializing_if, so absent from the default table).
        assert!(VoiceConfig::known_keys().contains("exclude_clients"));
    }

    #[test]
    fn exclude_clients_drops_unwired_tokens() {
        let wc = |j: &str| {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .exclude_clients
        };
        assert_eq!(
            wc(r#"{"exclude_clients":["not_a_wired_agent"]}"#),
            Some(vec![]),
            "unwired tokens must not enter the excluded-agent set"
        );
        // …and they're dropped from a mixed list without disturbing the wired agents' order.
        let mixed = serde_json::json!({
            "exclude_clients": [
                WiredAgent::Codex.as_str(),
                "not_a_wired_agent",
                WiredAgent::ClaudeCode.as_str(),
            ]
        });
        assert_eq!(
            wc(&mixed.to_string()),
            Some(vec![WiredAgent::Codex, WiredAgent::ClaudeCode])
        );
    }

    #[test]
    fn exclude_clients_round_trips_through_write_and_load() {
        // None serializes absent (≠ Some([])); tempdir only.
        for state in [None, Some(vec![]), Some(vec![WiredAgent::ClaudeCode])] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::rooted_at(dir.path());
            let cfg = VoiceConfig {
                exclude_clients: state.clone(),
                ..VoiceConfig::default()
            };
            write_settings(&paths, &cfg).unwrap();
            let loaded = VoiceConfig::load(&paths);
            assert_eq!(
                loaded.exclude_clients, state,
                "exclude_clients round-trip: {state:?}"
            );
        }
    }

    #[test]
    fn provider_is_an_ordered_ladder_failing_open_to_default() {
        let prov = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().provider;
        let default = vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu];
        assert_eq!(
            prov(r#"{"provider":["cuda","mlx","cpu"]}"#),
            vec![Provider::OrtCuda, Provider::Mlx, Provider::OrtCpu]
        );
        assert_eq!(prov(r#"{"provider":["cpu"]}"#), vec![Provider::OrtCpu]);
        assert_eq!(
            prov(r#"{"provider":["coreml","bogus","auto"]}"#),
            vec![Provider::OrtCoreMl]
        );
        assert_eq!(prov(r#"{"provider":[]}"#), default);
        assert_eq!(prov(r#"{"provider":"mlx"}"#), default);
        assert_eq!(prov(r#"{"provider":42}"#), default);

        for p in [
            Provider::OrtCpu,
            Provider::OrtCuda,
            Provider::OrtCoreMl,
            Provider::Mlx,
        ] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        for t in [TrayKind::Stt, TrayKind::Tts] {
            assert_eq!(TrayKind::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn provider_resolution_walks_the_ladder_per_platform() {
        // Only the macOS arm below exercises the native MLX rung; other
        // platforms have nothing platform-specific to assert here, so the helper is gated
        // with them to avoid an unused-closure warning off macOS.
        #[cfg(target_os = "macos")]
        let cfg = |rungs: Vec<Provider>| VoiceConfig {
            provider: rungs,
            ..VoiceConfig::default()
        };
        // STT resolution: first STT-usable rung, else CPU.
        #[cfg(target_os = "macos")]
        {
            // MLX is STT-usable only on Apple Silicon, so an
            // `[mlx, cpu]` wins on Apple Silicon but falls through to `cpu` on x86_64 —
            // Intel Mac gets the streaming ONNX path.
            #[cfg(target_arch = "aarch64")]
            assert_eq!(
                cfg(vec![Provider::Mlx, Provider::OrtCpu]).resolved_stt_provider(),
                Provider::Mlx
            );
            #[cfg(target_arch = "x86_64")]
            assert_eq!(
                cfg(vec![Provider::Mlx, Provider::OrtCpu]).resolved_stt_provider(),
                Provider::OrtCpu
            );
            // `uses_mlx_model` needs the Kokoro engine to actually RESOLVE, which only
            // happens where the built-in stack is usable (arm64 macOS) — on x86_64 macOS the
            // default TTS ladder falls through to `system`, so Kokoro never runs.
            #[cfg(target_arch = "aarch64")]
            {
                assert!(cfg(vec![Provider::Mlx]).uses_mlx_model());
                assert!(!cfg(vec![Provider::OrtCpu]).uses_mlx_model());
            }
        }
        // Default ladder always resolves to a concrete usable rung (never panics).
        let _ = VoiceConfig::default().resolved_stt_provider();
        let _ = VoiceConfig::default().tts_provider_token();
    }

    #[test]
    fn diarizer_provider_is_the_on_off_ladder() {
        let diar = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().diarizer;
        assert!(VoiceConfig::default().diarizer.is_empty());
        assert!(!VoiceConfig::default().is_diarization_on());
        let on = diar(r#"{"diarizer":["mlx"]}"#);
        assert_eq!(on, vec![DiarizerProvider::Mlx]);
        assert!(diar(r#"{"diarizer":["bogus"]}"#).is_empty());
        assert!(diar(r#"{"diarizer":[]}"#).is_empty());
        assert!(diar(r#"{"diarizer":"mlx"}"#).is_empty());

        let cfg = |r: Vec<DiarizerProvider>| VoiceConfig {
            diarizer: r,
            ..VoiceConfig::default()
        };
        assert!(cfg(vec![DiarizerProvider::Mlx]).is_diarization_on());
        let ladder = vec![DiarizerProvider::Mlx];
        assert_eq!(cfg(ladder).resolved_diarizer(), DiarizerProvider::Mlx);
    }

    #[test]
    fn tray_indicator_is_a_set_of_tokens() {
        let tray = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().tray;
        assert_eq!(
            tray(r#"{"tray":["stt","tts"]}"#),
            vec![TrayKind::Stt, TrayKind::Tts]
        );
        assert_eq!(tray(r#"{"tray":["tts"]}"#), vec![TrayKind::Tts]);
        assert!(tray(r#"{"tray":[]}"#).is_empty());
        assert_eq!(
            tray(r#"{"tray":["stt_animated","tts"]}"#),
            vec![TrayKind::SttAnimated, TrayKind::Tts]
        );
        assert_eq!(
            tray(r#"{"tray":["tts","tts_animated"]}"#),
            vec![TrayKind::TtsAnimated]
        );
        assert_eq!(
            tray(r#"{"tray":["tts","bogus","tts","stt"]}"#),
            vec![TrayKind::Stt, TrayKind::Tts]
        );
        for raw in [r#"{"tray":"stt"}"#, r#"{"tray":3}"#] {
            assert_eq!(
                serde_json::from_str::<VoiceConfig>(raw).unwrap().tray,
                vec![TrayKind::Stt, TrayKind::TtsAnimated],
                "{raw}"
            );
        }
    }

    #[test]
    fn listen_mode_parses_and_falls_back() {
        let p = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().listen_mode;
        assert_eq!(p(r#"{"listen_mode":"always"}"#), ListenMode::Always);
        assert_eq!(
            p(r#"{"listen_mode":"record_submit"}"#),
            ListenMode::RecordSubmit
        );
        assert_eq!(
            p(r#"{"listen_mode":"telepathy"}"#),
            ListenMode::RecordSubmit
        );
        assert_eq!(p(r#"{"listen_mode":9}"#), ListenMode::RecordSubmit);
    }

    #[test]
    fn always_listening_fields_parse() {
        let v: VoiceConfig = serde_json::from_str(
            r#"{"listen_mode":"always","hands_free":{"start":"hey","submit":"send it","cancel":"scrap"},"submit_confirm_ms":800,"endpoint_silence_ms":600}"#,
        )
        .unwrap();
        assert_eq!(v.listen_mode, ListenMode::Always);
        assert_eq!(v.hands_free.start, "hey");
        assert_eq!(v.hands_free.submit, "send it");
        assert_eq!(v.hands_free.cancel, "scrap");
        assert_eq!(v.submit_confirm_ms, 800);
        assert_eq!(v.endpoint_silence_ms, 600);
    }

    #[test]
    fn listen_mode_change_flagged() {
        let base = VoiceConfig::default();
        let m = VoiceConfig {
            listen_mode: ListenMode::Always,
            ..base.clone()
        };
        assert!(m.changes_since(&base).listen_mode_changed);
        // A wake-phrase change alone touches no warm subsystem (read fresh per turn).
        let w = VoiceConfig {
            hands_free: HandsFreePhrases {
                submit: "okay".into(),
                ..Default::default()
            },
            ..base.clone()
        };
        assert!(w.changes_since(&base).is_noop());
    }

    #[test]
    fn always_listening_value_roundtrips() {
        let v = sample_voice();
        let back = voice_from_value(voice_to_value(&v));
        assert_eq!(back.listen_mode, v.listen_mode);
        assert_eq!(back.hands_free, v.hands_free);
        assert_eq!(back.submit_confirm_ms, v.submit_confirm_ms);
        assert_eq!(back.endpoint_silence_ms, v.endpoint_silence_ms);
    }

    // ── Engine enum parsing ─────────────────────────────────────────────────

    #[test]
    fn stt_engine_ladder_is_an_ordered_ladder() {
        let p = |j: &str| -> Vec<SttEngine> {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .stt_engine_ladder
        };
        let default = vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode];
        // An explicit array keeps its order (deduped), known tokens only.
        assert_eq!(
            p(r#"{"stt_engine_ladder":["claude_code","built_in"]}"#),
            vec![SttEngine::ClaudeCode, SttEngine::BuiltIn]
        );
        // ARRAYS ONLY: a bare scalar string is NO LONGER a one-rung shorthand — it (known token
        // or not) degrades to the default ladder. `[]` is the only way to disable.
        assert_eq!(p(r#"{"stt_engine_ladder":"system"}"#), default);
        assert!(
            p(r#"{"stt_engine_ladder":[]}"#).is_empty(),
            "empty array = off"
        );
        // Unknown tokens drop from an array; an all-unknown / wrong-typed value (incl. a bare
        // scalar) falls open to the default ladder (never errors the block).
        assert_eq!(
            p(r#"{"stt_engine_ladder":["deepgram","built_in"]}"#),
            vec![SttEngine::BuiltIn]
        );
        assert_eq!(p(r#"{"stt_engine_ladder":"deepgram"}"#), default);
        assert_eq!(p(r#"{"stt_engine_ladder":3}"#), default);
    }

    #[test]
    fn tts_engine_ladder_is_an_ordered_ladder() {
        let p = |j: &str| -> Vec<TtsEngine> {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .tts_engine_ladder
        };
        let default = vec![TtsEngine::BuiltIn, TtsEngine::System];
        assert_eq!(
            p(r#"{"tts_engine_ladder":["system","built_in"]}"#),
            vec![TtsEngine::System, TtsEngine::BuiltIn]
        );
        // ARRAYS ONLY: a bare scalar string degrades to the default ladder (no one-rung
        // shorthand); `[]` is the only disable.
        assert_eq!(p(r#"{"tts_engine_ladder":"system"}"#), default);
        assert!(
            p(r#"{"tts_engine_ladder":[]}"#).is_empty(),
            "empty array = off"
        );
        assert_eq!(p(r#"{"tts_engine_ladder":"festival"}"#), default);
        assert_eq!(p(r#"{"tts_engine_ladder":9}"#), default);
    }

    #[test]
    fn tts_engine_preference_is_a_tristate_scalar_or_empty_array() {
        let p = |j: &str| -> Option<Vec<TtsEngine>> {
            serde_json::from_str::<VoiceConfig>(j).unwrap().tts_engine
        };
        // Absent ⇒ None (unset — defer to the ladder).
        assert_eq!(p("{}"), None);
        // A scalar token ⇒ Some(vec![engine]) — the forced single choice.
        assert_eq!(
            p(r#"{"tts_engine":"built_in"}"#),
            Some(vec![TtsEngine::BuiltIn])
        );
        assert_eq!(
            p(r#"{"tts_engine":"system"}"#),
            Some(vec![TtsEngine::System])
        );
        // An empty array ⇒ Some(vec![]) — explicit off.
        assert_eq!(p(r#"{"tts_engine":[]}"#), Some(Vec::new()));
        assert_eq!(p(r#"{"tts_engine":"off"}"#), Some(Vec::new()));
        // An unrecognized token or a non-empty array fails open to None (unset), never a
        // silent wrong choice.
        assert_eq!(p(r#"{"tts_engine":"festival"}"#), None);
        assert_eq!(p(r#"{"tts_engine":["built_in","system"]}"#), None);
    }

    #[test]
    fn stt_engine_preference_is_a_tristate_scalar_or_empty_array() {
        let p = |j: &str| -> Option<Vec<SttEngine>> {
            serde_json::from_str::<VoiceConfig>(j).unwrap().stt_engine
        };
        assert_eq!(p("{}"), None);
        assert_eq!(
            p(r#"{"stt_engine":"claude_code"}"#),
            Some(vec![SttEngine::ClaudeCode])
        );
        assert_eq!(p(r#"{"stt_engine":[]}"#), Some(Vec::new()));
        assert_eq!(p(r#"{"stt_engine":"off"}"#), Some(Vec::new()));
        assert_eq!(p(r#"{"stt_engine":"deepgram"}"#), None);
    }

    #[test]
    fn engine_preference_off_tokens_round_trip_as_empty_arrays() {
        let parsed: VoiceConfig =
            toml::from_str("tts_engine = \"off\"\nstt_engine = \"off\"\n").unwrap();
        assert_eq!(parsed.tts_engine, Some(Vec::new()));
        assert_eq!(parsed.stt_engine, Some(Vec::new()));
        assert_eq!(parsed.resolved_tts(), None);
        assert_eq!(parsed.resolved_stt(), None);

        let encoded = toml::to_string(&parsed).unwrap();
        let table: toml::Table = toml::from_str(&encoded).unwrap();
        for field in ["tts_engine", "stt_engine"] {
            assert!(
                table[field].as_array().is_some_and(Vec::is_empty),
                "{field} must serialize to the canonical [] off form"
            );
        }
        let round_trip: VoiceConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(round_trip.tts_engine, parsed.tts_engine);
        assert_eq!(round_trip.stt_engine, parsed.stt_engine);
    }

    #[test]
    fn resolved_engines_walk_the_ladder_first_usable() {
        // claude_code is always usable, so a default STT ladder always resolves to SOMETHING.
        assert!(VoiceConfig::default().resolved_stt().is_some());
        // An empty ladder = off (resolves to None) for both roles.
        let off = VoiceConfig {
            tts_engine_ladder: Vec::new(),
            stt_engine_ladder: Vec::new(),
            ..VoiceConfig::default()
        };
        assert!(off.resolved_tts().is_none() && !off.is_tts_on());
        assert!(off.resolved_stt().is_none());
        // Intel-macOS ORT-present/absent behavior is covered through an injected capability in
        // enums.rs. This resolver test deliberately avoids probing the developer's Homebrew keg.
    }

    #[test]
    fn resolved_tts_honors_ladder_order_when_multiple_usable() {
        // On a build where BOTH built_in (Kokoro) and system (`say`) can run, the FIRST listed
        // rung wins — proving resolution is preference-ORDERED, not a fixed priority.
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            target_os = "windows"
        ))]
        {
            let c = |rungs: Vec<TtsEngine>| VoiceConfig {
                tts_engine_ladder: rungs,
                ..VoiceConfig::default()
            };
            assert_eq!(
                c(vec![TtsEngine::System, TtsEngine::BuiltIn]).resolved_tts(),
                Some(TtsEngine::System)
            );
            assert_eq!(
                c(vec![TtsEngine::BuiltIn, TtsEngine::System]).resolved_tts(),
                Some(TtsEngine::BuiltIn)
            );
        }
    }

    #[test]
    fn resolved_tts_preference_wins_over_ladder_with_no_substitution() {
        // Unset preference (None) defers to the ladder.
        let deferring_engine = if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            TtsEngine::System
        } else {
            TtsEngine::BuiltIn
        };
        let deferring = VoiceConfig {
            tts_engine: None,
            tts_engine_ladder: vec![deferring_engine],
            ..VoiceConfig::default()
        };
        assert_eq!(
            deferring.resolved_tts(),
            deferring
                .tts_engine_ladder
                .first()
                .copied()
                .filter(|e| e.is_tts_usable())
        );

        // Explicit off (Some(vec![])) is off regardless of what the ladder would resolve to.
        let explicit_off = VoiceConfig {
            tts_engine: Some(Vec::new()),
            tts_engine_ladder: vec![TtsEngine::BuiltIn, TtsEngine::System],
            ..VoiceConfig::default()
        };
        assert_eq!(explicit_off.resolved_tts(), None);

        // An explicit choice that ISN'T usable here resolves to None — NEVER substituted with
        // a different engine, even though the ladder (if consulted) would resolve to one.
        // An explicit, USABLE choice wins outright, even when the ladder disagrees.
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            target_os = "windows"
        ))]
        {
            let explicit_system = VoiceConfig {
                tts_engine: Some(vec![TtsEngine::System]),
                tts_engine_ladder: vec![TtsEngine::BuiltIn],
                ..VoiceConfig::default()
            };
            assert_eq!(explicit_system.resolved_tts(), Some(TtsEngine::System));
        }
    }

    #[test]
    fn built_in_model_parse() {
        let selected: VoiceConfig = serde_json::from_str(
            r#"{"tts_engine":"built_in","tts_model":"chatterbox","tts_voices":{"chatterbox":["default"]}}"#,
        )
        .unwrap();
        assert_eq!(selected.tts_engine, Some(vec![TtsEngine::BuiltIn]));
        assert_eq!(selected.tts_model, TtsModel::Chatterbox);
        let expected_provider = if Provider::Mlx.is_tts_usable() {
            Provider::Mlx
        } else if Provider::OrtCuda.is_tts_usable() {
            Provider::OrtCuda
        } else {
            Provider::OrtCpu
        };
        assert_eq!(selected.resolved_tts_provider(), expected_provider);
        assert!(VoiceConfig::known_keys().contains("tts_model"));
        let wire = serde_json::to_value(&selected).unwrap();
        for absent in ["language", "tts_language", "rate"] {
            assert!(!VoiceConfig::known_keys().contains(absent));
            assert!(wire.get(absent).is_none(), "{absent} must not persist");
        }

        let changed = VoiceConfig {
            tts_model: TtsModel::Qwen,
            tts_voices: TtsVoicePools {
                qwen: vec!["ono_anna".into()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        assert!(changed.changes_since(&VoiceConfig::default()).tts_toggled);
    }

    #[test]
    fn resolved_stt_preference_wins_over_ladder_with_no_substitution() {
        // Unset preference (None) defers to the ladder.
        let deferring = VoiceConfig {
            stt_engine: None,
            stt_engine_ladder: vec![SttEngine::ClaudeCode],
            ..VoiceConfig::default()
        };
        assert_eq!(deferring.resolved_stt(), Some(SttEngine::ClaudeCode));

        // Explicit off, regardless of the ladder.
        let explicit_off = VoiceConfig {
            stt_engine: Some(Vec::new()),
            stt_engine_ladder: vec![SttEngine::ClaudeCode],
            ..VoiceConfig::default()
        };
        assert_eq!(explicit_off.resolved_stt(), None);

        // An explicit choice unusable here (system is macOS-only) resolves to off, never
        // substituting claude_code even though it's always usable.
        #[cfg(not(target_os = "macos"))]
        {
            let unusable_choice = VoiceConfig {
                stt_engine: Some(vec![SttEngine::System]),
                stt_engine_ladder: vec![SttEngine::ClaudeCode],
                ..VoiceConfig::default()
            };
            assert_eq!(
                unusable_choice.resolved_stt(),
                None,
                "unusable explicit choice resolves to off, never substitutes claude_code"
            );
        }

        // An explicit, always-usable choice (claude_code) wins outright.
        let explicit_claude = VoiceConfig {
            stt_engine: Some(vec![SttEngine::ClaudeCode]),
            stt_engine_ladder: vec![SttEngine::BuiltIn],
            ..VoiceConfig::default()
        };
        assert_eq!(explicit_claude.resolved_stt(), Some(SttEngine::ClaudeCode));
    }

    // ── Config parsing ──────────────────────────────────────────────────────

    #[test]
    fn double_tap_submits_is_a_plain_bool() {
        let sub = |j: &str| {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .double_tap_submit
        };
        // Absent ⇒ default off (a lone tap submits); explicit booleans pass through.
        assert!(!sub("{}"));
        assert!(sub(r#"{"double_tap_submit":true}"#));
        assert!(!sub(r#"{"double_tap_submit":false}"#));
    }

    #[test]
    fn narrate_is_a_set_of_tokens() {
        // The array form: known tokens kept in order, an empty array = narrate nothing.
        let both: VoiceConfig =
            serde_json::from_str(r#"{"narrate":["digests","shorts"]}"#).unwrap();
        assert_eq!(
            both.narrate,
            vec![NarrateKind::Digests, NarrateKind::Shorts]
        );
        assert!(both.narrates(NarrateKind::Digests) && both.narrates(NarrateKind::Shorts));

        let msgs: VoiceConfig = serde_json::from_str(r#"{"narrate":["digests"]}"#).unwrap();
        assert_eq!(msgs.narrate, vec![NarrateKind::Digests]);
        assert!(!msgs.narrates(NarrateKind::Shorts));

        let none: VoiceConfig = serde_json::from_str(r#"{"narrate":[]}"#).unwrap();
        assert!(none.narrate.is_empty(), "empty array narrates nothing");
    }

    #[test]
    fn narrate_drops_unknown_tokens_and_dedups() {
        let v: VoiceConfig =
            serde_json::from_str(r#"{"narrate":["shorts","loud","shorts","digests"]}"#).unwrap();
        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
    }

    #[test]
    fn narrate_non_array_falls_back_to_default() {
        for raw in [
            r#"{"narrate":true}"#,
            r#"{"narrate":"digests"}"#,
            r#"{"narrate":7}"#,
        ] {
            let v: VoiceConfig = serde_json::from_str(raw).unwrap();
            assert_eq!(
                v.narrate,
                vec![NarrateKind::Shorts, NarrateKind::Digests],
                "{raw}"
            );
        }
    }

    #[test]
    fn narrate_extra_fields_parse() {
        let v: VoiceConfig = serde_json::from_str(
            r#"{"narrate":["digests"],"skip_ahead_secs":8,"long_press_ms":750}"#,
        )
        .unwrap();
        assert_eq!(v.narrate, vec![NarrateKind::Digests]);
        assert_eq!(v.long_press_ms, 750);
    }

    /// A non-default config so the merge is observably distinct from defaults.
    pub(crate) fn sample_voice() -> VoiceConfig {
        VoiceConfig {
            tts_voices: TtsVoicePools {
                system: vec!["Samantha (Enhanced)".into()],
                kokoro: vec!["am_michael".into(), "am_adam".into()],
                chatterbox: vec!["default".into()],
                qwen: vec!["sohee".into()],
                omnivoice: vec!["deep_man".into()],
            },
            tts_model: TtsModel::Kokoro,
            tts_params: TtsParamPools {
                system: [("rate".to_string(), TtsParamValue::Float(1.25))].into(),
                kokoro: [("rate".to_string(), TtsParamValue::Float(0.8))].into(),
                omnivoice: [("steps".to_string(), TtsParamValue::Int(32))].into(),
                ..Default::default()
            },
            greet: true,
            stt_engine: None,
            stt_engine_ladder: vec![SttEngine::BuiltIn],
            diarizer: vec![DiarizerProvider::Mlx],
            activity_threshold: 0.55,
            match_threshold: 0.7,
            speaker_lock: false,
            tts_engine: None,
            tts_engine_ladder: vec![TtsEngine::System],
            provider: vec![Provider::OrtCpu],
            narrate: vec![NarrateKind::Digests],
            long_press_ms: 750,
            caps: false,
            tray: vec![TrayKind::Stt],
            listen_mode: ListenMode::Always,
            hands_free: HandsFreePhrases {
                submit: "go ahead".into(),
                ..Default::default()
            },
            submit_confirm_ms: 1200,
            endpoint_silence_ms: 650,
            full_duplex: true,
            capture_gain: CaptureGain::Manual(2.5),
            double_tap_submit: true, // non-default (default is false)
            paste_delay_ms: 150,     // non-default (default is 100)
            clear_on_input: vec![CancelSpeechScope::Other], // non-default (default is [current])
            pause_bg: true,          // non-default (default is false)
            earcon_reply: "Glass".into(), // non-default (default is the OS chime)
            earcon_input: "Funk".into(),
            codex_stream: false, // non-default (default is true)
            codex_daemon: true,  // non-default (default is false)
            codex_app_server_url: "ws://127.0.0.1:4550".into(), // non-default (default is empty)
            codex_bin: "/opt/codex/bin/codex".into(), // non-default launch command
            grok_stream: false,  // non-default (default is true)
            extra_terminals: vec!["myterm".into()], // non-default (default is [])
            extra_editors: vec!["myeditor.exe".into()], // non-default (default is [])
            exclude_clients: Some(vec![WiredAgent::ClaudeCode]), // non-default (default is None)
            agents: true,        // non-default (default is false)
        }
    }

    #[test]
    fn changes_since_flags_only_what_changed() {
        let base = VoiceConfig::default();

        let only_voice = VoiceConfig {
            tts_voices: TtsVoicePools {
                kokoro: vec!["am_michael".into()],
                ..Default::default()
            },
            tts_params: TtsParamPools {
                kokoro: [("rate".to_string(), TtsParamValue::Float(1.5))].into(),
                ..Default::default()
            },
            ..base.clone()
        };
        assert!(only_voice.changes_since(&base).is_noop());

        // Each toggle/engine field flags exactly its subsystem.
        let caps = VoiceConfig {
            caps: !base.caps,
            ..base.clone()
        };
        assert!(caps.changes_since(&base).caps_toggled);

        // Disabling TTS (explicit-off preference) changes the resolved engine on every platform.
        let tts = VoiceConfig {
            tts_engine: Some(Vec::new()), // off; base default has a usable rung
            ..base.clone()
        };
        assert!(tts.changes_since(&base).tts_toggled);

        // changes_since diffs the RESOLVED engine: disabling dictation (explicit-off
        // preference) flips stt_changed regardless of which on-device rungs are usable on
        // this build.
        let eng = VoiceConfig {
            stt_engine: Some(Vec::new()), // off; base default resolves to a usable engine
            ..base.clone()
        };
        assert!(eng.changes_since(&base).stt_changed);
    }

    #[test]
    fn write_settings_atomic_roundtrip_on_disk() {
        // The disk wrapper: write our config into a temp our config.toml, then
        // load() it back. Uses a tempdir so it never touches the live config.
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        // Seed an existing config with a hand-added key to prove a voice write preserves
        // the file's other keys.
        std::fs::write(&cfg, "custom_key = \"keep\"\n").unwrap();

        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = cfg.clone();

        let v = sample_voice();
        write_settings(&paths, &v).unwrap();

        // Re-read raw TOML to confirm the unrelated key survived the voice write.
        let raw: serde_json::Value =
            toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(
            raw["custom_key"],
            serde_json::json!("keep"),
            "hand-added key preserved"
        );

        // And load() reconstructs the written config.
        let lv = VoiceConfig::load(&paths);
        assert_eq!(lv.tts_voices, v.tts_voices);
        assert_eq!(lv.extra_terminals, v.extra_terminals);
        assert_eq!(lv.extra_editors, v.extra_editors);
    }

    #[test]
    fn write_settings_tolerates_missing_file() {
        // No existing config.toml at all → write creates it, load reads it back.
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("nested").join("config.toml");

        write_settings(&paths, &VoiceConfig::default()).unwrap();
        assert!(paths.config_toml.is_file());
        let lv = VoiceConfig::load(&paths);
        assert_eq!(
            lv.stt_engine_ladder,
            vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode]
        );
    }

    #[test]
    fn config_toml_is_native_typed_round_trip() {
        // Write a non-default config, then re-LOAD it from the TOML file: every enum
        // token + the numeric capture_gain must survive a typed TOML round-trip (no
        // JSON in between). Also assert the on-disk text is native TOML, not JSON.
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");

        let v = VoiceConfig {
            stt_engine_ladder: vec![SttEngine::BuiltIn],
            tts_engine_ladder: vec![TtsEngine::System],
            narrate: Vec::new(),
            full_duplex: true,
            tts_params: TtsParamPools {
                system: [("rate".to_string(), TtsParamValue::Float(1.25))].into(),
                kokoro: [("rate".to_string(), TtsParamValue::Float(0.8))].into(),
                ..Default::default()
            },
            capture_gain: CaptureGain::Manual(3.5),
            tts_voices: TtsVoicePools {
                kokoro: vec!["am_adam".into(), "af_bella".into()],
                ..Default::default()
            },
            ..VoiceConfig::default()
        };
        write_settings(&paths, &v).unwrap();

        let text = std::fs::read_to_string(&paths.config_toml).unwrap();
        assert!(
            text.contains("stt_engine_ladder = [\"built_in\"]"),
            "native TOML array of tokens, got:\n{text}"
        );
        assert!(
            !text.trim_start().starts_with('{'),
            "must be TOML, not JSON"
        );
        assert!(
            text.contains("capture_gain = 3.5"),
            "manual gain as a TOML number"
        );

        let r = VoiceConfig::load(&paths);
        assert_eq!(r.stt_engine_ladder, vec![SttEngine::BuiltIn]);
        assert_eq!(r.tts_engine_ladder, vec![TtsEngine::System]);
        assert!(
            r.narrate.is_empty(),
            "empty narrate set round-trips through TOML"
        );
        assert!(r.full_duplex);
        assert_eq!(r.system_rate(), 1.25);
        assert_eq!(r.model_rate(TtsModel::Kokoro), 0.8);
        assert_eq!(r.capture_gain.manual(), Some(3.5));
        assert_eq!(r.active_voices(), ["am_adam", "af_bella"]);
    }

    #[test]
    fn load_drops_out_of_range_per_target_rates() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        std::fs::write(
            &paths.config_toml,
            "[tts_params.system]\nrate = 5.0\n[tts_params.kokoro]\nrate = 0.1\n",
        )
        .unwrap();
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.system_rate(), 1.0);
        assert_eq!(cfg.model_rate(TtsModel::Kokoro), 1.0);
    }

    #[test]
    fn load_restores_default_pool_when_empty() {
        // A hand-edited empty voice pool fails open to the default pool on load —
        // `set_config` already rejects it, so a LOADED config never has zero voices.
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        std::fs::write(
            &paths.config_toml,
            "[tts_voices]\nsystem = []\nkokoro = []\nchatterbox = []\nqwen = []\nomnivoice = []\n",
        )
        .unwrap();
        let cfg = VoiceConfig::load(&paths);
        for model in TtsModel::ALL.iter().copied() {
            assert_eq!(cfg.voices_for(model), model.descriptor().default_voices);
        }
    }

    #[test]
    fn load_normalizes_language_and_removes_invalid_fixed_voices() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        std::fs::write(
            &paths.config_toml,
            "tts_model = \"qwen\"\n[tts_voices]\nqwen = [\"bogus\", \"ono_anna\"]\n",
        )
        .unwrap();
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.tts_voices.qwen, ["ono_anna"]);
    }

    #[test]
    fn load_tolerates_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        // A typo'd key is ignored by the typed parse; the known key still loads.
        std::fs::write(
            &paths.config_toml,
            "narate = \"off\"\nstt_engine_ladder = [\"built_in\"]\n",
        )
        .unwrap();
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(
            cfg.stt_engine_ladder,
            vec![SttEngine::BuiltIn],
            "array ladder ['built_in'] loads as a one-rung ladder"
        );
        assert_eq!(
            cfg.narrate,
            vec![NarrateKind::Shorts, NarrateKind::Digests],
            "typo'd 'narate' ignored → default narration set"
        );
        assert!(VoiceConfig::known_keys().contains("stt_engine_ladder"));
        assert!(!VoiceConfig::known_keys().contains("narate"));
    }

    #[test]
    fn load_logs_each_config_diagnostic_once_until_content_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        let invalid = "narate = \"off\"\n[tts_params.qwen]\nbogus = 1.0\n";
        std::fs::create_dir_all(paths.config_toml.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_toml, invalid).unwrap();

        VoiceConfig::load(&paths);
        VoiceConfig::load(&paths);
        let first = std::fs::read_to_string(&paths.log_file).unwrap();
        assert_eq!(first.matches("unknown key in config.toml").count(), 1);
        assert_eq!(first.matches("tts_params.qwen").count(), 1);

        std::fs::write(&paths.config_toml, "greet = true\n").unwrap();
        VoiceConfig::load(&paths);
        std::fs::write(&paths.config_toml, invalid).unwrap();
        VoiceConfig::load(&paths);
        let second = std::fs::read_to_string(&paths.log_file).unwrap();
        assert_eq!(second.matches("unknown key in config.toml").count(), 2);
        assert_eq!(second.matches("tts_params.qwen").count(), 2);
    }

    #[test]
    fn known_keys_includes_the_preference_fields_despite_defaulting_to_none() {
        // `tts_engine`/`stt_engine` (the preference fields) default to `None` and are
        // `skip_serializing_if`'d, so they're ABSENT from the default config's serialized
        // TOML table — `known_keys()` must insert them explicitly, or a user hand-setting
        // `tts_engine = "built_in"` would spuriously log an "unknown key" warning.
        assert!(VoiceConfig::known_keys().contains("tts_engine"));
        assert!(VoiceConfig::known_keys().contains("stt_engine"));

        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(dir.path());
        paths.config_toml = dir.path().join("config.toml");
        std::fs::write(&paths.config_toml, "tts_engine = \"built_in\"\n").unwrap();
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(cfg.tts_engine, Some(vec![TtsEngine::BuiltIn]));
    }
}
