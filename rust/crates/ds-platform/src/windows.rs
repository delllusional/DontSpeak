//! Windows platform impl — written behind cfg(target_os="windows").
//! UNCOMPILED on the macOS build host; types/APIs not yet exercised.
//!
//! * Caps lock state: `GetKeyState(VK_CAPITAL) & 0x0001` (low bit = toggle/LED
//!   state, exactly the lock state the engine polls).
//! * Dictation key: `SendInput` presses the chord (modifiers + base key) then
//!   releases — one discrete tap that toggles recording.
//! * Frontmost: `GetForegroundWindow` + `GetWindowThreadProcessId`, then resolve
//!   the process image name and match a terminal list (WindowsTerminal.exe,
//!   conhost.exe, powershell.exe, pwsh.exe, cmd.exe, alacritty.exe, ...).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    SendInput, VIRTUAL_KEY, VK_CAPITAL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::PWSTR;

use crate::{
    CapsEdge, CapsKeyMonitor, CapsLockReader, FrontmostWindow, KeyBase, KeyChord, KeyInjector,
    Platform, PreflightError,
};

const VK_RETURN: u16 = 0x0D; // Enter/Return — the auto-submit keystroke

/// Map a [`KeyBase`] to its Windows virtual-key code. `None` for `Unsupported`.
fn vk_for_base(base: &KeyBase) -> Option<u16> {
    Some(match base {
        KeyBase::Space => 0x20,  // VK_SPACE
        KeyBase::Enter => 0x0D,  // VK_RETURN
        KeyBase::Tab => 0x09,    // VK_TAB
        KeyBase::Escape => 0x1B, // VK_ESCAPE
        // VK_A..VK_Z == 0x41..0x5A, contiguous.
        KeyBase::Letter(c) => 0x41 + (c.to_ascii_uppercase() as u16 - b'A' as u16),
        KeyBase::Unsupported(_) => return None,
    })
}

#[cfg(test)]
mod keycode_parity {
    use super::*;
    use crate::chord::all_supported_bases;

    #[test]
    fn every_supported_base_maps_to_a_keycode() {
        for b in all_supported_bases() {
            assert!(
                vk_for_base(&b).is_some(),
                "Windows vk_for_base has no VK for {b:?}"
            );
        }
    }

    #[test]
    fn unsupported_base_has_no_keycode() {
        assert!(vk_for_base(&KeyBase::Unsupported("f5".into())).is_none());
    }
}

/// Process image base-names (lowercased, no path) that count as "a terminal is
/// frontmost" for the dictation-key / transcript-injection focus gate. The foreground
/// window belongs to the terminal HOST (Windows Terminal, the console host, or a
/// third-party emulator), so those are what `GetForegroundWindow` resolves to; the
/// shell exes are included for the rare case the window is attributed to them.
const TERM_EXES: &[&str] = &[
    "windowsterminal.exe", // Windows Terminal
    "openconsole.exe",     // Windows Terminal's console host
    "conhost.exe",         // classic console host
    "powershell.exe",      // Windows PowerShell 5.1
    "pwsh.exe",            // PowerShell 7+
    "cmd.exe",
    "alacritty.exe",
    "wezterm-gui.exe",
    "wezterm.exe",
    "hyper.exe",
    "kitty.exe",
    "mintty.exe", // Git Bash / MSYS2
];

// ── Caps-Lock low-level keyboard hook ────────────────────────────────────────
//
// We OWN the Caps key. A `WH_KEYBOARD_LL` hook (installed on a dedicated thread
// with its own message pump — the OS calls a low-level hook on the installing
// thread, which MUST pump) fires on every physical Caps transition and SUPPRESSES
// it (returns 1), so Windows never toggles capitals or the LED. This replaces the
// old 30 ms `GetAsyncKeyState` poll, whose sampling gap silently dropped any tap
// faster than the interval — the cause of "tapping Caps to submit does nothing".
//
// Each transition is latched into `CAPS_DOWN` (the live held state) AND pushed onto
// `CAPS_EDGES` (a lossless queue the engine drains each tick), so a down+up that
// both land inside one tick still replays as a real tap. The callback is trivial
// (set an atomic + push one edge) to stay well under `LowLevelHooksTimeout`. This
// mirrors the macOS CGEventTap that latches `caps_down` — the two ports converge.

/// Live physical-held state of the Caps key, written by the hook callback.
static CAPS_DOWN: AtomicBool = AtomicBool::new(false);
/// Lossless queue of Caps transitions awaiting drain by the engine (oldest first).
static CAPS_EDGES: Mutex<VecDeque<CapsEdge>> = Mutex::new(VecDeque::new());
/// One-shot guard so the hook thread is spawned exactly once per process.
static HOOK_STARTED: AtomicBool = AtomicBool::new(false);

/// The `WH_KEYBOARD_LL` callback. Runs on the dedicated hook thread for EVERY key
/// on the system; we act only on non-injected Caps and pass everything else through.
unsafe extern "system" fn caps_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // HC_ACTION (0) is the only code that carries a key event; anything else MUST be
    // forwarded untouched per the hook contract.
    if code == 0 {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Ignore synthetic events (our own SendInput, other tools) — only real hardware
        // Caps presses drive dictation; injected ones must never feed back in.
        let injected = (kb.flags.0 & LLKHF_INJECTED.0) != 0;
        if !injected && kb.vkCode == VK_CAPITAL.0 as u32 {
            let msg = wparam.0 as u32;
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            // Collapse auto-repeat: a held key streams WM_KEYDOWN — record only the
            // first DOWN (state was up) and the matching UP (state was down).
            let was_down = CAPS_DOWN.load(Ordering::Relaxed);
            if is_down && !was_down {
                CAPS_DOWN.store(true, Ordering::Relaxed);
                push_caps_edge(true);
            } else if is_up && was_down {
                CAPS_DOWN.store(false, Ordering::Relaxed);
                push_caps_edge(false);
            }
            // SUPPRESS: returning non-zero stops the OS from ever toggling caps/LED.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Record one Caps transition, bounding the queue so a never-draining consumer
/// (engine paused) can't grow it unboundedly.
fn push_caps_edge(down: bool) {
    if let Ok(mut q) = CAPS_EDGES.lock() {
        if q.len() >= 256 {
            q.pop_front();
        }
        q.push_back(CapsEdge {
            down,
            at: Instant::now(),
        });
    }
}

/// Spawn the hook thread once. Idempotent; failures are logged, not fatal (dictation
/// simply won't trigger, exactly as before a successful install).
fn ensure_caps_hook() {
    if HOOK_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("caps-ll-hook".into())
        .spawn(|| unsafe {
            let hmod = GetModuleHandleW(None).unwrap_or_default();
            let hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(caps_hook_proc), Some(hmod.into()), 0);
            let Ok(_hook) = hook else {
                eprintln!(
                    "dontspeak: SetWindowsHookExW(WH_KEYBOARD_LL) failed — Caps dictation disabled"
                );
                return;
            };
            // A low-level hook is delivered to THIS thread; keep a live message pump or
            // the callback is never invoked. No key messages dispatch here — the pump
            // only services the hook delivery (and any WM_QUIT to tear down).
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    if spawned.is_err() {
        // Couldn't spawn — allow a later retry rather than latching the guard on.
        HOOK_STARTED.store(false, Ordering::SeqCst);
        eprintln!("dontspeak: failed to spawn Caps hook thread");
    }
}

pub struct WindowsPlatform;

impl WindowsPlatform {
    pub fn new() -> Result<Self, PreflightError> {
        ensure_caps_hook();
        Ok(WindowsPlatform)
    }

    fn key(vk: u16, up: bool) -> INPUT {
        let flags = if up {
            KEYEVENTF_KEYUP
        } else {
            KEYBD_EVENT_FLAGS(0)
        };
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) {
        unsafe {
            SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }
}

impl CapsLockReader for WindowsPlatform {
    fn read(&self) -> Option<bool> {
        // Low-order bit = toggle (lock) state.
        let s = unsafe { GetKeyState(VK_CAPITAL.0 as i32) };
        Some((s & 0x0001) != 0)
    }

    /// The LATCHED Caps-Lock toggle/LED state: `GetKeyState(VK_CAPITAL) & 0x0001`
    /// (low bit = toggle/lock state). The OS latches this bit, so a tap is observed
    /// on the next poll even if no momentary key-down was seen — the edge signal the
    /// engine's full-mirror tick follows. UNCOMPILED on the macOS build host;
    /// verify at port time.
    fn caps_lock_on(&self) -> bool {
        let s = unsafe { GetKeyState(VK_CAPITAL.0 as i32) };
        (s & 0x0001) != 0
    }
}

impl KeyInjector for WindowsPlatform {
    // Native SendInput, ours — no library. The caller (ds-stt) gates these on
    // `terminal_frontmost()`. UNCOMPILED on the macOS host; verify on a Windows host.

    /// Tap the dictation chord once: press modifiers, press+release the base key, release
    /// modifiers (reverse). One discrete keypress = one toggle of Claude Code's voice TAP.
    fn tap_key(&self, chord: &KeyChord) {
        let Some(vk) = vk_for_base(&chord.base) else {
            crate::warn_unsupported_dictation_key(&chord.base);
            return;
        };
        // VK_CONTROL=0x11, VK_SHIFT=0x10, VK_MENU(Alt)=0x12, VK_LWIN=0x5B.
        let mods: &[(bool, u16)] = &[
            (chord.ctrl, 0x11),
            (chord.shift, 0x10),
            (chord.alt, 0x12),
            (chord.cmd, 0x5B),
        ];
        let mut seq = Vec::new();
        for &(on, m) in mods {
            if on {
                seq.push(Self::key(m, false));
            }
        }
        seq.push(Self::key(vk, false));
        seq.push(Self::key(vk, true));
        for &(on, m) in mods.iter().rev() {
            if on {
                seq.push(Self::key(m, true));
            }
        }
        Self::send(&seq);
    }

    fn type_text(&self, text: &str) {
        // Deliver the transcript via clipboard + Ctrl+V (mirrors the macOS Cmd+V paste):
        // ONE atomic paste, instant even for a long transcript. The old per-character
        // KEYEVENTF_UNICODE SendInput crawled — a console ingests synthetic unicode
        // keystrokes one at a time, so a multi-word transcript took visibly long to land.
        if text.is_empty() {
            return;
        }
        let Ok(mut cb) = arboard::Clipboard::new() else {
            return;
        };
        // Snapshot the user's clipboard text to RESTORE after the paste (None ⇒ non-text/
        // empty: clear what we put there rather than restore).
        let prev = cb.get_text().ok();
        if cb.set_text(text.to_string()).is_err() {
            return;
        }
        // Ctrl+V (VK_CONTROL=0x11, 'V'=0x56) — the universal Windows paste, also the
        // default paste in modern terminals (Windows Terminal / conhost).
        Self::send(&[
            Self::key(0x11, false),
            Self::key(0x56, false),
            Self::key(0x56, true),
            Self::key(0x11, true),
        ]);
        // Restore the user's clipboard off-thread once the async paste has read ours.
        crate::restore_clipboard_after_paste(prev);
    }

    fn press_enter(&self) {
        Self::send(&[Self::key(VK_RETURN, false), Self::key(VK_RETURN, true)]);
    }
}

impl FrontmostWindow for WindowsPlatform {
    fn terminal_frontmost(&self) -> bool {
        // GetForegroundWindow -> GetWindowThreadProcessId -> OpenProcess(LIMITED) ->
        // QueryFullProcessImageNameW -> match the basename against TERM_EXES. The
        // Parakeet STT engine gates transcript injection on this, so it FAILS
        // CLOSED: any failure (no foreground window, OpenProcess denied, query
        // fails) returns false and nothing is injected.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return false;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return false;
            }
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            // QueryFullProcessImageNameW writes a full path into the buffer and
            // updates `size` to the length actually written.
            let mut buf = [0u16; 260]; // MAX_PATH
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            if !ok {
                return false;
            }
            let path = String::from_utf16_lossy(&buf[..size as usize]);
            let base = path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(&path)
                .to_ascii_lowercase();
            TERM_EXES.contains(&base.as_str())
        }
    }

    fn paste_target_present(&self) -> bool {
        // Mirror the macOS AX probe (`focused_element_accepts_paste`): a paste target
        // exists when the FOCUSED element is an editable text control. The Windows
        // analogue of Accessibility is UI Automation — the focused element accepts a
        // paste when its ControlType is Edit or Document (the text-input roles, like
        // AXTextField/AXTextArea), OR it exposes a non-read-only Value pattern (the
        // settable-`AXValue` fallback that widens coverage).
        //
        // Conservative like macOS: a determinable-but-non-editable focus reads as "no
        // target" (false), so the dictation glow warns. The ONE deviation: if UI
        // Automation can't be created AT ALL (an infrastructure failure, not a focus
        // determination), fail OPEN (true) so a broken probe never nags continuously.
        //
        // The IUIAutomation object is created once per (engine poll) thread and cached;
        // GetFocusedElement is a cross-process UIA call, so this only runs while the
        // dictation panel is up (see the caller's `recording || awaiting_confirm` gate).
        //
        // A TERMINAL always accepts a paste, but its focused element reports as a
        // console/custom control with no Edit/Document type and no Value pattern, so the
        // UIA probe below would wrongly read "no target" (the orange glow). Treat a
        // terminal-frontmost window as a valid target up front — reusing terminal_frontmost,
        // and matching macOS where a terminal's AXTextArea reads as editable.
        if self.terminal_frontmost() {
            return true;
        }
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        };
        use windows::Win32::UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, UIA_DocumentControlTypeId,
            UIA_EditControlTypeId, UIA_ValuePatternId,
        };
        thread_local! {
            static UIA: std::cell::RefCell<Option<IUIAutomation>> =
                std::cell::RefCell::new(None);
        }
        UIA.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                unsafe {
                    // Best-effort COM init for THIS thread as MTA — harmless (S_FALSE) if
                    // already initialized; UI Automation works in either apartment.
                    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                    match CoCreateInstance::<_, IUIAutomation>(
                        &CUIAutomation,
                        None,
                        CLSCTX_INPROC_SERVER,
                    ) {
                        Ok(a) => *slot = Some(a),
                        Err(_) => return true, // can't probe at all → fail OPEN (no nagging)
                    }
                }
            }
            let automation = slot.as_ref().unwrap();
            unsafe {
                // No focus / unreadable focus ⇒ no paste target (macOS parity).
                let Ok(el) = automation.GetFocusedElement() else {
                    return false;
                };
                // Primary: an Edit or Document control type (the text-input roles).
                if let Ok(ct) = el.CurrentControlType() {
                    if ct == UIA_EditControlTypeId || ct == UIA_DocumentControlTypeId {
                        return true;
                    }
                }
                // Fallback: a non-read-only Value pattern (editable contents) — catches
                // editable elements that report a non-standard control type.
                if let Ok(vp) =
                    el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                {
                    if let Ok(read_only) = vp.CurrentIsReadOnly() {
                        if !read_only.as_bool() {
                            return true;
                        }
                    }
                }
                false
            }
        })
    }
}

// ── Caps-Lock LED indicator (dictation "recording" light) ────────────────────
//
// The engine drives the Caps LED as a pure dictation indicator (`set_caps_lock`
// at start/stop). On the key-owning Windows port the physical key is suppressed,
// so we light the LED out-of-band via the keyboard class driver's
// `IOCTL_KEYBOARD_SET_INDICATORS` — the hardware-LED path that does NOT touch
// win32k's logical Caps toggle (no capitals), the Windows analogue of Linux's
// `EV_LED`/`LED_CAPSL` write and macOS's `IOHIDSetModifierLockState`.
mod caps_led {
    use std::ffi::c_void;

    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION, DefineDosDeviceW,
        FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_NUMLOCK, VK_SCROLL};
    use windows::core::PCWSTR;

    // ntddkbd.h indicator bits (not surfaced by the `windows` crate).
    const KEYBOARD_SCROLL_LOCK_ON: u16 = 1;
    const KEYBOARD_NUM_LOCK_ON: u16 = 2;
    const KEYBOARD_CAPS_LOCK_ON: u16 = 4;
    // CTL_CODE(FILE_DEVICE_KEYBOARD=0x0b, function=0x0002, METHOD_BUFFERED=0,
    // FILE_ANY_ACCESS=0) = (0x0b<<16) | (0x0002<<2) = 0x000B_0008.
    const IOCTL_KEYBOARD_SET_INDICATORS: u32 = 0x000B_0008;
    // Class-driver instances to fan out to (KeyboardClass0..). One per physical
    // keyboard; we light the Caps LED on each so the right board responds.
    const MAX_KEYBOARDS: u32 = 8;

    /// `KEYBOARD_INDICATOR_PARAMETERS` (ntddkbd.h): which unit + the absolute LED set.
    #[repr(C)]
    struct KeyboardIndicatorParameters {
        unit_id: u16,
        led_flags: u16,
    }

    /// Assemble the ABSOLUTE indicator bitmask the driver expects: light Caps per
    /// `caps_on` while preserving the live Num/Scroll lock LEDs (the IOCTL replaces
    /// the whole set, so omitting a bit would dark its LED). Pure — unit-tested.
    fn led_flags(caps_on: bool, num_on: bool, scroll_on: bool) -> u16 {
        let mut f = 0u16;
        if scroll_on {
            f |= KEYBOARD_SCROLL_LOCK_ON;
        }
        if num_on {
            f |= KEYBOARD_NUM_LOCK_ON;
        }
        if caps_on {
            f |= KEYBOARD_CAPS_LOCK_ON;
        }
        f
    }

    /// The latched toggle (low) bit of a lock key, via `GetKeyState`.
    fn lock_on(vk: u16) -> bool {
        unsafe { (GetKeyState(vk as i32) & 0x0001) != 0 }
    }

    /// UTF-16, NUL-terminated — for the `PCWSTR` Win32 string args.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Light/clear the Caps-Lock LED as the dictation indicator, preserving the
    /// Num/Scroll LEDs and the logical Caps toggle. Best-effort: every Win32 call is
    /// fallible (no keyboard, access denied) and silently skipped — the indicator is
    /// cosmetic and must never break dictation.
    pub fn drive_caps(caps_on: bool) {
        let flags = led_flags(caps_on, lock_on(VK_NUMLOCK.0), lock_on(VK_SCROLL.0));
        let kip = KeyboardIndicatorParameters {
            unit_id: 0,
            led_flags: flags,
        };
        for idx in 0..MAX_KEYBOARDS {
            // A per-(pid,unit) DOS symlink to the kernel keyboard device — the
            // documented user-mode way to reach \Device\KeyboardClassN. Created in
            // the per-logon namespace (no elevation), then removed below.
            let dos = format!("DontSpeakKbd{}_{}", std::process::id(), idx);
            let dos_w = wide(&dos);
            let target_w = wide(&format!(r"\Device\KeyboardClass{idx}"));
            unsafe {
                if DefineDosDeviceW(
                    DDD_RAW_TARGET_PATH,
                    PCWSTR(dos_w.as_ptr()),
                    PCWSTR(target_w.as_ptr()),
                )
                .is_err()
                {
                    continue;
                }
                let path_w = wide(&format!(r"\\.\{dos}"));
                // Access 0 (no GENERIC_READ/WRITE) + share R/W: METHOD_BUFFERED
                // FILE_ANY_ACCESS IOCTLs need no access right, and on Windows 11 a
                // GENERIC_WRITE open hits a sharing violation against the class driver.
                if let Ok(h) = CreateFileW(
                    PCWSTR(path_w.as_ptr()),
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                ) {
                    if !h.is_invalid() {
                        let _ = DeviceIoControl(
                            h,
                            IOCTL_KEYBOARD_SET_INDICATORS,
                            Some(&kip as *const _ as *const c_void),
                            std::mem::size_of::<KeyboardIndicatorParameters>() as u32,
                            None,
                            0,
                            None,
                            None,
                        );
                        let _ = CloseHandle(h);
                    }
                }
                // Drop the temporary symlink (NULL target = remove all defs for the name).
                let _ = DefineDosDeviceW(
                    DDD_REMOVE_DEFINITION,
                    PCWSTR(dos_w.as_ptr()),
                    PCWSTR::null(),
                );
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn caps_bit_set_only_when_on() {
            // Caps off → no caps bit; caps on → caps bit (value 4).
            assert_eq!(led_flags(false, false, false) & KEYBOARD_CAPS_LOCK_ON, 0);
            assert_eq!(
                led_flags(true, false, false) & KEYBOARD_CAPS_LOCK_ON,
                KEYBOARD_CAPS_LOCK_ON
            );
        }

        #[test]
        fn preserves_num_and_scroll_independently() {
            // Num/Scroll LEDs must survive a caps on/off write, never clobbered.
            assert_eq!(led_flags(false, true, false), KEYBOARD_NUM_LOCK_ON);
            assert_eq!(led_flags(false, false, true), KEYBOARD_SCROLL_LOCK_ON);
            assert_eq!(
                led_flags(true, true, true),
                KEYBOARD_CAPS_LOCK_ON | KEYBOARD_NUM_LOCK_ON | KEYBOARD_SCROLL_LOCK_ON
            );
            // Toggling caps leaves the Num/Scroll bits identical.
            let off = led_flags(false, true, true);
            let on = led_flags(true, true, true);
            assert_eq!(on & !KEYBOARD_CAPS_LOCK_ON, off);
        }

        #[test]
        fn exact_ntddkbd_bit_values() {
            // Guard the hand-copied ntddkbd.h constants against drift.
            assert_eq!(KEYBOARD_SCROLL_LOCK_ON, 1);
            assert_eq!(KEYBOARD_NUM_LOCK_ON, 2);
            assert_eq!(KEYBOARD_CAPS_LOCK_ON, 4);
            assert_eq!(IOCTL_KEYBOARD_SET_INDICATORS, 0x000B_0008);
        }
    }
}

impl CapsKeyMonitor for WindowsPlatform {
    fn caps_physically_down(&self) -> bool {
        // The live held state latched by the low-level hook — event-driven, not
        // polled, so it never misses a transition the way `GetAsyncKeyState` did.
        CAPS_DOWN.load(Ordering::Relaxed)
    }
    fn set_caps_lock(&self, on: bool) {
        // Drive the dictation indicator on the PHYSICAL Caps-Lock LED, matching the
        // macOS (IOHIDSetModifierLockState) and Linux (EV_LED) ports. The low-level
        // hook SUPPRESSES the key, so we can't (and mustn't) toggle the logical lock
        // — that would re-enable capitals. `IOCTL_KEYBOARD_SET_INDICATORS` drives the
        // hardware LED directly, decoupled from win32k's toggle bit, so the light
        // tracks `holding` with no effect on typed case. Num/Scroll LEDs are preserved
        // (the IOCTL writes the FULL indicator set), mirroring the other two ports
        // which only ever touch the Caps bit.
        caps_led::drive_caps(on);
    }
    fn caps_event_driven(&self) -> bool {
        true
    }
    fn drain_caps_events(&self) -> Vec<CapsEdge> {
        match CAPS_EDGES.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl Platform for WindowsPlatform {
    fn preflight(&self) -> Result<(), PreflightError> {
        // No special permission required for SendInput at the same integrity
        // level; UIPI may block elevated targets — documented, not enforced.
        Ok(())
    }
}

// ── Microphone-in-use probe (TTS feedback gate) ──────────────────────────────
//
// Whether the default capture endpoint is being captured RIGHT NOW (the mic is in
// use anywhere on the system). The TTS paths use this to hold/skip playback so
// speech never feeds back into a live recording.

/// Windows: is the default capture endpoint being captured right now? Mirrors the
/// macOS CoreAudio probe — enumerate the audio sessions on the default capture
/// device and report true if ANY session is `AudioSessionStateActive` (some app
/// holds a live capture stream: Claude Code's dictation, our Parakeet STT, or any
/// other recorder). Best-effort: any COM failure returns false (no gate, always
/// play), matching the graceful degrade on platforms without a probe.
pub(crate) fn mic_active() -> bool {
    // Inside `mod windows`, the `windows` extern crate is named normally — the
    // lib.rs-scope `mod windows` shadow that forced a leading `::` does not apply here.
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, IAudioSessionControl, IAudioSessionEnumerator,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };

    unsafe {
        // Init COM on this thread. S_OK/S_FALSE (.is_ok()) ⇒ we own a balancing
        // CoUninitialize; RPC_E_CHANGED_MODE (err) ⇒ COM is already up in another
        // mode — proceed but do NOT uninit (we didn't initialize it).
        let did_init = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();

        let active = (|| -> windows::core::Result<bool> {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let device = enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?;
            let mgr: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let sessions: IAudioSessionEnumerator = mgr.GetSessionEnumerator()?;
            for i in 0..sessions.GetCount()? {
                let ctrl: IAudioSessionControl = sessions.GetSession(i)?;
                if ctrl.GetState()? == AudioSessionStateActive {
                    return Ok(true);
                }
            }
            Ok(false)
        })()
        .unwrap_or(false);

        if did_init {
            CoUninitialize();
        }
        active
    }
}
