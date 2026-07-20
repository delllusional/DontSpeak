//! Built-in TTS model identity on the `model_status` wire.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Selected built-in model. Unknown wire tokens fail deserialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusTtsModel {
    Kokoro,
    Chatterbox,
    Qwen,
    OmniVoice,
}

impl StatusTtsModel {
    pub const ALL: [StatusTtsModel; 4] = [
        StatusTtsModel::Kokoro,
        StatusTtsModel::Chatterbox,
        StatusTtsModel::Qwen,
        StatusTtsModel::OmniVoice,
    ];

    pub const TOKENS: &'static [&'static str] = &["kokoro", "chatterbox", "qwen", "omnivoice"];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusTtsModel::Kokoro => "kokoro",
            StatusTtsModel::Chatterbox => "chatterbox",
            StatusTtsModel::Qwen => "qwen",
            StatusTtsModel::OmniVoice => "omnivoice",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "kokoro" => StatusTtsModel::Kokoro,
            "chatterbox" => StatusTtsModel::Chatterbox,
            "qwen" => StatusTtsModel::Qwen,
            "omnivoice" => StatusTtsModel::OmniVoice,
            _ => return None,
        })
    }
}

impl FromStr for StatusTtsModel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

impl Serialize for StatusTtsModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StatusTtsModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = String::deserialize(deserializer)?;
        Self::parse(&token).ok_or_else(|| serde::de::Error::unknown_variant(&token, Self::TOKENS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_round_trip_and_unknown_fails() {
        for model in StatusTtsModel::ALL {
            assert_eq!(StatusTtsModel::parse(model.as_str()), Some(model));
            let json = serde_json::to_value(model).unwrap();
            assert_eq!(json, model.as_str());
            assert_eq!(
                serde_json::from_value::<StatusTtsModel>(json).unwrap(),
                model
            );
        }
        assert!(serde_json::from_value::<StatusTtsModel>(serde_json::json!("unknown")).is_err());
    }
}
