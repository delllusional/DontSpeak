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
    // Before GTK: hypervisor Wayland can freeze present/map — prefer X11 unless GDK_BACKEND set.
    prefer_x11_under_hypervisor();

    let app = adw::Application::builder().application_id(APP_ID).build();

    // Join the status thread before `engine_stop()` so it isn't mid-`model_status_wait`.
    let status_thread: Rc<RefCell<Option<status::StatusThread>>> = Rc::new(RefCell::new(None));

    app.connect_startup(|_| {
        // Install the process `log` backend once → existing unified activity log only
        // (`~/.local/state/dontspeak/logs/dontspeak.log` on Linux). No host-private log files.
        // Idempotent if the in-process engine also inits on its thread.
        ds_log::init();
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
    // GTK re-fires `activate` on relaunch / tray Settings (org.gtk.Application.Activate).
    // Window is only ever hidden (never destroyed) on close — re-show the **main** panel.
    //
    // Do not use `windows().first()`: the dictation overlay is also an application window
    // (`overlay::Overlay`), often ordered before the hidden main window. Presenting that
    // empty overlay looks like Settings did nothing.
    if present_main_window(app) {
        return;
    }

    let widgets = ui::build_window(app);
    // No sync prime: `model_status_json` is a blocking engine-IPC round-trip (up to 120s) and
    // this is the GTK main thread. Panel stays blank one frame; `status::spawn_push` delivers
    // the first snapshot within its 1s poll window.

    // Stay alive in the tray: hide (don't destroy) on close.
    let _hold = app.hold();
    widgets.window.set_hide_on_close(true);

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

    // ksni `spawn()` runs a blocking async setup on the *calling* thread before detaching
    // its service loop — never call it on the GTK UI thread (freezes Activate/Settings).
    let tray_handle = {
        use ksni::blocking::TrayMethods;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("ds-tray-spawn".into())
            .spawn(move || {
                let handle = tray::SpeakTray::new().spawn().ok();
                let _ = tx.send(handle);
            })
            .ok();
        // Brief wait only; missing tray is fail-soft.
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .ok()
            .flatten()
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
                let (kind, is_muted) = match &snap.status {
                    Some(s) => (
                        ds_status::tray_icon_kind(
                            s.running.stt_active,
                            s.running.tts_active,
                            &s.tray_indicator,
                        ),
                        s.running.muted,
                    ),
                    None => (ds_status::TrayIconKind::Idle, false),
                };
                muted.set(is_muted);
                if let Some(h) = &th {
                    h.update(move |t| {
                        t.muted = is_muted;
                        t.kind = kind;
                    });
                }
            }
        });
    }

    // Keep hold alive for the process lifetime (tray-resident).
    std::mem::forget(_hold);

    widgets.window.set_visible(true);
    widgets.window.present();
}

/// Re-show the health/settings `ApplicationWindow` if it already exists.
/// Skips the dictation overlay (`gtk::Window`, not `adw::ApplicationWindow`).
fn present_main_window(app: &adw::Application) -> bool {
    for win in app.windows() {
        // Main UI is the only `adw::ApplicationWindow`; overlay is plain `gtk::Window`.
        if !win.is::<adw::ApplicationWindow>() {
            continue;
        }
        win.set_visible(true);
        win.unminimize();
        win.present();
        return true;
    }
    false
}

/// If unset, force `GDK_BACKEND=x11` on common hypervisors so window show/Activate stays responsive.
fn prefer_x11_under_hypervisor() {
    if std::env::var_os("GDK_BACKEND").is_some() {
        return;
    }
    let product = std::fs::read_to_string("/sys/class/dmi/id/product_name")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let vendor = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let hyper = [
        "virtualbox",
        "vmware",
        "kvm",
        "qemu",
        "xen",
        "microsoft corporation",
        "parallels",
    ]
    .iter()
    .any(|h| product.contains(h) || vendor.contains(h));
    if hyper {
        // SAFETY: process-wide env before any GTK/GDK init.
        unsafe { std::env::set_var("GDK_BACKEND", "x11") };
    }
}
