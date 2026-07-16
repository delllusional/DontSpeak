//! Platform abstraction for the dontspeak engine.
//!
//! Capability traits: [`KeyInjector`] (dictation key tap), [`FrontmostWindow`] (terminal
//! focus gate), [`CapsKeyMonitor`] (physical Caps edges + LED). [`Platform`] aggregates
//! them plus `preflight()`. [`current()`] / [`is_mic_active()`] dispatch per OS; [`KeyChord`]
//! lives in `chord`. All three ports behind `cfg(target_os=…)`; full matrix in release CI.

use std::error::Error;
use std::fmt;
use std::time::Instant;

mod chord;
pub use chord::{KeyBase, KeyChord};

/// One physical Caps transition. Event-driven ports queue these so a down+up inside
/// one poll gap still replays. `at` is for long-press threshold vs the down edge.
#[derive(Clone, Copy, Debug)]
pub struct CapsEdge {
    /// `true` = DOWN, `false` = UP.
    pub down: bool,
    pub at: Instant,
}

/// Inject Claude Code voice dictation keypresses (TAP toggles recording; key from
/// `voice:pushToTalk`, default Space).
pub trait KeyInjector {
    /// One discrete tap (down+up). Default no-op. Caller (ds-stt) gates on
    /// `is_terminal_frontmost()`. Unsupported chords: log + skip.
    fn tap_key(&self, _chord: &KeyChord) {}

    /// Inject transcript text into focused app. Default no-op; caller gates frontmost.
    fn type_text(&self, _text: &str) {}

    /// Press Enter once (always-listening submit). Default no-op; caller gates frontmost.
    fn press_enter(&self) {}
}

/// Shared "can't synthesize dictation key" warning (one message for all ports).
pub fn warn_unsupported_dictation_key(base: &KeyBase) {
    eprintln!(
        "dontspeak: can't synthesize claude_code dictation key {base:?} — bind voice:pushToTalk to Space or a Ctrl+<letter>"
    );
}

/// Restore clipboard after paste, off the caller thread. Wait for async paste, then put
/// back snapshot (`Some`) or clear (`None`) only if clipboard still holds our transcript.
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

/// Focus gate: only inject dictation/transcript while a terminal is frontmost.
pub trait FrontmostWindow {
    fn is_terminal_frontmost(&self) -> bool;

    /// Frontmost app name for the confirm panel ("→ Terminal"). Default None.
    fn frontmost_app_name(&self) -> Option<String> {
        None
    }

    /// Best-effort: can focus accept paste? Warn-only, never gates delivery. Default true.
    fn has_paste_target(&self) -> bool {
        true
    }

    /// Replace config.toml `extra_terminals` union (assemble + every reload). Default no-op.
    fn set_extra_terminals(&self, _extra: Vec<String>) {}

    /// Replace `extra_custom_text_editors` for `has_paste_target` exemption (Win/macOS;
    /// Linux no-op — default always-true). Default no-op.
    fn set_extra_custom_text_editors(&self, _extra: Vec<String>) {}
}

/// Physical Caps hold + LED write (long-press + recording indicator).
pub trait CapsKeyMonitor {
    /// Physical hold, independent of LED/toggle. Engine uses for long-press reset.
    fn is_caps_physically_down(&self) -> bool;
    /// Force Caps LED/lock (e.g. long-press drift recovery OFF).
    fn set_caps_lock(&self, on: bool);

    /// macOS-only: IOHID/LED resource stuck denied despite grant (needs process relaunch).
    /// Default false. Engine polls + relaunches; prefer [`caps_monitor_stuck_detail`].
    fn caps_monitor_stuck(&self) -> bool {
        false
    }

    /// Which resource is stuck for the persisted engine log (stderr often invisible).
    fn caps_monitor_stuck_detail(&self) -> Option<&'static str> {
        None
    }

    /// True when Caps edges come from a lossless event stream ([`drain_caps_events`])
    /// rather than sampling [`is_caps_physically_down`]. Windows hook = true; polled
    /// ports default false. Event-driven ports suppress OS Caps toggle but still drive
    /// the LED via `set_caps_lock`.
    fn is_caps_event_driven(&self) -> bool {
        false
    }

    /// Drain Caps edges since last call (oldest first). Meaningful only if event-driven.
    fn drain_caps_events(&self) -> Vec<CapsEdge> {
        Vec::new()
    }

    /// [`acquire_caps_key`] phase 1: suppression that must be live before clearing logical
    /// Caps (macOS hidutil / Windows hook). false → skip normalize+finish. Default ready.
    fn begin_caps_key_acquisition(&self) -> bool {
        true
    }

    /// Phase 2: clear pre-existing logical Caps, indicator off. Default `set_caps_lock(false)`.
    fn normalize_caps_lock(&self) {
        self.set_caps_lock(false);
    }

    /// Phase 3: post-normalize suppression (Linux XKB `caps:none`). Default no-op.
    fn finish_caps_key_acquisition(&self) {}

    /// Release ownership; restore native Caps. Idempotent. MUST discard queued Caps edges
    /// (else re-acquire replays them). Default no-op.
    fn release_caps_key(&self) {}
}

/// Shared Caps ownership: begin → normalize → finish. Only acquisition entry point.
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

/// Full OS capability set.
pub trait Platform: KeyInjector + FrontmostWindow + CapsKeyMonitor {
    /// Silent, repeatable permission check (re-probe safe; never prompt).
    fn preflight(&self) -> Result<(), PreflightError>;

    /// One-shot permission prompt at startup only (macOS Accessibility). Default no-op.
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

// Native KeyInjector + CapsKeyMonitor per OS (no shared input library).

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOsPlatform;

// Mic-in-use watcher (dispatches to `is_mic_active()`).
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

/// Platform for this build target.
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

/// One row in the shared terminal table for `is_terminal_frontmost()`. `name` is
/// debug-only. Platform fields are `Some` only where that OS has an id; dual ids on
/// one OS (WezTerm, Ghostty) use two rows.
pub struct KnownTerminal {
    pub name: &'static str,
    pub windows_exe: Option<&'static str>,
    pub macos_bundle: Option<&'static str>,
    pub linux_wm_class: Option<&'static str>,
}

/// Single source for terminal ids (was three per-OS lists). Extend via config.toml
/// `extra_terminals` ([`FrontmostWindow::set_extra_terminals`]), not this slice.
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

// Mic-in-use probe: TTS holds/skips while any app records (no Claude Code signal).
// macOS CoreAudio / Windows WASAPI; Linux → false (always play).

/// Default mic currently capturing?
#[cfg(target_os = "macos")]
pub fn is_mic_active() -> bool {
    macos::is_mic_active()
}

#[cfg(windows)]
pub fn is_mic_active() -> bool {
    windows::is_mic_active()
}

/// No mic probe: never gate TTS.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn is_mic_active() -> bool {
    false
}

/// Detach inherited/auto console (Windows only; see `windows::detach_console`).
#[cfg(windows)]
pub fn detach_console() {
    windows::detach_console();
}

#[cfg(not(windows))]
pub fn detach_console() {}
