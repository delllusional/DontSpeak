//! DontSpeak Linux GUI host — GTK4 + libadwaita.
//!
//! The native modern-GNOME analogue of the macOS SwiftUI app and the Windows WinUI app: it
//! HOSTS the engine in-process via the `ds-core` C ABI (`engine_start` on launch,
//! `engine_stop` on quit) and renders the engine's pushed status as a tray icon, a health
//! panel, and (in a later increment) a focus-safe dictation overlay. Control lives in the MCP.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

mod ffi;
mod icon;
mod log_push;
mod overlay;
mod status;
mod tray;
mod ui;

pub(crate) const APP_ID: &str = "org.dontspeak.DontSpeak";

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_startup(|_| {
        ffi::set_locale(&sys_locale::get_locale().unwrap_or_else(|| "en".to_string()));
        overlay::load_css();
        ui::load_update_badge_css();
        // Host the engine in-process (idempotent; returns true if running now).
        ffi::engine_start();
    });
    app.connect_activate(on_activate);
    app.connect_shutdown(|_| {
        ffi::engine_stop();
    });

    app.run()
}

fn on_activate(app: &adw::Application) {
    let widgets = ui::build_window(app);
    // No synchronous prime here: `model_status_json` is a blocking engine-IPC round-trip
    // (up to 120s if the engine is slow to answer) and this runs on the GTK main thread.
    // The panel starts blank for one frame; `status::spawn_push` below (background thread)
    // delivers the first snapshot within its 1s poll window.

    // Live in the tray: keep the app running when the window is closed, and hide (not
    // destroy) the window on its close button.
    let hold = app.hold();
    widgets.window.connect_close_request(|w| {
        w.set_visible(false);
        glib::Propagation::Stop
    });

    // One-shot startup update check: `ds_update_check_json` is a blocking network call (the
    // ONE network-touching ds-core FFI entry point), so it runs on its own throwaway thread,
    // never the GTK main thread — mirrors the `status::spawn_push` background-thread pattern,
    // but fires exactly once per launch rather than looping. The result crosses back to the
    // main thread over a one-shot channel; `ui::apply_update_check` treats any failure/`{}`
    // payload as "no update" and leaves the pill hidden.
    {
        let w = widgets.clone();
        let (tx, rx) = async_channel::bounded::<String>(1);
        std::thread::Builder::new()
            .name("ds-update-check".into())
            .spawn(move || {
                let _ = tx.send_blocking(ffi::update_check_json());
            })
            .ok();
        glib::spawn_future_local(async move {
            if let Ok(json) = rx.recv().await {
                ui::apply_update_check(&w, &json);
            }
        });
    }

    // Tray on its own DBus thread; its menu hands commands back over a channel. Fail-soft:
    // no session bus / no SNI host → no tray, the rest of the app still runs.
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<tray::Cmd>();
    let tray_handle = {
        use ksni::blocking::TrayMethods;
        tray::SpeakTray::new(cmd_tx).spawn().ok()
    };

    // Shared latest mute state, so the tray "Mute" toggle knows what to flip to.
    let muted = Rc::new(Cell::new(false));

    // The focus-safe dictation overlay (hidden until a dictation is in progress).
    let overlay = overlay::Overlay::new(app);

    // Status push → health panel + tray icon + overlay.
    let (tx, rx) = async_channel::unbounded::<status::Snapshot>();
    status::spawn_push(tx);
    {
        let w = widgets.clone();
        let th = tray_handle.clone();
        let muted = muted.clone();
        let overlay = overlay.clone();
        glib::spawn_future_local(async move {
            while let Ok(snap) = rx.recv().await {
                ui::update(&w, &snap);
                overlay.apply(&snap);
                let (speaking, recording, is_muted) = match &snap.status {
                    Some(s) => (s.running.tts_active, s.running.stt_active, s.running.muted),
                    None => (false, false, false),
                };
                muted.set(is_muted);
                if let Some(h) = &th {
                    h.update(move |t| {
                        t.muted = is_muted;
                        t.speaking = speaking;
                        t.recording = recording;
                    });
                }
            }
        });
    }

    // Tray commands → actions on the GTK main thread.
    {
        let app = app.clone();
        let w = widgets.clone();
        let muted = muted.clone();
        glib::spawn_future_local(async move {
            let _hold = hold; // keep the app alive as long as we're listening for commands
            while let Ok(cmd) = cmd_rx.recv().await {
                match cmd {
                    tray::Cmd::ShowWindow => w.window.present(),
                    tray::Cmd::ToggleMute => {
                        // ffi::set_muted is a blocking engine-IPC round-trip (up to 120s
                        // worst case) — never call it inline from the GTK main context.
                        let want = !muted.get();
                        std::thread::spawn(move || {
                            ffi::set_muted(want);
                        });
                    }
                    tray::Cmd::Quit => app.quit(),
                }
            }
        });
    }

    widgets.window.present();
}
