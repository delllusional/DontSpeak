//! Platform abstraction for the dontspeak engine.
//!
//! Four capability traits split the OS-specific surface the engine needs:
//!   * [`CapsLockReader`]  — poll the native Caps Lock TOGGLE/lock state.
//!   * [`KeyInjector`]     — synthesize the dictation key tap (down+up) that toggles recording.
//!   * [`FrontmostWindow`] — is a terminal the frontmost app? (focus gate)
//!   * [`CapsKeyMonitor`]  — physical Caps key down-state (§F long-press) + the
//!     LED-off write (drift-recovery force-reset).
//!
//! [`Platform`] aggregates all four plus a one-time `preflight()` (permission
//! check). The free functions [`current()`] (the platform impl for the build
//! target) and [`mic_active()`] (system mic-in-use probe) dispatch to the per-OS
//! modules. The OS-independent [`KeyChord`]/[`KeyBase`] keybinding parser lives in
//! the `chord` module.
//!
//! Compile status:
//! - macOS: implemented & compile-verified on the Apple-Silicon build host
//!   (IOKit lock-state FFI, core-graphics CGEventPost, NSWorkspace).
//! - Windows: written behind cfg(target_os="windows"); UNCOMPILED here.
//! - Linux: written behind cfg(target_os="linux"); UNCOMPILED here.

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

/// Reads the live Caps Lock lock-state (the LED/toggle, NOT a key-down event).
pub trait CapsLockReader {
    /// `Some(true)` if caps is ON, `Some(false)` if OFF, `None` on a transient
    /// read failure (the engine skips that tick rather than guessing).
    fn read(&self) -> Option<bool>;

    /// The LATCHED Caps-Lock LED / lock state (true == ON), distinct from the
    /// momentary `CapsKeyMonitor::caps_physically_down`. The OS latches this bit,
    /// so even a tap too fast to be observed as a momentary key-down is reflected
    /// on the NEXT poll — this is the edge signal the "full mirror" engine tick
    /// follows (OFF→ON starts recording, ON→OFF stops). Distinct from `read()`,
    /// which on macOS returns the physical-key down state (HOLD semantics), not
    /// the latched lock.
    fn caps_lock_on(&self) -> bool;
}

/// Injects the keypress that drives Claude Code voice dictation: TAP — one keypress
/// toggles recording on, the next toggles it off. The key is whatever Claude Code's
/// `voice:pushToTalk` is bound to (default `Space`), read from its config.
pub trait KeyInjector {
    /// Synthesize ONE discrete key tap (down then up) for `chord`. DEFAULT no-op so the
    /// Win/Linux stubs + minimal test fakes compile unchanged; the macOS impl overrides
    /// it. The CALLER (ds-stt) gates this on `terminal_frontmost()` so the key never
    /// leaks outside a terminal. An unsupported chord is logged + skipped by the impl.
    fn tap_key(&self, _chord: &KeyChord) {}

    /// Inject `text` into the focused app (§C.3) — used by the local STT engines
    /// (Parakeet) to deliver a transcript. macOS prefers a clipboard-paste
    /// (arboard set + synth Cmd+V) over per-character Unicode events.
    ///
    /// DEFAULT no-op so MockPlatform in the engine tests + the Win/Linux stubs
    /// keep compiling unchanged; only the macOS impl overrides it. The CALLER
    /// (ds-stt) gates this on `terminal_frontmost()` so a transcript never leaks.
    fn type_text(&self, _text: &str) {}

    /// Press Return/Enter once (key down+up, no modifiers) — used by the
    /// always-listening loop to SUBMIT the prompt after the stopword fires.
    /// DEFAULT no-op (Win/Linux stubs + MockPlatform); the macOS impl overrides
    /// it. The CALLER gates this on `terminal_frontmost()`.
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
    fn terminal_frontmost(&self) -> bool;

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
    fn paste_target_present(&self) -> bool {
        true
    }
}

/// Physical Caps-Lock key down-duration + LED-off write — the NEW signal §F
/// needs for long-press detection, which the lock-state poll (`CapsLockReader`)
/// cannot observe (toggling the lock flips the bit instantly regardless of how
/// long you hold the key).
pub trait CapsKeyMonitor {
    /// Whether the Caps Lock key is physically held *right now*, independent of
    /// the LED/toggle state. The engine stamps the first true and fires a reset
    /// if it stays true past `long_press_ms`.
    fn caps_physically_down(&self) -> bool;
    /// Force the Caps Lock LED/lock state (the drift-recovery write used by the
    /// long-press reset to drive the LED OFF, `set_caps_lock(false)`).
    fn set_caps_lock(&self, on: bool);

    /// Whether this platform delivers Caps transitions as a lossless EVENT STREAM
    /// (drained via [`drain_caps_events`](Self::drain_caps_events)) rather than the
    /// engine sampling [`caps_physically_down`](Self::caps_physically_down) once per
    /// tick. Windows' low-level hook returns `true`; the polled platforms (macOS,
    /// Linux) and the test mock keep the DEFAULT `false`, so the engine drives them
    /// off the sampled boolean exactly as before. An event-driven platform fully
    /// SUPPRESSES the key (no OS caps TOGGLE, so no capitals), but `set_caps_lock`
    /// still drives the physical LED out-of-band as the dictation indicator — on
    /// Windows via `IOCTL_KEYBOARD_SET_INDICATORS`, matching the polled ports.
    fn caps_event_driven(&self) -> bool {
        false
    }

    /// Drain every Caps transition observed since the last call, oldest first. Only
    /// meaningful when [`caps_event_driven`](Self::caps_event_driven) is `true`; the
    /// DEFAULT returns empty so polled platforms and the mock are untouched.
    fn drain_caps_events(&self) -> Vec<CapsEdge> {
        Vec::new()
    }
}

/// One platform's full capability set.
pub trait Platform: CapsLockReader + KeyInjector + FrontmostWindow + CapsKeyMonitor {
    /// One-time startup check (e.g. macOS Accessibility trust). Returns an
    /// error the engine prints before exiting non-zero.
    fn preflight(&self) -> Result<(), PreflightError>;
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
// Key synthesis (`KeyInjector`) + Caps-Lock LED (`CapsLockReader`/`CapsKeyMonitor`) are
// implemented NATIVELY per OS below — one correct, self-maintained impl each, no library.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacPlatform;

// Cross-platform mic-in-use watcher (push interface; CoreAudio listener on macOS, poll
// thread elsewhere). Lives above the per-OS modules so it can dispatch to `mic_active()`.
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
pub fn current() -> Result<MacPlatform, PreflightError> {
    MacPlatform::new()
}

#[cfg(target_os = "windows")]
pub fn current() -> Result<WindowsPlatform, PreflightError> {
    WindowsPlatform::new()
}

#[cfg(target_os = "linux")]
pub fn current() -> Result<LinuxPlatform, PreflightError> {
    LinuxPlatform::new()
}

/// The terminal bundle/identifier set used by the focus gate (macOS bundle ids
/// here; Windows/Linux impls keep their own equivalent lists).
pub const TERM_BUNDLES: &[&str] = &[
    "com.googlecode.iterm2",
    "com.apple.Terminal",
    "com.mitchellh.ghostty",
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
// input device (mirrors the old `mic-active.swift` helper). Other platforms have
// no probe yet → `false` (no gate), which degrades to today's always-play. The
// Windows WASAPI probe is implemented during the Windows port.

/// Returns true if the system's default microphone is currently capturing.
///
/// Thin dispatch to the per-OS probe (CoreAudio on macOS, WASAPI on Windows, a
/// no-gate fallback elsewhere); the implementation for each target lives in that
/// OS's module.
#[cfg(target_os = "macos")]
pub fn mic_active() -> bool {
    macos::mic_active()
}

/// Returns true if the system's default microphone is currently capturing.
#[cfg(windows)]
pub fn mic_active() -> bool {
    windows::mic_active()
}

/// Stub for platforms with no mic probe yet (Linux): never gate TTS (always play).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn mic_active() -> bool {
    false
}
