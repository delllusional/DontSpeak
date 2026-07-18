//! StatusNotifierItem tray (GTK4 dropped legacy StatusIcon). Runs on its own thread (ksni
//! blocking DBus). Menu actions are handled **on this thread** (or a short worker) so they do
//! not depend on the GTK main loop polling async sources — which was dropping every tray
//! command under GApplication. Icon is a custom brand pixmap ([`crate::icon`]).

use ksni::Tray;
use ksni::menu::{MenuItem, StandardItem};

use crate::icon::{self, Rgb};

/// Sizes handed to the SNI host; it picks the closest to the panel slot.
const ICON_SIZES: [u32; 4] = [16, 24, 32, 48];

pub struct SpeakTray {
    /// [`ds_status::tray_icon_kind`].
    pub kind: ds_status::TrayIconKind,
    pub muted: bool,
    seed_purple: Rgb,
    mic_orange: Rgb,
}

impl SpeakTray {
    pub fn new() -> Self {
        let (seed_purple, mic_orange) = icon::brand_colors(&crate::ffi::brand_colors_json());
        SpeakTray {
            kind: ds_status::TrayIconKind::Idle,
            muted: false,
            seed_purple,
            mic_orange,
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

/// Ask the running GApplication to Activate (shows the main window on the GTK thread).
///
/// Real export path is `/org/dontspeak/DontSpeak` (not the gtk Application template path).
/// Signature is `a{sv}` platform-data — empty dict `{}`.
///
/// Spawns off this SNI thread so a slow D-Bus reply never freezes the tray menu.
fn activate_application() {
    std::thread::Builder::new()
        .name("ds-tray-activate".into())
        .spawn(|| {
            // Prefer absolute path: tray may run under a stripped systemd PATH.
            let gdbus = if std::path::Path::new("/usr/bin/gdbus").is_file() {
                "/usr/bin/gdbus"
            } else {
                "gdbus"
            };
            let out = std::process::Command::new(gdbus)
                .args([
                    "call",
                    "--session",
                    "--dest",
                    crate::APP_ID,
                    "--object-path",
                    "/org/dontspeak/DontSpeak",
                    "--method",
                    "org.gtk.Application.Activate",
                    "{}",
                ])
                .output();
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    log::warn!(
                        target: "tray",
                        "activate application exited {}: {}",
                        o.status,
                        err.trim()
                    );
                }
                // Goes through ds_log → existing dontspeak.log (not a new file).
                Err(e) => log::warn!(target: "tray", "activate application failed: {e}"),
            }
        })
        .ok();
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

    /// Left-click → show window (keep last tab). ItemIsMenu stays false (ksni default) so the
    /// host routes left-click here and only shows the context menu on right-click.
    fn activate(&mut self, _x: i32, _y: i32) {
        activate_application();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // Handlers run on the SNI/DBus thread — keep them non-blocking for GTK widgets.
        // Mutating `self` is pushed back into the live menu by ksni after the callback returns.
        //
        // macOS parity (`TrayMenu.swift`): every row is the same control type with a **leading**
        // glyph — Mute uses speaker on/off (not a separate CheckmarkItem gutter that misaligns
        // Settings/Quit). Same left icon column for all three actions.
        vec![
            StandardItem {
                label: crate::ffi::t("tray.mute"),
                // macOS: speaker.slash / speaker.wave.2
                icon_name: if self.muted {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                }
                .into(),
                activate: Box::new(move |tray: &mut SpeakTray| {
                    tray.muted = !tray.muted;
                    let want = tray.muted;
                    std::thread::spawn(move || {
                        if !crate::ffi::set_muted(want) {
                            log::warn!(target: "tray", "set_muted({want}) failed (engine down?)");
                        }
                    });
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: crate::ffi::t("tray.settings"),
                icon_name: "preferences-system-symbolic".into(),
                activate: Box::new(move |_| {
                    activate_application();
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: crate::ffi::t("tray.quit"),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(move |_| {
                    let _ = crate::ffi::engine_stop();
                    std::process::exit(0);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}
