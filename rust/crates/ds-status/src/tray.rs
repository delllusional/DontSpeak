//! Tray / state-stripe icon kind — single source for every host.
//!
//! Color only when `tray_indicator` lists `stt`/`tts` (or `*_animated`). Empty ⇒ idle.
//! Recording beats speaking (full-duplex). Download/warm stay on engine dots, not tray.

/// Wire tokens via [`TrayIconKind::as_str`].
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

/// True if `tray_indicator` enables color for `base` (`stt`/`tts`), including `_animated`.
fn colors_for(tray_indicator: &[impl AsRef<str>], base: &str) -> bool {
    tray_indicator.iter().any(|t| {
        let t = t.as_ref();
        t == base || t.strip_prefix(base).is_some_and(|rest| rest == "_animated")
    })
}

/// `stt_active` = Caps dictation (not always-on). `tts_active` = playback.
pub fn tray_icon_kind(
    stt_active: bool,
    tts_active: bool,
    tray_indicator: &[impl AsRef<str>],
) -> TrayIconKind {
    if stt_active && colors_for(tray_indicator, "stt") {
        return TrayIconKind::Recording;
    }
    if tts_active && colors_for(tray_indicator, "tts") {
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
    }

    #[test]
    fn empty_indicator_never_colors() {
        assert_eq!(
            tray_icon_kind(true, true, &[] as &[&str]),
            TrayIconKind::Idle
        );
    }

    #[test]
    fn stt_and_tts_tokens_enable_color() {
        let ind = ["stt", "tts"];
        assert_eq!(tray_icon_kind(true, false, &ind), TrayIconKind::Recording);
        assert_eq!(tray_icon_kind(false, true, &ind), TrayIconKind::Speaking);
        assert_eq!(tray_icon_kind(false, false, &ind), TrayIconKind::Idle);
    }

    #[test]
    fn animated_forms_count_as_enabled() {
        let ind = ["stt_animated", "tts_animated"];
        assert_eq!(tray_icon_kind(true, false, &ind), TrayIconKind::Recording);
        assert_eq!(tray_icon_kind(false, true, &ind), TrayIconKind::Speaking);
    }

    #[test]
    fn recording_wins_over_speaking() {
        let ind = ["stt", "tts"];
        assert_eq!(tray_icon_kind(true, true, &ind), TrayIconKind::Recording);
    }

    #[test]
    fn only_configured_side_colors() {
        let stt_only = ["stt"];
        assert_eq!(
            tray_icon_kind(true, true, &stt_only),
            TrayIconKind::Recording
        );
        assert_eq!(tray_icon_kind(false, true, &stt_only), TrayIconKind::Idle);
        let tts_only = ["tts"];
        assert_eq!(
            tray_icon_kind(true, true, &tts_only),
            TrayIconKind::Speaking
        );
    }
}
