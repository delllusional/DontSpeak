//! Active STT/TTS engine-object slots — single source for every status UI.
//!
//! Config tokens (`stt_engine` / `tts_engine`) select which `model_status` engine object
//! drives the TTS/STT row. Hosts map the slot to their DTO field; display names stay in
//! `ds-i18n`. `"off"` / unknown → [`None`] (row shows empty/off, not a wrong engine).

/// Which `model_status` engine object is the active TTS backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActiveTtsSlot {
    /// `tts_engine == "built_in"` → `kokoro` object.
    Kokoro,
    /// `tts_engine == "system"` → `tts_system` object.
    TtsSystem,
}

impl ActiveTtsSlot {
    pub const ALL: [ActiveTtsSlot; 2] = [ActiveTtsSlot::Kokoro, ActiveTtsSlot::TtsSystem];

    /// Wire / FFI token (`kokoro` | `tts_system`) — the model_status object key.
    pub fn as_str(self) -> &'static str {
        match self {
            ActiveTtsSlot::Kokoro => "kokoro",
            ActiveTtsSlot::TtsSystem => "tts_system",
        }
    }

    /// Config token (`built_in` | `system`) → slot. `"off"` / unknown → [`None`].
    pub fn from_engine(tts_engine: &str) -> Option<ActiveTtsSlot> {
        match tts_engine {
            "built_in" => Some(ActiveTtsSlot::Kokoro),
            "system" => Some(ActiveTtsSlot::TtsSystem),
            _ => None,
        }
    }
}

/// Which `model_status` engine object is the active STT backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActiveSttSlot {
    /// `stt_engine == "built_in"` → `parakeet` object.
    Parakeet,
    /// `stt_engine == "claude_code"` → `claude_code` object.
    ClaudeCode,
    /// `stt_engine == "system"` → `system` object.
    System,
}

impl ActiveSttSlot {
    pub const ALL: [ActiveSttSlot; 3] = [
        ActiveSttSlot::Parakeet,
        ActiveSttSlot::ClaudeCode,
        ActiveSttSlot::System,
    ];

    /// Wire / FFI token (`parakeet` | `claude_code` | `system`) — the model_status object key.
    pub fn as_str(self) -> &'static str {
        match self {
            ActiveSttSlot::Parakeet => "parakeet",
            ActiveSttSlot::ClaudeCode => "claude_code",
            ActiveSttSlot::System => "system",
        }
    }

    /// Config token (`built_in` | `claude_code` | `system`) → slot. `"off"` / unknown → [`None`].
    pub fn from_engine(stt_engine: &str) -> Option<ActiveSttSlot> {
        match stt_engine {
            "built_in" => Some(ActiveSttSlot::Parakeet),
            "claude_code" => Some(ActiveSttSlot::ClaudeCode),
            "system" => Some(ActiveSttSlot::System),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tts_config_tokens_map_to_object_keys() {
        assert_eq!(
            ActiveTtsSlot::from_engine("built_in").map(ActiveTtsSlot::as_str),
            Some("kokoro")
        );
        assert_eq!(
            ActiveTtsSlot::from_engine("system").map(ActiveTtsSlot::as_str),
            Some("tts_system")
        );
        assert_eq!(ActiveTtsSlot::from_engine("off"), None);
        assert_eq!(ActiveTtsSlot::from_engine(""), None);
        assert_eq!(ActiveTtsSlot::from_engine("kokoro"), None); // object key ≠ config token
    }

    #[test]
    fn stt_config_tokens_map_to_object_keys() {
        assert_eq!(
            ActiveSttSlot::from_engine("built_in").map(ActiveSttSlot::as_str),
            Some("parakeet")
        );
        assert_eq!(
            ActiveSttSlot::from_engine("claude_code").map(ActiveSttSlot::as_str),
            Some("claude_code")
        );
        assert_eq!(
            ActiveSttSlot::from_engine("system").map(ActiveSttSlot::as_str),
            Some("system")
        );
        assert_eq!(ActiveSttSlot::from_engine("off"), None);
        assert_eq!(ActiveSttSlot::from_engine("parakeet"), None);
    }

    #[test]
    fn slot_tokens_are_stable_wire_strings() {
        let tts: Vec<&str> = ActiveTtsSlot::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(tts, ["kokoro", "tts_system"]);
        let stt: Vec<&str> = ActiveSttSlot::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(stt, ["parakeet", "claude_code", "system"]);
    }
}
