//! Active STT/TTS `model_status` engine slots — single source for every status UI.
//!
//! Config tokens (`stt_engine` / `tts_engine`) → object key. Hosts map the slot; labels in
//! `ds-i18n`. `"off"` / unknown → [`None`].

/// Active TTS backend object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActiveTtsSlot {
    Kokoro,
    TtsSystem,
}

impl ActiveTtsSlot {
    pub const ALL: [ActiveTtsSlot; 2] = [ActiveTtsSlot::Kokoro, ActiveTtsSlot::TtsSystem];

    /// model_status object key: `kokoro` | `tts_system`.
    pub fn as_str(self) -> &'static str {
        match self {
            ActiveTtsSlot::Kokoro => "kokoro",
            ActiveTtsSlot::TtsSystem => "tts_system",
        }
    }

    /// Config `built_in` | `system` → slot.
    pub fn from_engine(tts_engine: &str) -> Option<ActiveTtsSlot> {
        match tts_engine {
            "built_in" => Some(ActiveTtsSlot::Kokoro),
            "system" => Some(ActiveTtsSlot::TtsSystem),
            _ => None,
        }
    }
}

/// Active STT backend object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActiveSttSlot {
    Parakeet,
    ClaudeCode,
    System,
}

impl ActiveSttSlot {
    pub const ALL: [ActiveSttSlot; 3] = [
        ActiveSttSlot::Parakeet,
        ActiveSttSlot::ClaudeCode,
        ActiveSttSlot::System,
    ];

    /// model_status object key: `parakeet` | `claude_code` | `system`.
    pub fn as_str(self) -> &'static str {
        match self {
            ActiveSttSlot::Parakeet => "parakeet",
            ActiveSttSlot::ClaudeCode => "claude_code",
            ActiveSttSlot::System => "system",
        }
    }

    /// Config `built_in` | `claude_code` | `system` → slot.
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
