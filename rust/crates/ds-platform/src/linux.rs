//! Linux platform impl — behind cfg(target_os="linux").
//!
//! Mirrors the macOS design (LED poll), NOT the Windows key-suppress hook:
//!
//! * Caps LED (`set_caps_lock`): a pure OUTPUT — the engine writes EV_LED/LED_CAPSL
//!   on each gesture edge (lit = recording), never reading the state back. We do NOT
//!   grab/suppress the key, so the user's own Caps still works. Reading the physical
//!   Caps key (below) needs membership in the `input` group (udev-rule.txt).
//! * Physical down (`caps_physically_down`, §F long-press): the LED can't tell hold
//!   duration, so we drain EV_KEY/KEY_CAPSLOCK events (non-blocking) each tick and cache
//!   the last down/up — the macOS IOHIDManager analogue.
//! * Dictation key / transcript (`KeyInjector`): synthesize via an evdev
//!   `uinput::VirtualDevice` — press the chord's modifiers + base key, then release.
//!   `type_text` pastes through the clipboard (arboard) + a synthesized Ctrl+Shift+V
//!   (the clipboard paste in VTE/konsole/kitty/alacritty terminals), like the other ports.
//! * Frontmost (`terminal_frontmost`): under X11, `x11rb` reads `_NET_ACTIVE_WINDOW` +
//!   that window's `WM_CLASS` and matches a terminal list. Under WAYLAND there is no
//!   portable active-window API; behaviour is DEGRADED — the gate fails OPEN (always
//!   emits), acceptable because Wayland compositors already isolate input per surface.
//!
//! Construction NEVER fails: like the macOS port, `new()` always returns a platform and
//! the capability check (keyboard + /dev/uinput access) is reported by `preflight()` as a
//! NON-FATAL warning — so the engine stays up as the resident RPC/TTS/STT service even
//! when the input devices are unavailable (no `input`-group membership yet, or a headless/
//! WSL/container host with no real keyboard). Caps dictation simply self-gates OFF, and a
//! later `usermod` + reload (the engine re-probes) enables it without a restart.

use std::cell::{Cell, RefCell};
use std::io;

use evdev::{
    AttributeSet, Device, EventSummary, EventType, InputEvent, KeyCode, LedCode,
    SynchronizationCode, uinput::VirtualDevice,
};

use crate::{
    CapsKeyMonitor, FrontmostWindow, KeyBase, KeyChord, KeyInjector, Platform, PreflightError,
};

/// WM_CLASS values (lowercased, second/class field) that count as "a terminal is
/// frontmost" for the dictation-key / transcript-injection focus gate. Mirrors the
/// macOS TERM_BUNDLES / Windows TERM_EXES allowlists for the common Linux terminals.
const TERM_WM_CLASSES: &[&str] = &[
    "gnome-terminal-server", // GNOME Terminal (VTE)
    "konsole",               // KDE
    "xterm",
    "uxterm",
    "urxvt",
    "rxvt",
    "terminator",
    "tilix",
    "xfce4-terminal",
    "qterminal",
    "lxterminal",
    "mate-terminal",
    "kitty",
    "alacritty",
    "org.wezfurlong.wezterm",
    "wezterm",
    "st", // suckless
    "foot",
    "footclient",
    "com.mitchellh.ghostty",
    "ghostty",
    "terminology",
    "guake",
    "tilda",
];

/// Map a [`KeyBase`] to its Linux evdev key code (US-QWERTY physical position — uinput
/// emits scancodes that the compositor maps to keysyms via the active layout). `None`
/// for `Unsupported`. Linux keycodes are NOT alphabetical, hence the explicit table.
fn key_for_base(base: &KeyBase) -> Option<KeyCode> {
    Some(match base {
        KeyBase::Space => KeyCode::KEY_SPACE,
        KeyBase::Enter => KeyCode::KEY_ENTER,
        KeyBase::Tab => KeyCode::KEY_TAB,
        KeyBase::Escape => KeyCode::KEY_ESC,
        KeyBase::Letter(c) => match c.to_ascii_lowercase() {
            'a' => KeyCode::KEY_A,
            'b' => KeyCode::KEY_B,
            'c' => KeyCode::KEY_C,
            'd' => KeyCode::KEY_D,
            'e' => KeyCode::KEY_E,
            'f' => KeyCode::KEY_F,
            'g' => KeyCode::KEY_G,
            'h' => KeyCode::KEY_H,
            'i' => KeyCode::KEY_I,
            'j' => KeyCode::KEY_J,
            'k' => KeyCode::KEY_K,
            'l' => KeyCode::KEY_L,
            'm' => KeyCode::KEY_M,
            'n' => KeyCode::KEY_N,
            'o' => KeyCode::KEY_O,
            'p' => KeyCode::KEY_P,
            'q' => KeyCode::KEY_Q,
            'r' => KeyCode::KEY_R,
            's' => KeyCode::KEY_S,
            't' => KeyCode::KEY_T,
            'u' => KeyCode::KEY_U,
            'v' => KeyCode::KEY_V,
            'w' => KeyCode::KEY_W,
            'x' => KeyCode::KEY_X,
            'y' => KeyCode::KEY_Y,
            'z' => KeyCode::KEY_Z,
            _ => return None,
        },
        KeyBase::Unsupported(_) => return None,
    })
}

/// Does this device look like the keyboard? (exposes both KEY_CAPSLOCK and the LED_CAPSL
/// LED — the source the macOS-style LED poll needs.) First match wins.
fn is_caps_keyboard(dev: &Device) -> bool {
    let has_caps = dev
        .supported_keys()
        .is_some_and(|k| k.contains(KeyCode::KEY_CAPSLOCK));
    let has_led = dev
        .supported_leds()
        .is_some_and(|l| l.contains(LedCode::LED_CAPSL));
    has_caps && has_led
}

/// Discover the keyboard evdev node (first device exposing KEY_CAPSLOCK + LED_CAPSL) and
/// set it non-blocking (the per-tick `caps_physically_down` drain must never block the
/// poll thread). `Err` carries an actionable message for `preflight`.
fn open_keyboard() -> Result<Device, String> {
    let dev = evdev::enumerate()
        .find(|(_, d)| is_caps_keyboard(d))
        .map(|(_, d)| d)
        .ok_or_else(|| {
            "no keyboard evdev device with Caps-Lock + LED found under /dev/input \
             (add yourself to the `input` group — see apps/linux/udev-rule.txt — or, under \
             WSL/containers, no real keyboard device is exposed)"
                .to_string()
        })?;
    dev.set_nonblocking(true)
        .map_err(|e| format!("set evdev keyboard non-blocking: {e}"))?;
    Ok(dev)
}

/// Build the uinput virtual keyboard, advertising every key we synthesize so the kernel
/// accepts the emitted events. Needs write access to /dev/uinput (udev rule + `input` group).
fn open_uinput() -> Result<VirtualDevice, String> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for k in [
        KeyCode::KEY_LEFTCTRL,
        KeyCode::KEY_LEFTSHIFT,
        KeyCode::KEY_LEFTALT,
        KeyCode::KEY_LEFTMETA,
        KeyCode::KEY_SPACE,
        KeyCode::KEY_ENTER,
        KeyCode::KEY_TAB,
        KeyCode::KEY_ESC,
        KeyCode::KEY_V,
    ] {
        keys.insert(k);
    }
    for c in b'a'..=b'z' {
        if let Some(k) = key_for_base(&KeyBase::Letter(c as char)) {
            keys.insert(k);
        }
    }
    VirtualDevice::builder()
        .map_err(|e| {
            format!("open /dev/uinput: {e} (is the udev rule installed and are you in the `input` group?)")
        })?
        .name("DontSpeak virtual keyboard")
        .with_keys(&keys)
        .map_err(|e| format!("uinput with_keys: {e}"))?
        .build()
        .map_err(|e| format!("uinput build: {e}"))
}

pub struct LinuxPlatform {
    /// The keyboard device — read for the Caps LED (`EVIOCGLED` ioctl) and drained for
    /// KEY_CAPSLOCK key events (long-press). `None` when no keyboard is reachable (the
    /// engine then runs as the RPC/TTS/STT service with Caps dictation off). `RefCell`
    /// because the engine drives the platform from its single poll thread (trait methods
    /// are `&self`, `fetch_events` is `&mut`).
    kbd: Option<RefCell<Device>>,
    /// Virtual-keyboard sink for `tap_key`/`type_text`/`press_enter`. `None` when
    /// /dev/uinput is not writable (key/transcript injection then no-ops).
    uinput: Option<RefCell<VirtualDevice>>,
    /// Last observed physical Caps-key state (true = held), updated by draining EV_KEY.
    caps_down: Cell<bool>,
    /// X11 focus-gate connection (None on Wayland or when X is unreachable).
    x11: Option<X11Focus>,
    wayland: bool,
    /// Why the input devices are unavailable, for the non-fatal `preflight()` warning;
    /// `None` once both the keyboard and uinput opened.
    init_warning: Option<String>,
}

impl LinuxPlatform {
    pub fn new() -> Result<Self, PreflightError> {
        // Construction never fails (macOS parity): capability gaps become a preflight
        // WARNING so the engine stays up as the RPC/TTS/STT service regardless.
        let (kbd, kbd_err) = match open_keyboard() {
            Ok(d) => (Some(RefCell::new(d)), None),
            Err(e) => (None, Some(e)),
        };
        let (uinput, uin_err) = match open_uinput() {
            Ok(u) => (Some(RefCell::new(u)), None),
            Err(e) => (None, Some(e)),
        };
        let init_warning = match (kbd_err, uin_err) {
            (None, None) => None,
            (a, b) => Some([a, b].into_iter().flatten().collect::<Vec<_>>().join("; ")),
        };

        let wayland = Self::wayland_session();
        // X11 focus gate only when not Wayland (Wayland fails open — see module docs).
        let x11 = if wayland { None } else { X11Focus::connect() };

        Ok(LinuxPlatform {
            kbd,
            uinput,
            caps_down: Cell::new(false),
            x11,
            wayland,
            init_warning,
        })
    }

    fn wayland_session() -> bool {
        std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE")
                .map(|s| s == "wayland")
                .unwrap_or(false)
    }

    /// Drain any pending EV_KEY events, updating the cached physical Caps-down state.
    /// Non-blocking: `WouldBlock` (no events) leaves the cache untouched.
    fn pump_caps_events(&self) {
        let Some(kbd) = self.kbd.as_ref() else {
            return;
        };
        let mut kbd = kbd.borrow_mut();
        loop {
            match kbd.fetch_events() {
                Ok(events) => {
                    for ev in events {
                        if let EventSummary::Key(_, KeyCode::KEY_CAPSLOCK, value) = ev.destructure()
                        {
                            // 1 = down, 0 = up, 2 = autorepeat (ignore — a hold streams 2s).
                            match value {
                                1 => self.caps_down.set(true),
                                0 => self.caps_down.set(false),
                                _ => {}
                            }
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break, // transient read error — keep the cached state
            }
        }
    }

    /// Emit a key-down or key-up for `code`, followed by a SYN report. No-op without uinput.
    fn emit(&self, code: KeyCode, down: bool) {
        let Some(uinput) = self.uinput.as_ref() else {
            return;
        };
        let val = if down { 1 } else { 0 };
        let events = [
            InputEvent::new(EventType::KEY.0, code.0, val),
            InputEvent::new(
                EventType::SYNCHRONIZATION.0,
                SynchronizationCode::SYN_REPORT.0,
                0,
            ),
        ];
        let _ = uinput.borrow_mut().emit(&events);
    }
}

impl KeyInjector for LinuxPlatform {
    // Native evdev/uinput synthesis, ours — no library. The caller (ds-stt) gates
    // every method on `terminal_frontmost()`, so a key/transcript never leaks outside a
    // terminal. An unsupported chord is logged + skipped; no uinput ⇒ silent no-op.

    /// Tap the dictation chord once: press modifiers, press+release the base key, release
    /// modifiers (reverse). One discrete keypress = one toggle of Claude Code's voice TAP.
    fn tap_key(&self, chord: &KeyChord) {
        let Some(key) = key_for_base(&chord.base) else {
            crate::warn_unsupported_dictation_key(&chord.base);
            return;
        };
        let mods: &[(bool, KeyCode)] = &[
            (chord.ctrl, KeyCode::KEY_LEFTCTRL),
            (chord.shift, KeyCode::KEY_LEFTSHIFT),
            (chord.alt, KeyCode::KEY_LEFTALT),
            (chord.cmd, KeyCode::KEY_LEFTMETA),
        ];
        for &(on, m) in mods {
            if on {
                self.emit(m, true);
            }
        }
        self.emit(key, true);
        self.emit(key, false);
        for &(on, m) in mods.iter().rev() {
            if on {
                self.emit(m, false);
            }
        }
    }

    fn type_text(&self, text: &str) {
        // Deliver the transcript via clipboard + Ctrl+Shift+V (the clipboard-paste chord in
        // VTE/konsole/kitty/alacritty terminals) — ONE atomic paste, instant even for a long
        // transcript. (Per-character uinput Unicode is layout-dependent and crawls.)
        if text.is_empty() || self.uinput.is_none() {
            return;
        }
        let Ok(mut cb) = arboard::Clipboard::new() else {
            return;
        };
        // Snapshot the user's clipboard to RESTORE after the paste (None ⇒ clear instead).
        let prev = cb.get_text().ok();
        if cb.set_text(text.to_string()).is_err() {
            return;
        }
        // Ctrl+Shift+V.
        self.emit(KeyCode::KEY_LEFTCTRL, true);
        self.emit(KeyCode::KEY_LEFTSHIFT, true);
        self.emit(KeyCode::KEY_V, true);
        self.emit(KeyCode::KEY_V, false);
        self.emit(KeyCode::KEY_LEFTSHIFT, false);
        self.emit(KeyCode::KEY_LEFTCTRL, false);
        // Restore the user's clipboard off-thread once the async paste has read ours.
        crate::restore_clipboard_after_paste(prev);
    }

    fn press_enter(&self) {
        self.emit(KeyCode::KEY_ENTER, true);
        self.emit(KeyCode::KEY_ENTER, false);
    }
}

impl FrontmostWindow for LinuxPlatform {
    fn terminal_frontmost(&self) -> bool {
        if self.wayland {
            // Wayland: no portable active-window query. The architecture (§C.3) accepts
            // fail-OPEN here because Wayland compositors already isolate input per surface,
            // so a synthetic paste can only reach the surface the user themselves focused.
            return true;
        }
        // X11: read _NET_ACTIVE_WINDOW + WM_CLASS, match the terminal allowlist. FAIL-CLOSED
        // (return false) on any failure so a transcript never leaks into a non-terminal app.
        match &self.x11 {
            Some(x11) => x11.terminal_frontmost(),
            None => false,
        }
    }
}

impl CapsKeyMonitor for LinuxPlatform {
    fn caps_physically_down(&self) -> bool {
        // Drain pending KEY_CAPSLOCK transitions, then report the cached held-state. Reliable
        // physical down/up on Linux (the evdev key event is independent of the LED latch).
        self.pump_caps_events();
        self.caps_down.get()
    }

    fn set_caps_lock(&self, on: bool) {
        // §F drift-recovery: drive the real keyboard LED via an EV_LED write on the device.
        // The kernel reflects it onto the physical LED. Best-effort; no-op without a keyboard.
        if let Some(kbd) = self.kbd.as_ref() {
            let events = [InputEvent::new(
                EventType::LED.0,
                LedCode::LED_CAPSL.0,
                on as i32,
            )];
            let _ = kbd.borrow_mut().send_events(&events);
        }
    }
}

impl Platform for LinuxPlatform {
    fn preflight(&self) -> Result<(), PreflightError> {
        // NON-FATAL (boot.rs treats this as a warning): the engine stays up as the resident
        // RPC/TTS/STT service; only Caps-Lock dictation depends on the input devices.
        match &self.init_warning {
            Some(w) => Err(PreflightError(w.clone())),
            None => Ok(()),
        }
    }
}

// ── X11 focus gate ───────────────────────────────────────────────────────────
//
// _NET_ACTIVE_WINDOW (EWMH) on the root window names the focused window; its WM_CLASS
// "class" field identifies the app. We cache the connection + interned atom so the
// per-edge query is one round-trip. Reused across calls; all x11rb calls take &self.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, Window};
use x11rb::rust_connection::RustConnection;

struct X11Focus {
    conn: RustConnection,
    root: Window,
    net_active_window: u32,
}

impl X11Focus {
    fn connect() -> Option<Self> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots.get(screen_num)?.root;
        let net_active_window = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        Some(X11Focus {
            conn,
            root,
            net_active_window,
        })
    }

    fn active_window(&self) -> Option<Window> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;
        let win = reply.value32()?.next()?;
        if win == 0 { None } else { Some(win) }
    }

    fn terminal_frontmost(&self) -> bool {
        let Some(win) = self.active_window() else {
            return false;
        };
        // WM_CLASS is "instance\0class\0" (Latin-1). Match the CLASS (second) field, then
        // fall back to the instance, against the terminal allowlist (lowercased).
        let Ok(cookie) =
            self.conn
                .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        else {
            return false;
        };
        let Ok(reply) = cookie.reply() else {
            return false;
        };
        let mut parts = reply
            .value
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_ascii_lowercase());
        let instance = parts.next();
        let class = parts.next();
        [class, instance]
            .into_iter()
            .flatten()
            .any(|name| TERM_WM_CLASSES.contains(&name.as_str()))
    }
}

#[cfg(test)]
mod keycode_parity {
    use super::*;
    use crate::chord::all_supported_bases;

    #[test]
    fn every_supported_base_maps_to_a_keycode() {
        for b in all_supported_bases() {
            assert!(
                key_for_base(&b).is_some(),
                "Linux key_for_base has no evdev code for {b:?}"
            );
        }
    }

    #[test]
    fn unsupported_base_has_no_keycode() {
        assert!(key_for_base(&KeyBase::Unsupported("f5".into())).is_none());
    }
}
