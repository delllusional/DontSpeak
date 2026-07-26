//! Focus-safe dictation overlay (macOS `OverlayPanel` / Windows `DictationPanel`).
//! Layer-shell (wlroots/KDE): OVERLAY + `KeyboardMode::None`. Else undecorated non-focusing
//! toplevel (GNOME/X11 best-effort). Shown when `dictation.state` ≠ `hidden`.
//! Plain path: drag + horizontal resize [`MIN_WIDTH`..=`MAX_WIDTH`]; width in
//! `$XDG_STATE_HOME/dontspeak/overlay-width` (position omitted — Wayland/GTK4). Layer-shell:
//! compositor-anchored, fixed width.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::status::Snapshot;
use ds_status::DictationState;

const DEFAULT_WIDTH: i32 = 460;
/// Horizontal-resize bounds + side-edge grab margin — mirrors macOS Overlay min/max/edgeMargin.
const MIN_WIDTH: i32 = 280;
const MAX_WIDTH: i32 = 900;
const EDGE: f64 = 8.0;

#[derive(Clone)]
pub struct Overlay {
    window: gtk::Window,
    label: gtk::Label,
    visible: Rc<Cell<bool>>,
    /// Plain-toplevel path only: `apply` persists width on hide. False under layer-shell.
    resizable: bool,
}

impl Overlay {
    pub fn new(app: &adw::Application) -> Self {
        let display = gtk::gdk::Display::default();
        // `is_supported()` ASSERTS a Wayland display (CRITICAL on X11) — gate on backend first.
        let on_wayland = display
            .as_ref()
            .map(|d| d.type_().name().contains("Wayland"))
            .unwrap_or(false);
        let layer_shell = on_wayland && gtk4_layer_shell::is_supported();

        let window = gtk::Window::builder()
            .application(app)
            .resizable(!layer_shell)
            .decorated(false)
            .deletable(false)
            .can_focus(false)
            .default_width(if layer_shell {
                DEFAULT_WIDTH
            } else {
                load_width()
            })
            .build();
        window.add_css_class("ds-overlay");

        if layer_shell {
            // wlroots / KDE: overlay surface; KeyboardMode::None keeps focus on the target.
            window.init_layer_shell();
            window.set_namespace(Some("ds-dictation"));
            window.set_layer(Layer::Overlay);
            window.set_keyboard_mode(KeyboardMode::None);
            window.set_anchor(Edge::Bottom, true);
            window.set_margin(Edge::Bottom, 90);
        } else {
            // GNOME/Mutter or X11: best-effort non-focusing float.
            window.set_modal(false);
            // Floor at MIN_WIDTH; ceiling on save/restore (GTK has no max-size).
            window.set_size_request(MIN_WIDTH, -1);
            // Transparent CSS needs a compositor; without one the window is opaque black.
            // `Display::is_composited()` is more precise than backend alone: Wayland always
            // composites, but X11 varies (bare tiling WM vs GNOME/KDE-on-Xorg).
            if !display.as_ref().map(|d| d.is_composited()).unwrap_or(false) {
                window.add_css_class("ds-overlay-solid");
            }
        }

        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        card.add_css_class("card");
        let label = gtk::Label::builder().wrap(true).xalign(0.0).build();
        label.add_css_class("ds-overlay-text");
        card.append(&label);
        window.set_child(Some(&card));

        if !layer_shell {
            install_move_resize(&window, &card);
        }

        Overlay {
            window,
            label,
            visible: Rc::new(Cell::new(false)),
            resizable: !layer_shell,
        }
    }

    /// Show/update or hide from a status push (same gate as the other hosts).
    pub fn apply(&self, snap: &Snapshot) {
        // Visibility is engine-side via canonical `dictation.state` (incl. REFUSED brief glow).
        let show = snap.status.as_ref().is_some_and(|s| {
            s.dictation.state != DictationState::Hidden && !s.dictation.external_ui_active
        });

        if !show {
            if self.visible.replace(false) {
                // Persist width while still allocated (0 once hidden). Position: see module docs.
                if self.resizable {
                    save_width(self.window.width());
                }
                self.window.set_visible(false);
            }
            return;
        }

        let s = snap.status.as_ref().expect("show implies Some");
        // Empty while recording: no shared i18n "speak now" key; glow is the cue.
        self.label.set_text(&s.dictation.text);
        // Orange glow: speak-now, missing paste target, or refused start (reuses no-target glow).
        let state = s.dictation.state;
        let glow = state == DictationState::Recording && s.dictation.text.is_empty()
            || !s.dictation.can_paste
            || state == DictationState::Refused;
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

/// Drag-to-move + horizontal edge-resize (plain-toplevel only). Press within `EDGE` of a side
/// → `begin_resize`; else `begin_move` — compositor owns the interaction (X11 + Mutter).
fn install_move_resize(window: &gtk::Window, card: &gtk::Box) {
    use gtk::gdk;

    let press = gtk::GestureClick::new();
    press.set_button(gdk::BUTTON_PRIMARY);
    {
        let window = window.clone();
        press.connect_pressed(move |g, _n, x, y| {
            let Some(toplevel) = window
                .surface()
                .and_then(|s| s.downcast::<gdk::Toplevel>().ok())
            else {
                return;
            };
            let Some(device) = g.device() else { return };
            let button = g.current_button() as i32;
            let time = g.current_event().map(|e| e.time()).unwrap_or(0);
            match hit_test(x, window.width() as f64) {
                Hit::LeftEdge => {
                    toplevel.begin_resize(gdk::SurfaceEdge::West, Some(&device), button, x, y, time)
                }
                Hit::RightEdge => {
                    toplevel.begin_resize(gdk::SurfaceEdge::East, Some(&device), button, x, y, time)
                }
                Hit::Body => toplevel.begin_move(&device, button, x, y, time),
            }
        });
    }
    card.add_controller(press);

    let motion = gtk::EventControllerMotion::new();
    {
        let window = window.clone();
        let card = card.clone();
        motion.connect_motion(move |_, x, _| {
            let on_edge = hit_test(x, window.width() as f64) != Hit::Body;
            card.set_cursor_from_name(if on_edge { Some("ew-resize") } else { None });
        });
    }
    card.add_controller(motion);
}

/// Side edge (resize) vs body (move). Pure for unit tests without a display.
#[derive(PartialEq, Debug)]
enum Hit {
    LeftEdge,
    RightEdge,
    Body,
}

fn hit_test(x: f64, width: f64) -> Hit {
    if x <= EDGE {
        Hit::LeftEdge
    } else if x >= width - EDGE {
        Hit::RightEdge
    } else {
        Hit::Body
    }
}

// `$XDG_STATE_HOME/dontspeak/overlay-width` (fallback `~/.local/state/…`) — same local-state
// root as the engine capskey marker; computed here to stay dependency-light. Width only.

fn width_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state.join("dontspeak").join("overlay-width"))
}

fn clamp_width(w: i32) -> i32 {
    w.clamp(MIN_WIDTH, MAX_WIDTH)
}

/// Persisted width, clamped; `DEFAULT_WIDTH` when unset or unparseable.
fn load_width() -> i32 {
    width_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(clamp_width)
        .unwrap_or(DEFAULT_WIDTH)
}

/// Persist current width (clamped). Best-effort.
fn save_width(w: i32) {
    if w <= 0 {
        return; // not allocated (already hidden)
    }
    if let Some(p) = width_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, clamp_width(w).to_string());
    }
}

/// Overlay styling, loaded once into the default display.
pub fn load_css() {
    let css = "
        .ds-overlay { background: transparent; }
        .ds-overlay.ds-overlay-solid { background-color: @window_bg_color; }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_splits_edges_from_body() {
        let w = 460.0;
        assert_eq!(hit_test(0.0, w), Hit::LeftEdge);
        assert_eq!(hit_test(EDGE, w), Hit::LeftEdge);
        assert_eq!(hit_test(EDGE + 0.1, w), Hit::Body);
        assert_eq!(hit_test(w / 2.0, w), Hit::Body);
        assert_eq!(hit_test(w - EDGE - 0.1, w), Hit::Body);
        assert_eq!(hit_test(w - EDGE, w), Hit::RightEdge);
        assert_eq!(hit_test(w, w), Hit::RightEdge);
    }

    #[test]
    fn width_clamps_to_bounds() {
        assert_eq!(clamp_width(100), MIN_WIDTH);
        assert_eq!(clamp_width(2000), MAX_WIDTH);
        assert_eq!(clamp_width(500), 500);
        assert_eq!(clamp_width(MIN_WIDTH), MIN_WIDTH);
        assert_eq!(clamp_width(MAX_WIDTH), MAX_WIDTH);
    }
}
