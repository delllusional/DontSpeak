//! StatusNotifierItem tray (the freedesktop spec; GTK4 dropped legacy StatusIcon). It runs
//! on its OWN thread (ksni's blocking DBus loop), so its menu callbacks hand work back to the
//! GTK main loop over an async-channel, and the main loop refreshes the state via the
//! `Handle::update`. The icon is a custom pixmap rendered from the shared brand glyph (see
//! [`crate::icon`]) — the same idle / recording / speaking / muted logic the macOS/Windows
//! hosts use, rather than a freedesktop theme-icon name.

use ksni::Tray;
use ksni::menu::{MenuItem, StandardItem};

use crate::icon::{self, Rgb};

/// The sizes we hand the SNI host; it picks the closest to the panel's slot.
const ICON_SIZES: [u32; 4] = [16, 24, 32, 48];

/// What the tray menu asks the GTK main loop to do (the tray thread can't touch GTK).
pub enum Cmd {
    ShowWindow,
    ToggleMute,
    Quit,
}

pub struct SpeakTray {
    pub speaking: bool,
    pub recording: bool,
    pub muted: bool,
    seed_purple: Rgb,
    mic_orange: Rgb,
    tx: async_channel::Sender<Cmd>,
}

impl SpeakTray {
    pub fn new(tx: async_channel::Sender<Cmd>) -> Self {
        let (seed_purple, mic_orange) = icon::brand_colors(&crate::ffi::brand_colors_json());
        SpeakTray {
            speaking: false,
            recording: false,
            muted: false,
            seed_purple,
            mic_orange,
            tx,
        }
    }

    /// Per-state glyph tint, mirroring macOS/Windows: recording → mic_orange, speaking →
    /// seed_purple, otherwise the idle foreground. (Muted is an overlaid slash, not a color.)
    fn ink(&self) -> Rgb {
        if self.recording {
            self.mic_orange
        } else if self.speaking {
            self.seed_purple
        } else {
            icon::idle_fg()
        }
    }
}

impl Tray for SpeakTray {
    fn id(&self) -> String {
        "org.dontspeak.DontSpeak".into()
    }
    fn title(&self) -> String {
        crate::ffi::t("common.app_name")
    }

    /// The brand glyph rendered from the shared `assets/tray-icon.svg`, tinted per state with
    /// the shared brand colors and slashed when muted — the same icon LOGIC as the macOS and
    /// Windows hosts (a custom pixmap, not a theme-icon name), so all three read identically.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let ink = self.ink();
        ICON_SIZES
            .iter()
            .map(|&s| icon::render(s, ink, self.muted))
            .collect()
    }

    /// Primary (left) click — open the status window, mirroring the macOS/Windows tray.
    /// ItemIsMenu stays false (ksni default), so the host routes left-click here and only
    /// shows the context menu below on right-click.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(Cmd::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // Each callback hands off to the GTK loop over the channel — never blocks the tray.
        let (show, mute, quit) = (self.tx.clone(), self.tx.clone(), self.tx.clone());
        vec![
            // Mute: the muted state is a checkmark shown in the SAME left icon column as
            // Status/Quit (so all three rows align), via a StandardItem icon — not a
            // CheckmarkItem, whose check renders in GNOME's separate ornament gutter and so
            // wouldn't line up. Empty icon when unmuted; the tray icon's slash is the
            // primary muted cue. Toggles mute on activate.
            StandardItem {
                label: crate::ffi::t("tray.mute"),
                icon_name: if self.muted {
                    "object-select-symbolic"
                } else {
                    ""
                }
                .into(),
                activate: Box::new(move |_| {
                    let _ = mute.try_send(Cmd::ToggleMute);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: crate::ffi::t("common.nav_status"),
                icon_name: "view-reveal-symbolic".into(),
                activate: Box::new(move |_| {
                    let _ = show.try_send(Cmd::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: crate::ffi::t("tray.quit"),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(move |_| {
                    let _ = quit.try_send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
