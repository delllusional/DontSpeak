//! Platform abstraction for the dontspeak engine.
//!
//! Three capability traits split the OS-specific surface the engine needs:
//!   * [`KeyInjector`]     — synthesize the dictation key tap (down+up) that toggles recording.
//!   * [`FrontmostWindow`] — is a terminal the frontmost app? (focus gate)
//!   * [`CapsKeyMonitor`]  — physical Caps key down/up edges (the gesture source) + the
//!     Caps LED write (driven as a pure output).
//!
//! [`Platform`] aggregates all three plus a one-time `preflight()` (permission
//! check). The free functions [`current()`] (the platform impl for the build
//! target) and [`is_mic_active()`] (system mic-in-use probe) dispatch to the per-OS
//! modules. The OS-independent [`KeyChord`]/[`KeyBase`] keybinding parser lives in
//! the `chord` module.
//!
//! All three ports are implemented, each behind its `cfg(target_os=…)`, and are
//! built + tested per-OS in CI (the release full matrix: Linux, Windows, macOS;
//! per-commit CI covers Linux only).

use std::error::Error;
use std::fmt;
use std::time::Instant;

mod chord;
pub use chord::{KeyBase, KeyChord};

/// One physical Caps-Lock key transition, captured the instant the OS reports it.
/// An event-driven platform (Windows' low-level keyboard hook) records these into a
/// queue the engine drains each tick — so a tap whose down AND up both land inside a
/// single poll gap is still replayed as a real down+up pair (never dropped). `at` is
/// the moment the edge occurred, used for the long-press threshold against the down.
#[derive(Clone, Copy, Debug)]
pub struct CapsEdge {
    /// `true` = key went DOWN, `false` = key came UP.
    pub down: bool,
    /// When the transition was observed (hook-callback time).
    pub at: Instant,
}

/// Injects the keypress that drives Claude Code voice dictation: TAP — one keypress
/// toggles recording on, the next toggles it off. The key is whatever Claude Code's
/// `voice:pushToTalk` is bound to (default `Space`), read from its config.
pub trait KeyInjector {
    /// Synthesize ONE discrete key tap (down then up) for `chord`. DEFAULT no-op so the
    /// Win/Linux stubs + minimal test fakes compile unchanged; the macOS impl overrides
    /// it. The CALLER (ds-stt) gates this on `is_inject_terminal_frontmost()` so the key
    /// never leaks outside an inject-eligible terminal (terminal-LIKE table rows such as
    /// Zed are excluded — see [`KnownTerminal::inject_keys`]). An unsupported chord is
    /// logged + skipped by the impl.
    fn tap_key(&self, _chord: &KeyChord) {}

    /// Inject `text` into the focused app (§C.3) — used by the local STT engines
    /// (Parakeet) to deliver a transcript. macOS prefers a clipboard-paste
    /// (arboard set + synth Cmd+V) over per-character Unicode events.
    ///
    /// Called by the PTT confirm path and always-listening stop-word path. Their
    /// explicit confirmation gesture is the intent gate; this is deliberately
    /// not restricted to a frontmost terminal. The default no-op keeps lightweight
    /// test/stub platforms compiling; all production OS implementations override it.
    fn type_text(&self, _text: &str) {}

    /// Press Return/Enter once (key down+up, no modifiers) — used by the
    /// always-listening loop to SUBMIT the prompt after the stopword fires.
    /// DEFAULT no-op (Win/Linux stubs + MockPlatform); the macOS impl overrides
    /// it. The CALLER gates this on `is_terminal_frontmost()`.
    fn press_enter(&self) {}
}

/// Log the single shared "can't synthesize the dictation key" error. Each port's
/// [`KeyInjector::tap_key`] calls this when its keycode map (Windows VK / macOS keycode /
/// Linux uinput) has no entry for the configured chord's base key — one user-facing
/// message, one source of truth instead of the same `eprintln!` copied into all three ports.
pub fn warn_unsupported_dictation_key(base: &KeyBase) {
    eprintln!(
        "dontspeak: can't synthesize claude_code dictation key {base:?} — bind voice:pushToTalk to Space or a Ctrl+<letter>"
    );
}

/// Restore the user's clipboard after a transcript paste ([`KeyInjector::type_text`]), OFF
/// the caller's thread. Every port's clipboard-paste delivery (Windows Ctrl+V / macOS Cmd+V
/// / Linux Ctrl+Shift+V) ends identically: spawn a thread, wait for the async paste to read
/// what we set, then put back the snapshot (`Some`) or clear what we left (`None`). Before
/// restoring, verify that the clipboard still contains our transcript: if the user or
/// another app copied something newer during the delay, that newer value wins.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn restore_clipboard_after_paste(prev: Option<String>, pasted: String) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if cb.get_text().ok().as_deref() != Some(pasted.as_str()) {
                return;
            }
            match prev {
                Some(p) => {
                    let _ = cb.set_text(p);
                }
                None => {
                    let _ = cb.clear();
                }
            }
        }
    });
}

/// Focus gate: only synthesize the dictation key / transcript while a terminal is
/// frontmost so the keystroke never leaks into another app.
pub trait FrontmostWindow {
    fn is_terminal_frontmost(&self) -> bool;

    /// Like [`is_terminal_frontmost`], but only `inject_keys: true` rows (+ `extra_terminals`).
    /// Gate for ClaudeNative PTT taps: Zed (`inject_keys: false`) is terminal for TTS/paste
    /// but must not receive synthesized keys. Default = full table; OS impls filter.
    /// Wayland fails closed (no portable frontmost query; same hazard as terminal gate).
    fn is_inject_terminal_frontmost(&self) -> bool {
        self.is_terminal_frontmost()
    }

    /// Is `app_tag` (e.g. `"zed"`) frontmost? Engine uses this at `start_recording` for
    /// frontend-owned PTT. Mapping: [`FRONTEND_APPS`]. Fails closed (wrong `false` =
    /// classic overlay/paste). Wayland also fails closed here — unlike terminal gates,
    /// fail-open would treat any subscription as always frontmost. Default `false`.
    fn is_app_frontmost(&self, _app_tag: &str) -> bool {
        false
    }

    /// The localized name of the frontmost application (e.g. "Ghostty",
    /// "Terminal"), captured on the Caps OFF→ON edge so the dictation confirm
    /// panel can show the paste target ("→ Terminal"). DEFAULT None so the
    /// Win/Linux stubs and the engine-test MockPlatform keep compiling; only the
    /// macOS impl overrides it.
    fn frontmost_app_name(&self) -> Option<String> {
        None
    }

    /// Whether the current focus appears able to accept pasted text.
    ///
    /// This is a best-effort presentation signal: the engine samples it while the
    /// dictation panel is visible and warns when false. It never gates delivery; a
    /// confirmed transcript is pasted into whatever is focused. Default `true` so
    /// platforms without a reliable probe fail open; macOS and Windows override it.
    fn can_paste(&self) -> bool {
        true
    }

    /// Add the user's extra terminal identifiers (config.toml `extra_terminals`) to this
    /// platform's `KNOWN_TERMINALS` union, in THIS OS's native form. Called once right
    /// after platform construction (`Engine::assemble`) and again on every
    /// `Engine::reload` (config.toml is mtime-watched, so a hand-edit + save takes effect
    /// live, no restart) — REPLACES the whole list each call, not additive. DEFAULT no-op
    /// so the engine-test `MockPlatform`/`ds-stt`'s own mocks and any future stub keep
    /// compiling unchanged; only the three OS impls override it.
    fn set_extra_terminals(&self, _extra: Vec<String>) {}

    /// Add the user's extra custom-text-editor identifiers (config.toml
    /// `extra_editors`) — mirrors `set_extra_terminals` but widens
    /// `can_paste()`'s custom-drawn-editor exemption instead of
    /// `is_terminal_frontmost()`'s table. Effective on Windows (`CUSTOM_TEXT_EXES`) and
    /// macOS (`CUSTOM_TEXT_BUNDLES`); Linux accepts-and-ignores the call because its
    /// `can_paste` is the always-true trait default (nothing to exempt), keeping
    /// the config field uniformly settable regardless of OS. DEFAULT no-op.
    fn set_extra_editors(&self, _extra: Vec<String>) {}
}

/// Physical Caps-Lock key down-duration + LED write — the signal §F needs for
/// long-press detection (measured off how long the physical key is held), plus
/// the Caps LED output the engine drives on each gesture edge.
pub trait CapsKeyMonitor {
    /// Whether the Caps Lock key is physically held *right now*, independent of
    /// the LED/toggle state. The engine stamps the first true and fires a reset
    /// if it stays true past `long_press_ms`.
    fn is_caps_physically_down(&self) -> bool;
    /// Force the Caps Lock LED/lock state (the drift-recovery write used by the
    /// long-press reset to drive the LED OFF, `set_caps_lock(false)`).
    fn set_caps_lock(&self, on: bool);

    /// Whether ANY of the platform's `IOHIDManagerOpen`-gated resources is confirmed
    /// STUCK: denied by the OS even though the permission it needs is already
    /// granted, in a way that won't self-heal without a fresh process. Default
    /// `false` — only macOS has this failure mode (see
    /// `ds_platform::macos::iohid`'s module doc); Windows' low-level hook and
    /// Linux's evdev read don't. On macOS this ORs together TWO independent call
    /// sites that each hit it (the caps-HID physical-key monitor, `iohid.rs`, AND
    /// the Caps-Lock LED writer, `led.rs`) — either one being stuck is reason
    /// enough to relaunch. The engine polls this next to its own permission
    /// re-probe and relaunches itself on `true` (see `dontspeakd::boot::engine_run`,
    /// which logs [`caps_monitor_stuck_detail`](Self::caps_monitor_stuck_detail)
    /// rather than this bare bool, so the log names which resource was actually
    /// responsible).
    fn caps_monitor_stuck(&self) -> bool {
        false
    }

    /// Human-readable detail for when [`caps_monitor_stuck`](Self::caps_monitor_stuck)
    /// is `true` — which resource(s), for logging. `None` when not stuck (or on
    /// platforms where `caps_monitor_stuck` is always `false`). This exists because
    /// each resource's own low-level warning is an `eprintln!` to raw process
    /// stderr — NOT the same destination as `dontspeakd`'s structured `log()`
    /// (`~/Library/Logs/DontSpeak/dontspeak.log`), and for a GUI-launched app
    /// stderr typically isn't captured anywhere a user or developer would see it.
    /// So the ONE line that reliably lands in the persisted log (the engine's own,
    /// via this detail) needs to be self-sufficient, not a pointer to a sibling
    /// line that may not exist anywhere visible.
    fn caps_monitor_stuck_detail(&self) -> Option<&'static str> {
        None
    }

    /// Whether this platform delivers Caps transitions as a lossless EVENT STREAM
    /// (drained via [`drain_caps_events`](Self::drain_caps_events)) rather than the
    /// engine sampling [`is_caps_physically_down`](Self::is_caps_physically_down) once per
    /// tick. Windows' low-level hook returns `true`; the polled platforms (macOS,
    /// Linux) and the test mock keep the DEFAULT `false`, so the engine drives them
    /// off the sampled boolean exactly as before. An event-driven platform fully
    /// SUPPRESSES the key (no OS caps TOGGLE, so no capitals), but `set_caps_lock`
    /// still drives the physical LED out-of-band as the dictation indicator — on
    /// Windows via `IOCTL_KEYBOARD_SET_INDICATORS`, matching the polled ports.
    fn is_caps_event_driven(&self) -> bool {
        false
    }

    /// Drain every Caps transition observed since the last call, oldest first. Only
    /// meaningful when [`is_caps_event_driven`](Self::is_caps_event_driven) is `true`; the
    /// DEFAULT returns empty so polled platforms and the mock are untouched.
    fn drain_caps_events(&self) -> Vec<CapsEdge> {
        Vec::new()
    }

    /// First phase of [`acquire_caps_key`]: establish any suppression that must be live
    /// before the existing logical Caps state is cleared. macOS installs its `hidutil`
    /// remap here; Windows installs and waits for its low-level hook; Linux uses the
    /// default because its synthetic normalization tap must still reach XKB. Return
    /// `false` when ownership could not be prepared; the shared acquisition sequence
    /// then skips normalization and completion. DEFAULT: ready with no work.
    fn begin_caps_key_acquisition(&self) -> bool {
        true
    }

    /// Second, mandatory phase of [`acquire_caps_key`]: clear any logical Caps state
    /// that predates DontSpeak's ownership and leave the indicator dark. The shared
    /// acquisition sequence calls this on every startup and OFF→ON acquisition, so a
    /// platform cannot install suppression while accidentally preserving capitals.
    /// DEFAULT: the platform's ordinary OFF writer; Linux and Windows override because
    /// their indicator writers are deliberately decoupled from the logical toggle.
    fn normalize_caps_lock(&self) {
        self.set_caps_lock(false);
    }

    /// Final phase of [`acquire_caps_key`]: install suppression that must happen after
    /// normalization. Linux applies XKB `caps:none` here, after its synthetic Caps tap;
    /// macOS and Windows use the default no-op because they suppress in the first phase.
    fn finish_caps_key_acquisition(&self) {}

    /// Release ownership taken by [`acquire_caps_key`],
    /// restoring the key's native OS behavior. Idempotent — safe to call when not
    /// currently owned (including at startup if caps dictation starts disabled).
    /// Called on every ON→OFF live toggle and at final shutdown. Implementations
    /// MUST also discard any already-queued-but-undelivered Caps edges (Windows'
    /// `drain_caps_events` backlog) — otherwise a burst of presses made while
    /// released replays in full the instant the key is re-acquired, desyncing the
    /// tap/double-tap gesture state machine. DEFAULT no-op.
    fn release_caps_key(&self) {}
}

/// Take ownership of the physical Caps key through one shared cross-platform sequence:
/// prepare suppression where required, normalize any pre-existing logical Caps state,
/// then finish suppression where normalization had to run first. This is the only
/// acquisition entry point; platforms implement the phase primitives above rather than
/// duplicating (and potentially omitting) the normalization policy.
pub fn acquire_caps_key(monitor: &(impl CapsKeyMonitor + ?Sized)) {
    if !monitor.begin_caps_key_acquisition() {
        return;
    }
    monitor.normalize_caps_lock();
    monitor.finish_caps_key_acquisition();
}

#[cfg(test)]
mod caps_key_acquisition_tests {
    use std::cell::RefCell;

    use super::*;

    struct Probe {
        begin_ok: bool,
        calls: RefCell<Vec<&'static str>>,
    }

    impl CapsKeyMonitor for Probe {
        fn is_caps_physically_down(&self) -> bool {
            false
        }

        fn set_caps_lock(&self, on: bool) {
            assert!(!on, "acquisition must normalize Caps Lock to OFF");
            self.calls.borrow_mut().push("normalize");
        }

        fn begin_caps_key_acquisition(&self) -> bool {
            self.calls.borrow_mut().push("begin");
            self.begin_ok
        }

        fn finish_caps_key_acquisition(&self) {
            self.calls.borrow_mut().push("finish");
        }
    }

    #[test]
    fn acquisition_always_normalizes_between_the_platform_phases() {
        let probe = Probe {
            begin_ok: true,
            calls: RefCell::new(Vec::new()),
        };

        acquire_caps_key(&probe);

        assert_eq!(*probe.calls.borrow(), ["begin", "normalize", "finish"]);
    }

    #[test]
    fn failed_preparation_does_not_mutate_caps_state_or_finish_ownership() {
        let probe = Probe {
            begin_ok: false,
            calls: RefCell::new(Vec::new()),
        };

        acquire_caps_key(&probe);

        assert_eq!(*probe.calls.borrow(), ["begin"]);
    }
}

/// One platform's full capability set.
pub trait Platform: KeyInjector + FrontmostWindow + CapsKeyMonitor {
    /// One-time startup check (e.g. macOS Accessibility trust). Returns an
    /// error the engine prints before exiting non-zero. SILENT and repeatable —
    /// the caps re-probe loop calls it on a timer, so it must never prompt.
    fn preflight(&self) -> Result<(), PreflightError>;

    /// One-time startup PROMPT for the OS permissions the engine needs (macOS
    /// Accessibility). Unlike [`Platform::preflight`] this MAY pop a system dialog
    /// and register the app in the permission list, so it must be called exactly
    /// ONCE at startup — never from the re-probe loop. Default: no-op (Linux/Windows
    /// grant input access via udev / no prompt).
    fn request_permissions(&self) {}
}

#[derive(Debug)]
pub struct PreflightError(pub String);

impl fmt::Display for PreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl Error for PreflightError {}

// ---- per-OS modules --------------------------------------------------------
// Key synthesis (`KeyInjector`) + Caps-Lock LED (`CapsKeyMonitor`) are
// implemented NATIVELY per OS below — one correct, self-maintained impl each, no library.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOsPlatform;

// Cross-platform mic-in-use watcher (push interface; CoreAudio listener on macOS, poll
// thread elsewhere). Lives above the per-OS modules so it can dispatch to `is_mic_active()`.
mod mic_watch;
pub use mic_watch::{MicState, MicWatcher};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsPlatform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxPlatform;

/// Construct the platform impl for the current build target.
#[cfg(target_os = "macos")]
pub fn current() -> Result<MacOsPlatform, PreflightError> {
    MacOsPlatform::new()
}

#[cfg(target_os = "windows")]
pub fn current() -> Result<WindowsPlatform, PreflightError> {
    WindowsPlatform::new()
}

#[cfg(target_os = "linux")]
pub fn current() -> Result<LinuxPlatform, PreflightError> {
    LinuxPlatform::new()
}

/// One entry in the shared "known terminal" table — the apps whose main text surface
/// isn't visible to at least one OS's accessibility API, so `is_terminal_frontmost()`
/// still treats a synthetic paste/tap as landing on a valid target. `name` is for
/// readability/debugging only and is never matched against anything; each platform
/// field is `Some` only where this app needs (and has) an identifier on that OS — a
/// platform-only terminal (PowerShell, GNOME Terminal) leaves the other two `None`.
/// An app that ships TWO identifiers on the SAME OS (WezTerm's modern vs. legacy exe/
/// wm_class, Ghostty's two Linux wm_class spellings) gets one extra row rather than a
/// list-valued field, since every platform's lookup just filters non-`None` values
/// across ALL rows regardless of how many rows one logical app spans.
pub struct KnownTerminal {
    pub name: &'static str,
    pub windows_exe: Option<&'static str>,
    pub macos_bundle: Option<&'static str>,
    pub linux_wm_class: Option<&'static str>,
    /// Synthesize keys for `claude_code` PTT? True for real terminals; false for
    /// terminal-like frontends (Zed) that join the TTS focus gate but must not receive
    /// the push-to-talk chord (see [`FrontmostWindow::is_inject_terminal_frontmost`]).
    pub inject_keys: bool,
}

/// Shared "which app counts as a terminal" for `is_terminal_frontmost()`.
/// `inject_keys: true` = former per-OS terminal lists; `false` = Zed-style focus-only.
/// Extend via config `extra_terminals` (inject-eligible) at lookup time.
pub const KNOWN_TERMINALS: &[KnownTerminal] = &[
    // ---- cross-platform ---------------------------------------------------------
    KnownTerminal {
        name: "Alacritty",
        windows_exe: Some("alacritty.exe"),
        macos_bundle: None,
        linux_wm_class: Some("alacritty"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Kitty",
        windows_exe: Some("kitty.exe"),
        macos_bundle: None,
        linux_wm_class: Some("kitty"),
        inject_keys: true,
    },
    // WezTerm ships a modern canonical identifier and a legacy/alternate one on BOTH
    // Windows and Linux; both rows must stay present.
    KnownTerminal {
        name: "WezTerm",
        windows_exe: Some("wezterm-gui.exe"),
        macos_bundle: None,
        linux_wm_class: Some("org.wezfurlong.wezterm"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "WezTerm (legacy exe / bare wm_class)",
        windows_exe: Some("wezterm.exe"),
        macos_bundle: None,
        linux_wm_class: Some("wezterm"),
        inject_keys: true,
    },
    // Ghostty: one macOS bundle id + two Linux wm_class spellings.
    KnownTerminal {
        name: "Ghostty",
        windows_exe: None,
        macos_bundle: Some("com.mitchellh.ghostty"),
        linux_wm_class: Some("com.mitchellh.ghostty"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Ghostty (bare wm_class)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("ghostty"),
        inject_keys: true,
    },
    // ---- Windows-only -------------------------------------------------------------
    KnownTerminal {
        name: "Windows Terminal",
        windows_exe: Some("windowsterminal.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Windows Terminal's console host",
        windows_exe: Some("openconsole.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "classic console host",
        windows_exe: Some("conhost.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Windows PowerShell 5.1",
        windows_exe: Some("powershell.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "PowerShell 7+",
        windows_exe: Some("pwsh.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Command Prompt",
        windows_exe: Some("cmd.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Hyper",
        windows_exe: Some("hyper.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Git Bash / MSYS2 (mintty)",
        windows_exe: Some("mintty.exe"),
        macos_bundle: None,
        linux_wm_class: None,
        inject_keys: true,
    },
    // ---- macOS-only -----------------------------------------------------------
    KnownTerminal {
        name: "iTerm2",
        windows_exe: None,
        macos_bundle: Some("com.googlecode.iterm2"),
        linux_wm_class: None,
        inject_keys: true,
    },
    KnownTerminal {
        name: "Terminal.app",
        windows_exe: None,
        macos_bundle: Some("com.apple.Terminal"),
        linux_wm_class: None,
        inject_keys: true,
    },
    // ---- Linux-only -----------------------------------------------------------
    KnownTerminal {
        name: "GNOME Terminal (VTE)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("gnome-terminal-server"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Konsole (KDE)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("konsole"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "xterm",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("xterm"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "uxterm",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("uxterm"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "urxvt",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("urxvt"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "rxvt",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("rxvt"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Terminator",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("terminator"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Tilix",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("tilix"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Xfce Terminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("xfce4-terminal"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "QTerminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("qterminal"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "LXTerminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("lxterminal"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "MATE Terminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("mate-terminal"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "st (suckless)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("st"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "foot",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("foot"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "foot (client mode)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("footclient"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Terminology",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("terminology"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Guake",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("guake"),
        inject_keys: true,
    },
    KnownTerminal {
        name: "Tilda",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("tilda"),
        inject_keys: true,
    },
    // ---- terminal-LIKE apps (focus gate only — never key injection) -------------
    // Zed hosts CLI agents in embedded terminals, so for the TTS focus gate
    // (`pause_in_background`) and the `can_paste` short-circuits it must count
    // as "a terminal is frontmost" — but `inject_keys: false` keeps the `claude_code`
    // engine's push-to-talk chord out of Zed's buffers (see `KnownTerminal::inject_keys`).
    // Identities repeat FRONTEND_APPS / per-OS CUSTOM_TEXT_*. Linux wm_class rule:
    // store lowercased (X11 lookup lowercases; KNOWN_TERMINALS matches byte-exact).
    KnownTerminal {
        name: "Zed",
        windows_exe: Some("zed.exe"),
        macos_bundle: Some("dev.zed.Zed"),
        linux_wm_class: Some("dev.zed.zed"),
        inject_keys: false,
    },
    KnownTerminal {
        name: "Zed Preview",
        windows_exe: Some("zed-preview.exe"),
        macos_bundle: Some("dev.zed.Zed-Preview"),
        linux_wm_class: Some("dev.zed.zed-preview"),
        inject_keys: false,
    },
    KnownTerminal {
        name: "Zed Dev",
        windows_exe: Some("zed-dev.exe"),
        macos_bundle: Some("dev.zed.Zed-Dev"),
        linux_wm_class: Some("dev.zed.zed-dev"),
        inject_keys: false,
    },
];

// ── Frontend-app identity table (subscriber frontmost matching) ─────────────
//
// Maps the `app` tag a frontend sends in `subscribe_frontend` (e.g. "zed") to that
// app's per-OS process identities, for `FrontmostWindow::is_app_frontmost()`. Kept
// SEPARATE from `KNOWN_TERMINALS` (which gates key/transcript injection and mic
// pause-in-background) and from the per-OS `CUSTOM_TEXT_*` paste exemptions (which
// gate `can_paste`) — this table gates only "may a live subscriber own the
// in-flight dictation", and folding it into either of those would change unrelated
// behavior. Identities intentionally repeat the ones already vetted for Zed in
// `CUSTOM_TEXT_BUNDLES` (macos.rs) / `CUSTOM_TEXT_EXES` (windows.rs).
//
// Linux wm_class casing rule (same as KNOWN_TERMINALS): store lowercased. Match
// helpers are also case-insensitive as belt-and-suspenders for other OS paths.

/// One frontend app: the wire tag plus every identity it presents per OS.
/// List-valued fields (unlike [`KnownTerminal`]'s one-identity-per-row shape)
/// because a tag is looked up as a unit — one row per logical app.
pub struct FrontendApp {
    /// `subscribe_frontend` wire tag (lowercase by convention).
    pub tag: &'static str,
    /// Windows process basenames (lowercased).
    pub windows_exes: &'static [&'static str],
    /// macOS bundle ids (canonical case).
    pub macos_bundles: &'static [&'static str],
    /// Linux X11 WM_CLASS names (lowercased — same rule as [`KNOWN_TERMINALS`]).
    pub linux_wm_classes: &'static [&'static str],
}

/// Frontends the daemon can frontmost-match (Zed only today).
/// Zed ships three channels; list every OS identity so Stable / Preview / Dev all
/// own `subscribe_frontend` dictations.
pub const FRONTEND_APPS: &[FrontendApp] = &[FrontendApp {
    tag: "zed",
    windows_exes: &["zed.exe", "zed-preview.exe", "zed-dev.exe"],
    macos_bundles: &["dev.zed.Zed", "dev.zed.Zed-Preview", "dev.zed.Zed-Dev"],
    // Lowercased (X11 path lowercases; same storage rule as KNOWN_TERMINALS).
    linux_wm_classes: &["dev.zed.zed", "dev.zed.zed-preview", "dev.zed.zed-dev"],
}];

fn frontend_app(tag: &str) -> Option<&'static FrontendApp> {
    FRONTEND_APPS
        .iter()
        .find(|a| a.tag.eq_ignore_ascii_case(tag))
}

/// Known `subscribe_frontend` tag? (Not used by Wayland `is_app_frontmost` — that fails closed.)
pub fn frontend_tag_known(tag: &str) -> bool {
    frontend_app(tag).is_some()
}

/// Case-insensitive: does `tag` own Windows basename `exe`?
pub fn frontend_tag_matches_windows_exe(tag: &str, exe: &str) -> bool {
    frontend_app(tag).is_some_and(|a| a.windows_exes.iter().any(|e| e.eq_ignore_ascii_case(exe)))
}

/// Case-insensitive: does `tag` own macOS bundle `bundle`?
pub fn frontend_tag_matches_macos_bundle(tag: &str, bundle: &str) -> bool {
    frontend_app(tag).is_some_and(|a| {
        a.macos_bundles
            .iter()
            .any(|b| b.eq_ignore_ascii_case(bundle))
    })
}

/// Case-insensitive: does `tag` own X11 WM_CLASS `wm_class`?
pub fn frontend_tag_matches_linux_wm_class(tag: &str, wm_class: &str) -> bool {
    frontend_app(tag).is_some_and(|a| {
        a.linux_wm_classes
            .iter()
            .any(|c| c.eq_ignore_ascii_case(wm_class))
    })
}

// ── Microphone-in-use probe (TTS feedback gate) ──────────────────────────────
//
// Whether the default audio INPUT device is capturing RIGHT NOW (the mic is in
// use anywhere on the system) — true while Claude Code's voice dictation, the
// engine's own Parakeet STT, or any other app is recording. The TTS paths use
// this to hold/skip playback so speech never feeds back into a live recording.
//
// Claude Code exposes no recording-state hook/signal, so we read it from the OS.
// macOS: CoreAudio `kAudioDevicePropertyDeviceIsRunningSomewhere` on the default
// input device. Windows: a WASAPI capture-session probe. Linux has no probe yet →
// `false` (no gate), which degrades to always-play.

/// Whether the default microphone is currently capturing (per-OS probe).
///
/// Thin dispatch to the per-OS probe (CoreAudio on macOS, WASAPI on Windows, a
/// no-gate fallback elsewhere); the implementation for each target lives in that
/// OS's module.
#[cfg(target_os = "macos")]
pub fn is_mic_active() -> bool {
    macos::is_mic_active()
}

/// Whether the default microphone is currently capturing (per-OS probe).
#[cfg(windows)]
pub fn is_mic_active() -> bool {
    windows::is_mic_active()
}

/// Stub for platforms with no mic probe yet (Linux): never gate TTS (always play).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_mic_active() -> bool {
    false
}

/// Detach this process from any console it inherited or was implicitly given (Windows
/// only; a no-op elsewhere — only Windows ties a console-subsystem process to an
/// auto-created window). See `windows::detach_console` for why `dontspeak.exe` needs this
/// for its non-interactive roles.
#[cfg(windows)]
pub fn detach_console() {
    windows::detach_console();
}

/// No-op on platforms with no console/subsystem distinction.
#[cfg(not(windows))]
pub fn detach_console() {}

#[cfg(test)]
mod known_terminal_inject_split {
    use super::*;

    /// The `inject_keys: false` rows must be EXACTLY the Zed channel rows — every
    /// real terminal emulator stays inject-eligible (a regression here would silently
    /// disable the `claude_code` engine's push-to-talk tap for that terminal).
    #[test]
    fn only_the_zed_rows_opt_out_of_key_injection() {
        let non_inject: Vec<&KnownTerminal> =
            KNOWN_TERMINALS.iter().filter(|t| !t.inject_keys).collect();
        assert_eq!(
            non_inject.iter().map(|t| t.name).collect::<Vec<_>>(),
            vec!["Zed", "Zed Preview", "Zed Dev"],
            "inject_keys:false is reserved for terminal-LIKE frontends (Zed)"
        );
        // Identities stay in sync with FRONTEND_APPS (Linux stored lowercased for
        // the X11 byte-exact match — see the table comment).
        assert_eq!(non_inject[0].windows_exe, Some("zed.exe"));
        assert_eq!(non_inject[0].macos_bundle, Some("dev.zed.Zed"));
        assert_eq!(non_inject[0].linux_wm_class, Some("dev.zed.zed"));
        assert_eq!(non_inject[1].windows_exe, Some("zed-preview.exe"));
        assert_eq!(non_inject[1].macos_bundle, Some("dev.zed.Zed-Preview"));
        assert_eq!(non_inject[1].linux_wm_class, Some("dev.zed.zed-preview"));
        assert_eq!(non_inject[2].windows_exe, Some("zed-dev.exe"));
        assert_eq!(non_inject[2].macos_bundle, Some("dev.zed.Zed-Dev"));
        assert_eq!(non_inject[2].linux_wm_class, Some("dev.zed.zed-dev"));
    }

    /// The trait default must keep mocks/stubs on today's behavior: every
    /// terminal is inject-eligible unless the platform overrides the split.
    #[test]
    fn default_inject_gate_delegates_to_is_terminal_frontmost() {
        struct Stub(bool);
        impl FrontmostWindow for Stub {
            fn is_terminal_frontmost(&self) -> bool {
                self.0
            }
        }
        assert!(Stub(true).is_inject_terminal_frontmost());
        assert!(!Stub(false).is_inject_terminal_frontmost());
    }
}

#[cfg(test)]
mod frontend_app_matching {
    use super::*;

    #[test]
    fn zed_tag_matches_each_platform_identity() {
        assert!(frontend_tag_matches_windows_exe("zed", "zed.exe"));
        assert!(frontend_tag_matches_windows_exe("zed", "zed-preview.exe"));
        assert!(frontend_tag_matches_windows_exe("zed", "zed-dev.exe"));
        assert!(frontend_tag_matches_macos_bundle("zed", "dev.zed.Zed"));
        assert!(frontend_tag_matches_macos_bundle(
            "zed",
            "dev.zed.Zed-Preview"
        ));
        assert!(frontend_tag_matches_macos_bundle("zed", "dev.zed.Zed-Dev"));
        assert!(frontend_tag_matches_linux_wm_class("zed", "dev.zed.Zed"));
        assert!(frontend_tag_matches_linux_wm_class(
            "zed",
            "dev.zed.Zed-Preview"
        ));
        assert!(frontend_tag_matches_linux_wm_class("zed", "dev.zed.Zed-Dev"));
    }

    #[test]
    fn matching_is_case_insensitive_in_identity_and_tag() {
        // Windows basenames arrive pre-lowercased; X11 wm_class arrives lowercased by
        // the lookup path; macOS bundle ids arrive case-preserved. All must match.
        assert!(frontend_tag_matches_windows_exe("zed", "Zed.EXE"));
        assert!(frontend_tag_matches_macos_bundle("zed", "dev.zed.zed"));
        assert!(frontend_tag_matches_linux_wm_class("zed", "dev.zed.zed"));
        assert!(frontend_tag_matches_windows_exe("Zed", "zed.exe"));
    }

    #[test]
    fn unknown_tag_matches_nothing_anywhere() {
        assert!(!frontend_tag_known("vscode"));
        assert!(!frontend_tag_matches_windows_exe("vscode", "code.exe"));
        assert!(!frontend_tag_matches_macos_bundle(
            "vscode",
            "com.microsoft.VSCode"
        ));
        assert!(!frontend_tag_matches_linux_wm_class("vscode", "code"));
        // Even an identity Zed DOES own must not match under the wrong tag.
        assert!(!frontend_tag_matches_windows_exe("vscode", "zed.exe"));
    }

    #[test]
    fn zed_tag_rejects_foreign_identities() {
        assert!(!frontend_tag_matches_windows_exe("zed", "code.exe"));
        assert!(!frontend_tag_matches_windows_exe("zed", "zed")); // basename keeps .exe
        assert!(!frontend_tag_matches_macos_bundle("zed", "dev.zed"));
        assert!(!frontend_tag_matches_linux_wm_class("zed", "zed"));
    }

    #[test]
    fn known_tag_probe_matches_table() {
        assert!(frontend_tag_known("zed"));
        assert!(frontend_tag_known("ZED")); // tag lookup is case-insensitive
        assert!(!frontend_tag_known(""));
    }

    #[test]
    fn default_trait_impl_is_never_frontend_owned() {
        struct Stub;
        impl FrontmostWindow for Stub {
            fn is_terminal_frontmost(&self) -> bool {
                false
            }
        }
        assert!(!Stub.is_app_frontmost("zed"));
    }

    #[test]
    fn zed_frontend_apps_table_snapshot() {
        let zed = frontend_app("zed").expect("zed row present");
        assert_eq!(
            zed.windows_exes,
            &["zed.exe", "zed-preview.exe", "zed-dev.exe"]
        );
        assert_eq!(
            zed.macos_bundles,
            &["dev.zed.Zed", "dev.zed.Zed-Preview", "dev.zed.Zed-Dev"]
        );
        assert_eq!(
            zed.linux_wm_classes,
            &["dev.zed.zed", "dev.zed.zed-preview", "dev.zed.zed-dev"]
        );
    }
}
