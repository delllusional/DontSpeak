//! Tray / state-stripe icon kind — single source for every host.
//!
//! Color is gated by `model_status.tray_indicator`: plain token (`"stt"` / `"tts"`) or
//! animated form (`"stt_animated"` / `"tts_animated"`) both enable the tint. Empty list
//! ⇒ never color (idle). Recording wins over speaking when both apply (full-duplex live-mic
//! cue). Download/warm states stay on engine dots only — never the tray.

/// Tray / title-bar indicator kind. Wire tokens via [`TrayIconKind::as_str`].
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

    /// Wire / FFI token (`idle` | `recording` | `speaking`).
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

/// Whether `tray_indicator` enables coloring for `base` (`"stt"` / `"tts"`), including
/// the `_animated` form hosts use for breathing pills (macOS).
fn colors_for(tray_indicator: &[impl AsRef<str>], base: &str) -> bool {
    tray_indicator.iter().any(|t| {
        let t = t.as_ref();
        t == base || t.strip_prefix(base).is_some_and(|rest| rest == "_animated")
    })
}

/// Resolve tray/state-stripe kind from live activity + `tray_indicator` config.
///
/// `stt_active` is Caps dictation (not always-on capture). `tts_active` is playback.
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
        assert_eq!(
            tray_icon_kind(true, false, &ind),
            TrayIconKind::Recording
        );
        assert_eq!(
            tray_icon_kind(false, true, &ind),
            TrayIconKind::Speaking
        );
        assert_eq!(tray_icon_kind(false, false, &ind), TrayIconKind::Idle);
    }

    #[test]
    fn animated_forms_count_as_enabled() {
        let ind = ["stt_animated", "tts_animated"];
        assert_eq!(
            tray_icon_kind(true, false, &ind),
            TrayIconKind::Recording
        );
        assert_eq!(
            tray_icon_kind(false, true, &ind),
            TrayIconKind::Speaking
        );
    }

    #[test]
    fn recording_wins_over_speaking() {
        let ind = ["stt", "tts"];
        assert_eq!(
            tray_icon_kind(true, true, &ind),
            TrayIconKind::Recording
        );
    }

    #[test]
    fn only_configured_side_colors() {
        let stt_only = ["stt"];
        assert_eq!(
            tray_icon_kind(true, true, &stt_only),
            TrayIconKind::Recording
        );
        assert_eq!(
            tray_icon_kind(false, true, &stt_only),
            TrayIconKind::Idle
        );
        let tts_only = ["tts"];
        assert_eq!(
            tray_icon_kind(true, true, &tts_only),
            TrayIconKind::Speaking
        );
    }
}
