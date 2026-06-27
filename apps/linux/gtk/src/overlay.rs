//! The focus-safe dictation overlay — the GTK4 analogue of the macOS `OverlayPanel` /
//! Windows layered `DictationPanel`. It shows the live transcript (and a "speak now" /
//! "no paste target" glow) WITHOUT stealing keyboard focus, so the paste still lands in the
//! terminal the user was in.
//!
//! Focus-safety on Linux has no single portable API: on wlroots/KDE compositors we use
//! `gtk4-layer-shell` (an OVERLAY-layer surface with `KeyboardMode::None`); on GNOME/Mutter
//! (no wlr-layer-shell) and X11 we fall back to a plain undecorated non-focusing window —
//! best-effort, the documented Wayland limitation. Shown exactly when the macOS/Windows hosts
//! show theirs: `awaiting_confirm || (recording && local_stt)`.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::status::Snapshot;

#[derive(Clone)]
pub struct Overlay {
    window: gtk::Window,
    label: gtk::Label,
    visible: Rc<Cell<bool>>,
}

impl Overlay {
    pub fn new(app: &adw::Application) -> Self {
        let window = gtk::Window::builder()
            .application(app)
            .resizable(false)
            .decorated(false)
            .deletable(false)
            .can_focus(false)
            .default_width(460)
            .build();
        window.add_css_class("ds-overlay");

        // `gtk4_layer_shell::is_supported()` ASSERTS a Wayland display (CRITICAL on X11), so
        // gate it on the actual display backend first — never call layer-shell under X11.
        let on_wayland = gtk::gdk::Display::default()
            .map(|d| d.type_().name().contains("Wayland"))
            .unwrap_or(false);
        if on_wayland && gtk4_layer_shell::is_supported() {
            // wlroots / KDE: a true overlay surface that never takes the keyboard.
            window.init_layer_shell();
            window.set_namespace(Some("ds-dictation"));
            window.set_layer(Layer::Overlay);
            window.set_keyboard_mode(KeyboardMode::None);
            window.set_anchor(Edge::Bottom, true);
            window.set_margin(Edge::Bottom, 90);
        } else {
            // GNOME/Mutter (no wlr-layer-shell) or X11: best-effort non-focusing float.
            window.set_modal(false);
        }

        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        card.add_css_class("card");
        let label = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .build();
        label.add_css_class("ds-overlay-text");
        card.append(&label);
        window.set_child(Some(&card));

        Overlay {
            window,
            label,
            visible: Rc::new(Cell::new(false)),
        }
    }

    /// Show/update or hide the overlay from a status push (same gate as the other hosts).
    pub fn apply(&self, snap: &Snapshot) {
        let show = matches!(
            &snap.status,
            Some(s) if s.dictation.awaiting_confirm
                || (s.dictation.recording && s.dictation.local_stt)
        );

        if !show {
            if self.visible.replace(false) {
                self.window.set_visible(false);
            }
            return;
        }

        let s = snap.status.as_ref().expect("show implies Some");
        // Show the live transcript; empty while recording shows nothing (no Linux-local prompt
        // text — there is no shared i18n key for one, and the glow already cues "speak now").
        self.label.set_text(&s.dictation.text);
        // Orange glow: the engine-computed "speak now" hint, OR a missing paste target.
        let glow = s.dictation.prompt_glow || !s.dictation.has_paste_target;
        if glow {
            self.window.add_css_class("glow");
        } else {
            self.window.remove_css_class("glow");
        }

        if !self.visible.replace(true) {
            self.window.present();
        }
    }
}

/// The overlay (and panel) styling, loaded once into the default display.
pub fn load_css() {
    let css = "
        .ds-overlay { background: transparent; }
        .ds-overlay box.card {
            background-color: alpha(@window_bg_color, 0.92);
            border: 1px solid alpha(@borders, 0.7);
            border-radius: 16px;
            padding: 14px 18px;
        }
        .ds-overlay.glow box.card {
            border-color: #FF9F0A;
            box-shadow: 0 0 18px 2px alpha(#FF9F0A, 0.5);
        }
        .ds-overlay-text { font-size: 1.15rem; }
    ";
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
