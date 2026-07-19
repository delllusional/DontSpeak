//! Tray icon kind + indicator tokens — single source for every host.
//!
//! Color only when `tray` lists STT/TTS kinds. Empty ⇒ idle.
//! Recording beats speaking (full-duplex). Download/warm stay on engine dots, not tray.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Config/status `tray` tokens (`stt` / `tts` / `*_animated`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusTrayKind {
    Stt,
    Tts,
    SttAnimated,
    TtsAnimated,
}

impl StatusTrayKind {
    pub const ALL: [StatusTrayKind; 4] = [
        StatusTrayKind::Stt,
        StatusTrayKind::Tts,
        StatusTrayKind::SttAnimated,
        StatusTrayKind::TtsAnimated,
    ];

    pub const TOKENS: &'static [&'static str] =
        &["stt", "tts", "stt_animated", "tts_animated"];

    pub fn as_str(self) -> &'static str {
        match self {
            StatusTrayKind::Stt => "stt",
            StatusTrayKind::Tts => "tts",
            StatusTrayKind::SttAnimated => "stt_animated",
            StatusTrayKind::TtsAnimated => "tts_animated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "stt" => StatusTrayKind::Stt,
            "tts" => StatusTrayKind::Tts,
            "stt_animated" => StatusTrayKind::SttAnimated,
            "tts_animated" => StatusTrayKind::TtsAnimated,
            _ => return None,
        })
    }

    pub fn is_stt(self) -> bool {
        matches!(self, StatusTrayKind::Stt | StatusTrayKind::SttAnimated)
    }

    pub fn is_tts(self) -> bool {
        matches!(self, StatusTrayKind::Tts | StatusTrayKind::TtsAnimated)
    }
}

impl Serialize for StatusTrayKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for StatusTrayKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::unknown_variant(&s, Self::TOKENS))
    }
}

/// Resolved tray/state-stripe icon. Wire tokens via [`TrayIconKind::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayIconKind {
    Idle,
    Recording,
    Speaking,
}

impl TrayIconKind {
    pub const ALL: [TrayIconKind; 3] = [
        TrayIconKind::Idle,
        TrayIconKind::Recording,
        TrayIconKind::Speaking,
    ];

    /// `idle` | `recording` | `speaking`.
    pub fn as_str(self) -> &'static str {
        match self {
            TrayIconKind::Idle => "idle",
            TrayIconKind::Recording => "recording",
            TrayIconKind::Speaking => "speaking",
        }
    }

    pub fn parse(s: &str) -> Option<TrayIconKind> {
        Some(match s {
            "idle" => TrayIconKind::Idle,
            "recording" => TrayIconKind::Recording,
            "speaking" => TrayIconKind::Speaking,
            _ => return None,
        })
    }
}

impl Serialize for TrayIconKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TrayIconKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| {
            serde::de::Error::unknown_variant(&s, &["idle", "recording", "speaking"])
        })
    }
}

/// `stt_active` = Caps dictation (not always-on). `tts_active` = playback.
pub fn tray_icon_kind(
    stt_active: bool,
    tts_active: bool,
    tray: &[StatusTrayKind],
) -> TrayIconKind {
    if stt_active && tray.iter().any(|k| k.is_stt()) {
        return TrayIconKind::Recording;
    }
    if tts_active && tray.iter().any(|k| k.is_tts()) {
        return TrayIconKind::Speaking;
    }
    TrayIconKind::Idle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_parse_round_trips() {
        for v in TrayIconKind::ALL {
            assert_eq!(TrayIconKind::parse(v.as_str()), Some(v));
        }
        assert_eq!(TrayIconKind::parse(""), None);
        assert_eq!(TrayIconKind::parse("Recording"), None);
        for v in StatusTrayKind::ALL {
            assert_eq!(StatusTrayKind::parse(v.as_str()), Some(v));
            let j = serde_json::to_value(v).unwrap();
            assert_eq!(serde_json::from_value::<StatusTrayKind>(j).unwrap(), v);
        }
        assert!(serde_json::from_value::<StatusTrayKind>(serde_json::json!("both")).is_err());
    }

    #[test]
    fn empty_indicator_never_colors() {
        assert_eq!(tray_icon_kind(true, true, &[]), TrayIconKind::Idle);
    }

    #[test]
    fn stt_and_tts_tokens_enable_color() {
        let ind = [StatusTrayKind::Stt, StatusTrayKind::Tts];
        assert_eq!(tray_icon_kind(true, false, &ind), TrayIconKind::Recording);
        assert_eq!(tray_icon_kind(false, true, &ind), TrayIconKind::Speaking);
        assert_eq!(tray_icon_kind(false, false, &ind), TrayIconKind::Idle);
    }

    #[test]
    fn animated_forms_count_as_enabled() {
        let ind = [StatusTrayKind::SttAnimated, StatusTrayKind::TtsAnimated];
        assert_eq!(tray_icon_kind(true, false, &ind), TrayIconKind::Recording);
        assert_eq!(tray_icon_kind(false, true, &ind), TrayIconKind::Speaking);
    }

    #[test]
    fn recording_wins_over_speaking() {
        let ind = [StatusTrayKind::Stt, StatusTrayKind::Tts];
        assert_eq!(tray_icon_kind(true, true, &ind), TrayIconKind::Recording);
    }

    #[test]
    fn only_configured_side_colors() {
        let stt_only = [StatusTrayKind::Stt];
        assert_eq!(
            tray_icon_kind(true, true, &stt_only),
            TrayIconKind::Recording
        );
        assert_eq!(tray_icon_kind(false, true, &stt_only), TrayIconKind::Idle);
        let tts_only = [StatusTrayKind::Tts];
        assert_eq!(
            tray_icon_kind(true, true, &tts_only),
            TrayIconKind::Speaking
        );
    }
}
