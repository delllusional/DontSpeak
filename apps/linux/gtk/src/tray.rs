//! StatusNotifierItem tray (GTK4 dropped legacy StatusIcon). Runs on its own thread (ksni
//! blocking DBus); menu callbacks hand work to the GTK main loop over an async-channel, and
//! the main loop refreshes state via `Handle::update`. Icon is a custom brand pixmap
//! ([`crate::icon`]) — not a freedesktop theme name.

use ksni::Tray;
use ksni::menu::{MenuItem, StandardItem};

use crate::icon::{self, Rgb};

/// Sizes handed to the SNI host; it picks the closest to the panel slot.
const ICON_SIZES: [u32; 4] = [16, 24, 32, 48];

/// Tray menu → GTK main loop (tray thread must not touch GTK).
pub enum Cmd {
    ShowWindow,
    ToggleMute,
    Quit,
}

pub struct SpeakTray {
    /// Shared kind from [`ds_status::tray_icon_kind`] (gates on `tray_indicator`).
    pub kind: ds_status::TrayIconKind,
    pub muted: bool,
    seed_purple: Rgb,
    mic_orange: Rgb,
    tx: async_channel::Sender<Cmd>,
}

impl SpeakTray {
    pub fn new(tx: async_channel::Sender<Cmd>) -> Self {
        let (seed_purple, mic_orange) = icon::brand_colors(&crate::ffi::brand_colors_json());
        SpeakTray {
            kind: ds_status::TrayIconKind::Idle,
            muted: false,
            seed_purple,
            mic_orange,
            tx,
        }
    }

    /// Per-state tint: recording → mic_orange, speaking → seed_purple, else idle.
    /// Muted is a slash, not a color. Download/warm live only on engine dots, not tray.
    fn ink(&self) -> Rgb {
        match self.kind {
            ds_status::TrayIconKind::Recording => self.mic_orange,
            ds_status::TrayIconKind::Speaking => self.seed_purple,
            ds_status::TrayIconKind::Idle => icon::idle_fg(),
        }
    }
}

impl Tray for SpeakTray {
    fn id(&self) -> String {
        crate::APP_ID.into()
    }
    fn title(&self) -> String {
        crate::ffi::t("common.app_name")
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let ink = self.ink();
        ICON_SIZES
            .iter()
            .map(|&s| icon::render(s, ink, self.muted))
            .collect()
    }

    /// Left-click → status window. ItemIsMenu stays false (ksni default) so the host routes
    /// left-click here and only shows the context menu on right-click.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(Cmd::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // Callbacks only enqueue — never block the tray DBus thread.
        let (show, mute, quit) = (self.tx.clone(), self.tx.clone(), self.tx.clone());
        vec![
            // Mute uses a StandardItem icon in the same left column as Status/Quit so rows
            // align. CheckmarkItem would put its check in GNOME's ornament gutter instead.
            // Empty icon when unmuted; tray slash is the primary muted cue.
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
