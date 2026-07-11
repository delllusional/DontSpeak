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
    /// it. The CALLER (ds-stt) gates this on `is_terminal_frontmost()` so the key never
    /// leaks outside a terminal. An unsupported chord is logged + skipped by the impl.
    fn tap_key(&self, _chord: &KeyChord) {}

    /// Inject `text` into the focused app (§C.3) — used by the local STT engines
    /// (Parakeet) to deliver a transcript. macOS prefers a clipboard-paste
    /// (arboard set + synth Cmd+V) over per-character Unicode events.
    ///
    /// DEFAULT no-op so MockPlatform in the engine tests + the Win/Linux stubs
    /// keep compiling unchanged; only the macOS impl overrides it. The CALLER
    /// (ds-stt) gates this on `is_terminal_frontmost()` so a transcript never leaks.
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
/// / Linux Ctrl+Shift+V) ends identically: spawn a thread, wait ~200 ms for the async paste
/// to read what we set, then put back the snapshot (`Some`) or clear what we left (`None`).
/// The 200 ms margin and the restore-vs-clear rule live here once, not in all three ports.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub fn restore_clipboard_after_paste(prev: Option<String>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if let Ok(mut cb) = arboard::Clipboard::new() {
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

    /// The localized name of the frontmost application (e.g. "Ghostty",
    /// "Terminal"), captured on the Caps OFF→ON edge so the dictation confirm
    /// panel can show the paste target ("→ Terminal"). DEFAULT None so the
    /// Win/Linux stubs and the engine-test MockPlatform keep compiling; only the
    /// macOS impl overrides it.
    fn frontmost_app_name(&self) -> Option<String> {
        None
    }

    /// Whether something focused would ACCEPT a paste right now — i.e. an editable
    /// text field / input has keyboard focus (macOS: a system-wide focused AX element
    /// whose value is settable; Windows: the foreground thread has a focus window).
    /// Used by the `paste_focus_check` guard to decide whether a confirm tap pastes
    /// or instead flashes "nothing to paste into" and keeps the transcript.
    ///
    /// DEFAULT `true` so the Linux stub + the engine-test `MockPlatform` behave
    /// exactly as today (the paste always proceeds); only the macOS and Windows
    /// impls override it. Because the guard is opt-in (`paste_focus_check`, default
    /// off) AND a second tap force-pastes regardless, an occasional false negative
    /// here can never trap a transcript.
    fn has_paste_target(&self) -> bool {
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
    /// `extra_custom_text_editors`) — mirrors `set_extra_terminals` but widens
    /// `has_paste_target()`'s custom-drawn-editor exemption instead of
    /// `is_terminal_frontmost()`'s table. Effective on Windows (`CUSTOM_TEXT_EXES`) and
    /// macOS (`CUSTOM_TEXT_BUNDLES`); Linux accepts-and-ignores the call because its
    /// `has_paste_target` is the always-true trait default (nothing to exempt), keeping
    /// the config field uniformly settable regardless of OS. DEFAULT no-op.
    fn set_extra_custom_text_editors(&self, _extra: Vec<String>) {}
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

    /// Take ownership of the physical Caps key: install whatever OS-level suppression
    /// this platform uses (Windows: `WH_KEYBOARD_LL` hook; Linux: XKB `caps:none` remap;
    /// macOS: `hidutil` null remap) so a physical press no longer toggles native
    /// capitals/the LED and drives the engine's tap gesture instead. Idempotent — safe
    /// to call when already owned. Called once at startup if caps dictation starts
    /// enabled, and again on every OFF→ON live toggle (`Engine::set_caps_gate`).
    /// DEFAULT no-op (the test mock).
    fn acquire_caps_key(&self) {}

    /// Release ownership taken by [`acquire_caps_key`](Self::acquire_caps_key),
    /// restoring the key's native OS behavior. Idempotent — safe to call when not
    /// currently owned (including at startup if caps dictation starts disabled).
    /// Called on every ON→OFF live toggle and at final shutdown. Implementations
    /// MUST also discard any already-queued-but-undelivered Caps edges (Windows'
    /// `drain_caps_events` backlog) — otherwise a burst of presses made while
    /// released replays in full the instant the key is re-acquired, desyncing the
    /// tap/double-tap gesture state machine. DEFAULT no-op.
    fn release_caps_key(&self) {}
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
}

/// The single shared source of truth for "which app counts as a terminal" across all
/// three platforms' `is_terminal_frontmost()`. Covers exactly the identifiers
/// previously hand-maintained as three separate flat lists (Windows's old `TERM_EXES`,
/// macOS's old `TERM_BUNDLES`, Linux's old `TERM_WM_CLASSES` — see each platform
/// module's golden-list test, which pins this table against those pre-refactor
/// literals). Adding a new cross-platform terminal with ONE identifier per OS is one
/// row here instead of up to three separate edits. A user can extend this table without a
/// code change via config.toml's `extra_terminals` (see [`FrontmostWindow::set_extra_terminals`]) —
/// unioned in at lookup time on each platform, never merged into this compiled-in slice.
pub const KNOWN_TERMINALS: &[KnownTerminal] = &[
    // ---- cross-platform ---------------------------------------------------------
    KnownTerminal {
        name: "Alacritty",
        windows_exe: Some("alacritty.exe"),
        macos_bundle: None,
        linux_wm_class: Some("alacritty"),
    },
    KnownTerminal {
        name: "Kitty",
        windows_exe: Some("kitty.exe"),
        macos_bundle: None,
        linux_wm_class: Some("kitty"),
    },
    // WezTerm ships a modern canonical identifier and a legacy/alternate one on BOTH
    // Windows and Linux; both rows must stay present.
    KnownTerminal {
        name: "WezTerm",
        windows_exe: Some("wezterm-gui.exe"),
        macos_bundle: None,
        linux_wm_class: Some("org.wezfurlong.wezterm"),
    },
    KnownTerminal {
        name: "WezTerm (legacy exe / bare wm_class)",
        windows_exe: Some("wezterm.exe"),
        macos_bundle: None,
        linux_wm_class: Some("wezterm"),
    },
    // Ghostty: one macOS bundle id + two Linux wm_class spellings.
    KnownTerminal {
        name: "Ghostty",
        windows_exe: None,
        macos_bundle: Some("com.mitchellh.ghostty"),
        linux_wm_class: Some("com.mitchellh.ghostty"),
    },
    KnownTerminal {
        name: "Ghostty (bare wm_class)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("ghostty"),
    },
    // ---- Windows-only -------------------------------------------------------------
    KnownTerminal {
        name: "Windows Terminal",
        windows_exe: Some("windowsterminal.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Windows Terminal's console host",
        windows_exe: Some("openconsole.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "classic console host",
        windows_exe: Some("conhost.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Windows PowerShell 5.1",
        windows_exe: Some("powershell.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "PowerShell 7+",
        windows_exe: Some("pwsh.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Command Prompt",
        windows_exe: Some("cmd.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Hyper",
        windows_exe: Some("hyper.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Git Bash / MSYS2 (mintty)",
        windows_exe: Some("mintty.exe"),
        macos_bundle: None,
        linux_wm_class: None,
    },
    // ---- macOS-only -----------------------------------------------------------
    KnownTerminal {
        name: "iTerm2",
        windows_exe: None,
        macos_bundle: Some("com.googlecode.iterm2"),
        linux_wm_class: None,
    },
    KnownTerminal {
        name: "Terminal.app",
        windows_exe: None,
        macos_bundle: Some("com.apple.Terminal"),
        linux_wm_class: None,
    },
    // ---- Linux-only -----------------------------------------------------------
    KnownTerminal {
        name: "GNOME Terminal (VTE)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("gnome-terminal-server"),
    },
    KnownTerminal {
        name: "Konsole (KDE)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("konsole"),
    },
    KnownTerminal {
        name: "xterm",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("xterm"),
    },
    KnownTerminal {
        name: "uxterm",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("uxterm"),
    },
    KnownTerminal {
        name: "urxvt",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("urxvt"),
    },
    KnownTerminal {
        name: "rxvt",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("rxvt"),
    },
    KnownTerminal {
        name: "Terminator",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("terminator"),
    },
    KnownTerminal {
        name: "Tilix",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("tilix"),
    },
    KnownTerminal {
        name: "Xfce Terminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("xfce4-terminal"),
    },
    KnownTerminal {
        name: "QTerminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("qterminal"),
    },
    KnownTerminal {
        name: "LXTerminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("lxterminal"),
    },
    KnownTerminal {
        name: "MATE Terminal",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("mate-terminal"),
    },
    KnownTerminal {
        name: "st (suckless)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("st"),
    },
    KnownTerminal {
        name: "foot",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("foot"),
    },
    KnownTerminal {
        name: "foot (client mode)",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("footclient"),
    },
    KnownTerminal {
        name: "Terminology",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("terminology"),
    },
    KnownTerminal {
        name: "Guake",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("guake"),
    },
    KnownTerminal {
        name: "Tilda",
        windows_exe: None,
        macos_bundle: None,
        linux_wm_class: Some("tilda"),
    },
];

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
