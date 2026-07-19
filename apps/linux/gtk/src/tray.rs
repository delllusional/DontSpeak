//! StatusNotifierItem tray on its own thread (ksni). Menu actions run here (not GTK main loop).

use ksni::Tray;
use ksni::menu::{MenuItem, StandardItem};

use crate::icon::{self, Rgb};

/// Sizes handed to the SNI host; it picks the closest to the panel slot.
const ICON_SIZES: [u32; 4] = [16, 24, 32, 48];

pub struct SpeakTray {
    /// From [`ds_status::tray_icon_kind`].
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

    fn ink(&self) -> Rgb {
        match self.kind {
            ds_status::TrayIconKind::Recording => self.mic_orange,
            ds_status::TrayIconKind::Speaking => self.seed_purple,
            ds_status::TrayIconKind::Idle => icon::idle_fg(),
        }
    }
}

/// GApplication Activate on `/org/dontspeak/DontSpeak` (`a{sv}` empty). Off SNI thread.
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
                // log::warn → unified dontspeak.log (ds_log).
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

    /// Left-click → show window (`ItemIsMenu` false).
    fn activate(&mut self, _x: i32, _y: i32) {
        activate_application();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        // SNI thread: non-blocking for GTK. Same leading-glyph rows as macOS TrayMenu.
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
