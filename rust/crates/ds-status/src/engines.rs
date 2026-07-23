//! Status-wire STT/TTS engine tokens — config tokens plus synthetic `"off"`.
//!
//! Config enums (`ds-config::{Stt,Tts}Engine`) have no `Off` variant (preference empty
//! vec means off). Status always emits a concrete token. Wire tokens match config
//! `as_str()` when not off. Unknown tokens fail deserialize (no fail-open).

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn unknown_engine<'de, D: Deserializer<'de>>(
    token: &str,
    expected: &'static [&'static str],
) -> D::Error {
    serde::de::Error::unknown_variant(token, expected)
}

/// STT engine identity on `model_status.stt.engine` (includes status-only `"off"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum StatusSttEngine {
    BuiltIn,
    System,
    ClaudeCode,
    #[default]
    Off,
}

impl StatusSttEngine {
    pub const ALL: [StatusSttEngine; 4] = [
        StatusSttEngine::BuiltIn,
        StatusSttEngine::System,
        StatusSttEngine::ClaudeCode,
        StatusSttEngine::Off,
    ];

    pub const TOKENS: &'static [&'static str] = &["built_in", "system", "claude_code", "off"];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusSttEngine::BuiltIn => "built_in",
            StatusSttEngine::System => "system",
            StatusSttEngine::ClaudeCode => "claude_code",
            StatusSttEngine::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "built_in" => StatusSttEngine::BuiltIn,
            "system" => StatusSttEngine::System,
            "claude_code" => StatusSttEngine::ClaudeCode,
            "off" => StatusSttEngine::Off,
            _ => return None,
        })
    }
}

impl FromStr for StatusSttEngine {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

impl Serialize for StatusSttEngine {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StatusSttEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| unknown_engine::<D>(&s, Self::TOKENS))
    }
}

/// TTS engine identity on `model_status.tts.engine` (includes status-only `"off"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum StatusTtsEngine {
    BuiltIn,
    System,
    #[default]
    Off,
}

impl StatusTtsEngine {
    pub const ALL: [StatusTtsEngine; 3] = [
        StatusTtsEngine::BuiltIn,
        StatusTtsEngine::System,
        StatusTtsEngine::Off,
    ];

    pub const TOKENS: &'static [&'static str] = &["built_in", "system", "off"];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusTtsEngine::BuiltIn => "built_in",
            StatusTtsEngine::System => "system",
            StatusTtsEngine::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "built_in" => StatusTtsEngine::BuiltIn,
            "system" => StatusTtsEngine::System,
            "off" => StatusTtsEngine::Off,
            _ => return None,
        })
    }
}

impl FromStr for StatusTtsEngine {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

impl Serialize for StatusTtsEngine {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StatusTtsEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| unknown_engine::<D>(&s, Self::TOKENS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_tokens_round_trip_and_pin() {
        for v in StatusSttEngine::ALL {
            assert_eq!(StatusSttEngine::parse(v.as_str()), Some(v));
            let j = serde_json::to_value(v).unwrap();
            assert_eq!(j, v.as_str());
            assert_eq!(serde_json::from_value::<StatusSttEngine>(j).unwrap(), v);
        }
        assert!(serde_json::from_value::<StatusSttEngine>(serde_json::json!("nope")).is_err());
    }

    #[test]
    fn tts_tokens_round_trip_and_pin() {
        for v in StatusTtsEngine::ALL {
            assert_eq!(StatusTtsEngine::parse(v.as_str()), Some(v));
            let j = serde_json::to_value(v).unwrap();
            assert_eq!(j, v.as_str());
            assert_eq!(serde_json::from_value::<StatusTtsEngine>(j).unwrap(), v);
        }
        assert!(serde_json::from_value::<StatusTtsEngine>(serde_json::json!("nope")).is_err());
    }
}
