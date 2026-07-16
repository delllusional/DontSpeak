//! DontSpeak Linux GUI host — GTK4 + libadwaita.
//!
//! Hosts the engine in-process via the `ds-core` C ABI and renders pushed status as tray,
//! health panel, and focus-safe dictation overlay. Control lives in the MCP.

use std::cell::{Cell, RefCell};
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

    // Join the status thread before `engine_stop()` so it isn't mid-`model_status_wait`.
    let status_thread: Rc<RefCell<Option<status::StatusThread>>> = Rc::new(RefCell::new(None));

    app.connect_startup(|_| {
        ffi::set_locale(&sys_locale::get_locale().unwrap_or_else(|| "en".to_string()));
        overlay::load_css();
        ui::load_update_badge_css();
        // Idempotent; true if the engine is running after the call.
        ffi::engine_start();
    });
    app.connect_activate({
        let st = status_thread.clone();
        move |app| on_activate(app, st.clone())
    });
    app.connect_shutdown(move |_| {
        if let Some(st) = status_thread.borrow_mut().take() {
            st.join();
        }
        ffi::engine_stop();
    });

    app.run()
}

fn on_activate(app: &adw::Application, status_thread: Rc<RefCell<Option<status::StatusThread>>>) {
    // GTK re-fires `activate` on relaunch of a running instance. The window is only ever
    // hidden (never destroyed) on close — re-present instead of stacking a second tray/UI.
    if let Some(existing) = app.windows().first() {
        existing.present();
        return;
    }

    let widgets = ui::build_window(app);
    // No sync prime: `model_status_json` is a blocking engine-IPC round-trip (up to 120s) and
    // this is the GTK main thread. Panel stays blank one frame; `status::spawn_push` delivers
    // the first snapshot within its 1s poll window.

    // Stay alive in the tray: hide (don't destroy) on close.
    let hold = app.hold();
    widgets.window.connect_close_request(|w| {
        w.set_visible(false);
        glib::Propagation::Stop
    });

    // One-shot update check on a throwaway thread — `ds_update_check_json` is the only
    // network-touching ds-core FFI entry (blocking HTTP). Failures/`{}` → pill stays hidden
    // (`ui::apply_update_check`).
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

    // Tray on its own DBus thread; commands come back over a channel. Fail-soft: no session
    // bus / no SNI host → no tray, rest of the app still runs.
    let (cmd_tx, cmd_rx) = async_channel::unbounded::<tray::Cmd>();
    let tray_handle = {
        use ksni::blocking::TrayMethods;
        tray::SpeakTray::new(cmd_tx).spawn().ok()
    };

    let muted = Rc::new(Cell::new(false));
    let overlay = overlay::Overlay::new(app);

    let (tx, rx) = async_channel::bounded::<status::Snapshot>(1);
    let st = status::spawn_push(tx);
    *status_thread.borrow_mut() = Some(st);
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

    {
        let app = app.clone();
        let w = widgets.clone();
        let muted = muted.clone();
        glib::spawn_future_local(async move {
            let _hold = hold; // keep the app alive while tray commands are listened for
            while let Ok(cmd) = cmd_rx.recv().await {
                match cmd {
                    tray::Cmd::ShowWindow => w.window.present(),
                    tray::Cmd::ToggleMute => {
                        // Blocking engine-IPC (up to 120s) — never inline on the GTK main context.
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
