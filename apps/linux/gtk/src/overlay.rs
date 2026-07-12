//! The focus-safe dictation overlay — the GTK4 analogue of the macOS `OverlayPanel` /
//! Windows layered `DictationPanel`. It shows the live transcript (and a "speak now" /
//! "no paste target" glow) WITHOUT stealing keyboard focus, so the paste still lands in the
//! terminal the user was in.
//!
//! Focus-safety on Linux has no single portable API: on wlroots/KDE compositors we use
//! `gtk4-layer-shell` (an OVERLAY-layer surface with `KeyboardMode::None`); on GNOME/Mutter
//! (no wlr-layer-shell) and X11 we fall back to a plain undecorated non-focusing window —
//! best-effort, the documented Wayland limitation. Shown exactly when the macOS/Windows hosts
//! show theirs: the canonical `dictation.state` token is not `hidden`.
//!
//! Drag + resize (macOS/Windows parity): on the plain-toplevel path the pill is
//! DRAGGABLE (press the body → `Toplevel::begin_move`) and horizontally RESIZABLE (press
//! within `EDGE` of the left/right side → `Toplevel::begin_resize`), matching both peers,
//! which are also width-only (macOS `DictationPanel.swift`, Windows `WM_SIZING`). The
//! chosen width is clamped to [`MIN_WIDTH`, `MAX_WIDTH`] and PERSISTED across restarts
//! (`$XDG_STATE_HOME/dontspeak/overlay-width`). Position is NOT persisted across restarts:
//! GTK4 exposes no toplevel-positioning API (Wayland forbids it, X11 dropped it), so a
//! drag only lasts the session (the window is hidden, never destroyed, so it stays put).
//! The `gtk4-layer-shell` path stays compositor-anchored + fixed-width — a layer surface
//! can't be user-moved or -resized; that's the same Wayland limitation as the focus gate.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::status::Snapshot;
use ds_status::DictationState;

/// Default pill width; the user's dragged width is persisted and restored over this.
const DEFAULT_WIDTH: i32 = 460;
/// Horizontal-resize bounds (px) + the side-edge grab margin within which a press
/// resizes instead of moves — mirrors macOS `Overlay.min/maxWidth`/`edgeMargin`.
const MIN_WIDTH: i32 = 280;
const MAX_WIDTH: i32 = 900;
const EDGE: f64 = 8.0;

#[derive(Clone)]
pub struct Overlay {
    window: gtk::Window,
    label: gtk::Label,
    visible: Rc<Cell<bool>>,
    /// True on the plain-toplevel path where the pill is user-movable/-resizable, so
    /// `apply` knows to persist the width when it hides. False under layer-shell.
    resizable: bool,
}

impl Overlay {
    pub fn new(app: &adw::Application) -> Self {
        let display = gtk::gdk::Display::default();
        // `gtk4_layer_shell::is_supported()` ASSERTS a Wayland display (CRITICAL on X11), so
        // gate it on the actual display backend first — never call layer-shell under X11.
        let on_wayland = display
            .as_ref()
            .map(|d| d.type_().name().contains("Wayland"))
            .unwrap_or(false);
        let layer_shell = on_wayland && gtk4_layer_shell::is_supported();

        let window = gtk::Window::builder()
            .application(app)
            // Only the plain-toplevel path can be user-resized; a layer surface can't.
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
            // Floor the width so an edge-drag can't shrink the pill past readability;
            // the ceiling is enforced when the width is saved/restored (GTK toplevels
            // have no max-size). Matches macOS's clamp.
            window.set_size_request(MIN_WIDTH, -1);
            // CSS `background: transparent` relies on a compositor for alpha blending; without
            // one the window renders as opaque black instead of see-through.
            // `Display::is_composited()` (still present in gdk4-rs 0.11 — a prior version of
            // this comment incorrectly assumed GTK4 had removed it) is GDK's own documented
            // check for exactly this, and more precise than inferring from the backend: a
            // Wayland compositor always composites by definition, but plain X11 can go either
            // way — a bare tiling WM with no `picom`/`compton` doesn't, while GNOME/KDE-on-Xorg
            // does, and the backend-only heuristic wrongly painted the latter opaque too.
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

    /// Show/update or hide the overlay from a status push (same gate as the other hosts).
    pub fn apply(&self, snap: &Snapshot) {
        // Visibility is decided ONCE in the engine and shipped as the canonical
        // `dictation.state` token (vocabulary: `ds_status::DictationState`), so this pill
        // shows exactly when the macOS/Windows overlays do — including the REFUSED start
        // (the brief warning-glow pill is the feedback that the Caps tap did nothing).
        let show = match &snap.status {
            Some(s) => match DictationState::parse(&s.dictation.state) {
                Some(st) => st != DictationState::Hidden,
                // Older engine (no/unknown token): the legacy boolean derivation, removed
                // with the redundant booleans once every producer ships the token.
                None => {
                    s.dictation.awaiting_confirm
                        || (s.dictation.recording && s.dictation.local_stt)
                        || s.dictation.refused
                }
            },
            None => false,
        };

        if !show {
            if self.visible.replace(false) {
                // Persist the width the user dragged to before hiding (still allocated
                // here; 0 once hidden). Position can't be persisted — see module docs.
                if self.resizable {
                    save_width(self.window.width());
                }
                self.window.set_visible(false);
            }
            return;
        }

        let s = snap.status.as_ref().expect("show implies Some");
        // Show the live transcript; empty while recording shows nothing (no Linux-local prompt
        // text — there is no shared i18n key for one, and the glow already cues "speak now").
        self.label.set_text(&s.dictation.text);
        // Orange glow: the engine-computed "speak now" hint, a missing paste target, OR a
        // refused start (the refusal REUSES the no-target warning glow verbatim).
        let glow = s.dictation.prompt_glow || !s.dictation.has_paste_target || s.dictation.refused;
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

/// Wire drag-to-move + horizontal edge-resize onto the pill (plain-toplevel path only).
///
/// One primary-button press gesture decides move vs. resize by where the press lands —
/// within `EDGE` of the left/right side resizes that edge (`begin_resize`), anywhere else
/// moves the whole window (`begin_move`) — exactly the macOS `DragView` split. Both hand
/// the interaction to the compositor, so it works on X11 and Mutter/Wayland alike without
/// GTK owning any positioning state. A motion controller shows the `ew-resize` cursor over
/// the side edges so the affordance is discoverable.
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

    // Hover feedback: a horizontal-resize cursor within the side-edge grab margin.
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

/// Where a press landed along the pill's width — a side edge (resize) or the body (move).
/// Pure so the move-vs-resize split is unit-tested without a display; mirrors macOS
/// `DragView`'s `edgeMargin` hit test.
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

// ── width persistence ────────────────────────────────────────────────────────
//
// `$XDG_STATE_HOME/dontspeak/overlay-width` (fallback `~/.local/state/…`) — the same
// local-state root the engine's capskey marker uses, computed here directly to keep this
// host dependency-light. Only the width is stored (macOS/Windows resize is width-only too).

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

/// The persisted width, clamped; `DEFAULT_WIDTH` when unset or unparseable.
fn load_width() -> i32 {
    width_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(clamp_width)
        .unwrap_or(DEFAULT_WIDTH)
}

/// Persist the current width (clamped). Best-effort — a failure just forgets the size.
fn save_width(w: i32) {
    if w <= 0 {
        return; // not allocated (already hidden) — nothing meaningful to store.
    }
    if let Some(p) = width_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&p, clamp_width(w).to_string());
    }
}

/// The overlay (and panel) styling, loaded once into the default display.
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
        assert_eq!(hit_test(EDGE, w), Hit::LeftEdge); // inclusive at the margin
        assert_eq!(hit_test(EDGE + 0.1, w), Hit::Body);
        assert_eq!(hit_test(w / 2.0, w), Hit::Body);
        assert_eq!(hit_test(w - EDGE - 0.1, w), Hit::Body);
        assert_eq!(hit_test(w - EDGE, w), Hit::RightEdge); // inclusive at the margin
        assert_eq!(hit_test(w, w), Hit::RightEdge);
    }

    #[test]
    fn width_clamps_to_bounds() {
        assert_eq!(clamp_width(100), MIN_WIDTH); // below floor
        assert_eq!(clamp_width(2000), MAX_WIDTH); // above ceiling
        assert_eq!(clamp_width(500), 500); // in range, untouched
        assert_eq!(clamp_width(MIN_WIDTH), MIN_WIDTH);
        assert_eq!(clamp_width(MAX_WIDTH), MAX_WIDTH);
    }
}
