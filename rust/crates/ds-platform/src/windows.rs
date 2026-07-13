//! Windows platform impl — behind cfg(target_os="windows"). Built + tested on
//! Windows in CI (the release full matrix).
//!
//! * Caps LED: `set_caps_lock` drives the physical Caps light out-of-band via
//!   `IOCTL_KEYBOARD_SET_INDICATORS` — a pure recording-state output. The logical
//!   toggle is read only while the shared key-acquisition sequence normalizes startup.
//! * Dictation key: `SendInput` presses the chord (modifiers + base key) then
//!   releases — one discrete tap that toggles recording.
//! * Frontmost: `GetForegroundWindow` + `GetWindowThreadProcessId`, then resolve
//!   the process image name and match a terminal list (WindowsTerminal.exe,
//!   conhost.exe, powershell.exe, pwsh.exe, cmd.exe, alacritty.exe, ...).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    SendInput, VIRTUAL_KEY, VK_CAPITAL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
    HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::PWSTR;

use crate::{
    CapsEdge, CapsKeyMonitor, FrontmostWindow, KeyBase, KeyChord, KeyInjector, Platform,
    PreflightError,
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

/// Is `exe` (a lowercased Windows process basename) one of the shared table's known
/// terminal identifiers (`ds_platform::KNOWN_TERMINALS`), OR one of the user's
/// config.toml `extra_terminals` entries? Replaces the old hand-maintained `TERM_EXES`
/// array. `extra` entries are matched case-insensitively (a user may type any casing),
/// unlike the built-in table's byte-exact literals.
fn is_known_terminal_exe(exe: &str, extra: &[String]) -> bool {
    crate::KNOWN_TERMINALS
        .iter()
        .any(|t| t.windows_exe == Some(exe))
        || extra.iter().any(|e| e.eq_ignore_ascii_case(exe))
}

/// Process image base-names (lowercased, no path) for editors that render their main text
/// surface with a custom GPU/canvas toolkit rather than a native Win32 edit control, so UI
/// Automation exposes no Edit/Document role (and no settable Value pattern) on the focused
/// buffer even though a synthetic paste + Enter lands in it fine. Same underlying cause as
/// the terminal exemption in `has_paste_target()` below (a custom-drawn text view with no
/// AX/UIA text pattern) — kept as a SEPARATE list rather than folded into the shared
/// `KNOWN_TERMINALS` terminal table, because `is_terminal_frontmost()` also gates
/// unrelated behavior (mic pause-in-background in `ttsq.rs`, dictation-key/transcript
/// leak prevention in `ds-stt`) that a code editor should not opt into just because its
/// buffer view happens to be UIA-invisible.
///
/// - "zed.exe": Zed's GPUI framework draws the buffer itself; its Windows UI Automation
///   support is still partial (accessibility is an explicitly ongoing project — see
///   zed-industries/zed discussion #6576) and doesn't yet expose the buffer as Edit/Document.
///
/// A user can extend this table without a code change via config.toml's
/// `extra_custom_text_editors` (see [`crate::FrontmostWindow::set_extra_custom_text_editors`])
/// — unioned in at lookup time by [`is_custom_text_exe`], never merged into this slice.
const CUSTOM_TEXT_EXES: &[&str] = &["zed.exe"];

/// Is `exe` one of the built-in [`CUSTOM_TEXT_EXES`], OR one of the user's config.toml
/// `extra_custom_text_editors` entries? Case-insensitive for the user-supplied extras
/// only (the built-in table's exact-match behavior for its own literals is untouched).
fn is_custom_text_exe(exe: &str, extra: &[String]) -> bool {
    CUSTOM_TEXT_EXES.contains(&exe) || extra.iter().any(|e| e.eq_ignore_ascii_case(exe))
}

/// `GetForegroundWindow` -> `GetWindowThreadProcessId` -> `OpenProcess(LIMITED)` ->
/// `QueryFullProcessImageNameW` -> the lowercased basename, or `None` on any failure (no
/// foreground window, access denied, query failure). Shared by `is_terminal_frontmost()`
/// and the `has_paste_target()` custom-editor exemption so the raw Win32 FFI to resolve
/// "what process owns the foreground window" isn't duplicated between them.
fn frontmost_process_basename() -> Option<String> {
    // SAFETY: Win32 FFI with locally owned buffers — `pid`/`buf`/`size` are stack locals
    // that outlive the calls that write them, `handle` comes from a successful
    // `OpenProcess` and is closed exactly once below, and the `buf[..size]` slice uses the
    // length `QueryFullProcessImageNameW` reported.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return None;
        };
        // QueryFullProcessImageNameW writes a full path into the buffer and updates `size`
        // to the length actually written.
        // QueryFullProcessImageNameW supports extended-length paths. A MAX_PATH-sized
        // buffer incorrectly rejects otherwise valid foreground processes installed
        // under a long path.
        let mut buf = vec![0u16; 32_768];
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
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        Some(
            path.rsplit(['\\', '/'])
                .next()
                .unwrap_or(&path)
                .to_ascii_lowercase(),
        )
    }
}

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
/// OS thread id of the dedicated hook-pump thread (`GetCurrentThreadId`), published right
/// after `SetWindowsHookExW` succeeds. Lets [`shutdown_caps_hook`] `PostThreadMessage`
/// `WM_QUIT` to that specific thread from anywhere else, unblocking its `GetMessageW`
/// loop so it can unhook and exit. 0 = no pump thread currently installed (never started,
/// spawn/`SetWindowsHookExW` failed, or already shut down).
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// Installed hook handle, published independently of its pump thread so shutdown can
/// forcibly uninstall it if that thread fails to process WM_QUIT before the deadline.
/// Stored as an integer because the generated `HHOOK` raw pointer is not `Sync`.
static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);
/// The pump thread's `JoinHandle`, so [`shutdown_caps_hook`] can WAIT for the unhook to
/// actually finish (confirms `UnhookWindowsHookEx` ran) instead of firing `WM_QUIT` and
/// hoping. `None` until [`ensure_caps_hook`] spawns the thread.
static HOOK_JOIN: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// The `WH_KEYBOARD_LL` callback. Runs on the dedicated hook thread for EVERY key
/// on the system; we act only on non-injected Caps and pass everything else through.
unsafe extern "system" fn caps_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // HC_ACTION (0) is the only code that carries a key event; anything else MUST be
    // forwarded untouched per the hook contract.
    if code == 0 {
        // SAFETY: for HC_ACTION (code == 0, checked above) the OS passes a valid
        // KBDLLHOOKSTRUCT pointer in lparam, alive for the duration of this callback
        // (the WH_KEYBOARD_LL contract); we only read from it.
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
    // SAFETY: forwards the exact code/wparam/lparam the OS handed this callback, as the
    // hook-chain contract requires for events we don't consume; no pointers of ours cross.
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

/// Spawn the hook thread once and wait until `SetWindowsHookExW` has either installed
/// the suppression hook or failed. The readiness barrier closes the acquisition race:
/// the shared normalization phase may inspect and clear logical Caps only after physical
/// presses are guaranteed to be suppressed. Idempotent; failures are logged, not fatal.
fn ensure_caps_hook() -> bool {
    if HOOK_STARTED.swap(true, Ordering::SeqCst) {
        return HOOK_HANDLE.load(Ordering::SeqCst) != 0;
    }
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("caps-ll-hook".into())
        // SAFETY: plain Win32 FFI on this dedicated thread — `caps_hook_proc` is a `fn`
        // item (it can never dangle), `msg` is a live stack local for every
        // GetMessageW/DispatchMessageW call, and UnhookWindowsHookEx gets the handle
        // SetWindowsHookExW just returned, on the same thread that installed it.
        .spawn(move || unsafe {
            let hmod = GetModuleHandleW(None).unwrap_or_default();
            let hook =
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(caps_hook_proc), Some(hmod.into()), 0);
            let Ok(hook) = hook else {
                eprintln!(
                    "dontspeak: SetWindowsHookExW(WH_KEYBOARD_LL) failed — Caps dictation disabled"
                );
                HOOK_STARTED.store(false, Ordering::SeqCst);
                let _ = ready_tx.send(false);
                return;
            };
            // Publish this thread's id so `shutdown_caps_hook` — called from the engine's
            // teardown path, on a DIFFERENT thread — can post it WM_QUIT to unblock the
            // pump below and tear the hook down.
            let tid = GetCurrentThreadId();
            HOOK_HANDLE.store(hook.0 as isize, Ordering::SeqCst);
            HOOK_THREAD_ID.store(tid, Ordering::SeqCst);
            // If the caller timed out and dropped the receiver, do not leave a hook it
            // believes failed to start. Unhook here, on the installing thread, and let a
            // later acquisition retry from a clean global state.
            if ready_tx.send(true).is_err() {
                if HOOK_HANDLE
                    .compare_exchange(hook.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    let _ = UnhookWindowsHookEx(hook);
                }
                let _ = HOOK_THREAD_ID.compare_exchange(tid, 0, Ordering::SeqCst, Ordering::SeqCst);
                HOOK_STARTED.store(false, Ordering::SeqCst);
                return;
            }
            // A low-level hook is delivered to THIS thread; keep a live message pump or
            // the callback is never invoked. No key messages dispatch here — the pump
            // only services the hook delivery and the WM_QUIT `shutdown_caps_hook` posts
            // to tear it down.
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // WM_QUIT received — unhook on THIS thread (the one that installed it, per
            // the documented UnhookWindowsHookEx/SetWindowsHookExW pairing), then clear
            // the published id so a stale value is never posted to after we're gone.
            if HOOK_HANDLE
                .compare_exchange(hook.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let _ = UnhookWindowsHookEx(hook);
            }
            let _ = HOOK_THREAD_ID.compare_exchange(tid, 0, Ordering::SeqCst, Ordering::SeqCst);
        });
    match spawned {
        Ok(handle) => {
            *HOOK_JOIN.lock().unwrap() = Some(handle);
            const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
            match ready_rx.recv_timeout(START_TIMEOUT) {
                Ok(true) => true,
                Ok(false) => {
                    shutdown_caps_hook();
                    false
                }
                Err(error) => {
                    // Dropping the receiver makes a late-starting thread take its
                    // `send(true).is_err()` cleanup branch above. `shutdown_caps_hook`
                    // also joins (or forcibly unhooks) within its own bounded timeout.
                    drop(ready_rx);
                    eprintln!(
                        "dontspeak: Caps hook did not become ready within {START_TIMEOUT:?} ({error})"
                    );
                    shutdown_caps_hook();
                    false
                }
            }
        }
        Err(_) => {
            // Couldn't spawn — allow a later retry rather than latching the guard on.
            HOOK_STARTED.store(false, Ordering::SeqCst);
            eprintln!("dontspeak: failed to spawn Caps hook thread");
            false
        }
    }
}

/// Tear down the `WH_KEYBOARD_LL` hook + its pump thread: post `WM_QUIT` to the thread
/// that installed the hook (unblocking its `GetMessageW` loop, which then calls
/// `UnhookWindowsHookEx` on itself before exiting — see [`ensure_caps_hook`]), then wait
/// (bounded — see below) for the unhook to be CONFIRMED complete before returning — not
/// just requested. Also clears the latched Caps state (`CAPS_DOWN` / `CAPS_EDGES`) so a
/// later [`ensure_caps_hook`] starts clean — this is what makes `release_caps_key` +
/// the shared `acquire_caps_key` safe to call on every live `caps_enabled` toggle, not just at
/// process start/exit: a burst of presses made while released never survives to
/// replay against the fresh hook — and resets `HOOK_STARTED` so the NEXT
/// acquisition reinstalls a fresh hook rather than silently staying
/// uninstalled (the bug this closes: without this reset, a stop+start cycle never
/// reinstalled the hook — or worse, before this fix, `ensure_caps_hook`'s guard let a
/// SECOND hook thread stack on top of a still-live first one, since nothing ever called
/// this to release the first). Idempotent: a no-op when no hook is installed
/// (`HOOK_THREAD_ID` reads 0 — never started, spawn/`SetWindowsHookExW` failed, or
/// already shut down).
///
/// Bounded wait, NOT an untimed `join()`: `release_caps_key` (hence this) now runs from
/// `Engine::set_caps_gate` on every live OFF toggle — the engine's single poll thread,
/// not just process/engine `Drop` as before — so an untimed join risks freezing that
/// thread's tick/gesture timing if the pump thread is ever slow to service its
/// `GetMessageW` loop. Mirrors `dontspeakd::helper_stt::HelperStt::abort`'s identical
/// concern for the same poll thread: poll `is_finished()` against a deadline instead of
/// blocking on `join()`, and detach (leave it running, or wedged, on its own) if the
/// deadline passes rather than hang the caller either way.
fn shutdown_caps_hook() {
    let tid = HOOK_THREAD_ID.load(Ordering::SeqCst);
    if tid != 0 {
        // SAFETY: PostThreadMessageW passes no pointers (both message params are 0); a
        // stale or already-exited `tid` just makes the call fail, which we ignore.
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(handle) = HOOK_JOIN.lock().unwrap().take() {
        const JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
        let deadline = std::time::Instant::now() + JOIN_TIMEOUT;
        let mut joined = false;
        while std::time::Instant::now() < deadline {
            if handle.is_finished() {
                let _ = handle.join();
                joined = true;
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        if !joined {
            let raw = HOOK_HANDLE.swap(0, Ordering::SeqCst);
            if raw != 0 {
                // SAFETY: `raw` is the live handle published immediately after a
                // successful SetWindowsHookExW call. Atomic swap transfers responsibility
                // for the one unhook to this thread; the pump's compare_exchange then
                // prevents it from unhooking the same handle again.
                unsafe {
                    let _ = UnhookWindowsHookEx(HHOOK(raw as *mut std::ffi::c_void));
                }
            }
            eprintln!(
                "dontspeak: shutdown_caps_hook gave up waiting {JOIN_TIMEOUT:?} for the hook \
                 pump thread; forcibly unhooked and detached it instead of blocking"
            );
        }
    }
    CAPS_DOWN.store(false, Ordering::Relaxed);
    if let Ok(mut q) = CAPS_EDGES.lock() {
        q.clear();
    }
    HOOK_STARTED.store(false, Ordering::SeqCst);
}

pub struct WindowsPlatform {
    /// User config.toml `extra_terminals` — extends `KNOWN_TERMINALS` at lookup time.
    /// `RefCell`, not another global `static`: unlike the Caps hook state above (owned by
    /// a free `unsafe extern "system" fn` callback with no `self`), this is only ever read/
    /// written through ordinary `&self` trait methods on the engine's single poll thread
    /// (via `Rc<WindowsPlatform>`, which is `!Send`), so plain interior mutability suffices.
    extra_terminals: RefCell<Vec<String>>,
    /// User config.toml `extra_custom_text_editors` — extends `CUSTOM_TEXT_EXES` at lookup
    /// time. Same single-thread reasoning as `extra_terminals` above.
    extra_custom_text_editors: RefCell<Vec<String>>,
}

impl WindowsPlatform {
    /// Does NOT acquire the Caps key — the engine calls `ds_platform::acquire_caps_key`
    /// right after construction, only if caps dictation starts enabled (see
    /// `Engine::assemble`), so a `caps_enabled=false` startup never installs the hook.
    pub fn new() -> Result<Self, PreflightError> {
        Ok(WindowsPlatform {
            extra_terminals: RefCell::new(Vec::new()),
            extra_custom_text_editors: RefCell::new(Vec::new()),
        })
    }

    /// Release every platform resource this port opened OUTSIDE its own struct's
    /// lifetime: the systemwide `WH_KEYBOARD_LL` hook + its dedicated pump thread (see
    /// `shutdown_caps_hook`), and the physical Caps-Lock LED (forced off so a
    /// mid-dictation stop never leaves it lit for the rest of the process's life).
    /// `ds_engine_stop()` only joins the engine's own thread and clears its `running`
    /// flag — nothing else in the shutdown path tore any of this down, so a
    /// `ds_engine_stop()` + `ds_engine_start()` cycle without a process exit left Caps
    /// suppressed permanently (the still-live hook thread keeps eating every physical
    /// tap) with the LED possibly stuck lit and unrecoverable by the user (the hook also
    /// blocks a real Caps press from toggling it back off).
    ///
    /// Called unconditionally via this struct's `Drop` impl below, which — since `plat` is an
    /// `Rc<WindowsPlatform>` local to `engine_run`'s stack (see `dontspeakd::boot::engine_run`)
    /// — fires the moment the engine thread's function returns, i.e. exactly when
    /// `ds_engine_stop()`'s `t.join()` unblocks.
    pub fn shutdown(&self) {
        // Physical LED off first — cheap, and correct even if the hook teardown below
        // has to wait on the pump thread's message loop to drain.
        self.set_caps_lock(false);
        shutdown_caps_hook();
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
        // SAFETY: `inputs` is a live, fully initialized slice for the duration of the
        // call, and cbSize is the true size of INPUT, as SendInput requires.
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            eprintln!(
                "dontspeak: SendInput inserted {sent} of {} keyboard events",
                inputs.len()
            );
        }
    }

    fn logical_caps_lock_on() -> bool {
        // SAFETY: GetKeyState takes no pointers. For toggle keys its low bit is the
        // logical ON/OFF state; unlike the physical LED, that is the state which changes
        // typed case and must be cleared before DontSpeak owns the key.
        lock_state_is_on(unsafe { GetKeyState(VK_CAPITAL.0 as i32) })
    }
}

fn lock_state_is_on(state: i16) -> bool {
    (state & 0x0001) != 0
}

impl Drop for WindowsPlatform {
    fn drop(&mut self) {
        // Mirrors `MacOsPlatform`/`LinuxPlatform`'s Drop impls: release everything `Self::new`
        // opened outside this struct's own fields. `plat` is an `Rc<WindowsPlatform>` local to
        // `engine_run`'s stack (see `dontspeakd::boot::engine_run`), so this fires
        // automatically the moment the engine thread's function returns — no explicit
        // shutdown call site needed. See `Self::shutdown`'s doc comment for what this
        // undoes and why (the un-hooked `WH_KEYBOARD_LL` hook + a possibly-stuck Caps LED).
        self.shutdown();
    }
}

impl KeyInjector for WindowsPlatform {
    // Native SendInput, ours — no library. The caller (ds-stt) gates these on
    // `is_terminal_frontmost()`.

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
        crate::restore_clipboard_after_paste(prev, text.to_owned());
    }

    fn press_enter(&self) {
        Self::send(&[Self::key(VK_RETURN, false), Self::key(VK_RETURN, true)]);
    }
}

impl FrontmostWindow for WindowsPlatform {
    fn is_terminal_frontmost(&self) -> bool {
        // Match the frontmost process's basename against the shared KNOWN_TERMINALS
        // table. The Parakeet STT engine gates transcript injection on this, so it
        // FAILS CLOSED: any failure to resolve the frontmost process (see
        // `frontmost_process_basename`) returns false and nothing is injected.
        frontmost_process_basename()
            .is_some_and(|base| is_known_terminal_exe(&base, &self.extra_terminals.borrow()))
    }

    fn has_paste_target(&self) -> bool {
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
        // UIA probe below would wrongly read "no target" (the orange glow); same story for
        // a frontmost CUSTOM_TEXT_EXES editor (its buffer view isn't UIA-visible either).
        // Treat either as a valid target up front, resolving the frontmost process ONCE
        // (rather than calling `is_terminal_frontmost()`, which would re-resolve it) —
        // matching macOS where a terminal's AXTextArea reads as editable.
        if frontmost_process_basename().is_some_and(|base| {
            is_known_terminal_exe(&base, &self.extra_terminals.borrow())
                || is_custom_text_exe(&base, &self.extra_custom_text_editors.borrow())
        }) {
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
                const { std::cell::RefCell::new(None) };
        }
        UIA.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                // SAFETY: COM FFI with no caller pointers — CoInitializeEx runs on this
                // thread before CoCreateInstance, and the created IUIAutomation is an
                // owned, refcounted wrapper kept in a thread-local (never crosses
                // threads).
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
            // SAFETY: UIA calls on the thread-local IUIAutomation created above (COM is
            // initialized on this thread); every result is an owned `windows`-crate
            // wrapper checked through Result — no raw pointers escape this block.
            unsafe {
                // No focus / unreadable focus ⇒ no paste target (macOS parity).
                let Ok(el) = automation.GetFocusedElement() else {
                    return false;
                };
                // Primary: an Edit or Document control type (the text-input roles).
                if let Ok(ct) = el.CurrentControlType()
                    && (ct == UIA_EditControlTypeId || ct == UIA_DocumentControlTypeId)
                {
                    return true;
                }
                // Fallback: a non-read-only Value pattern (editable contents) — catches
                // editable elements that report a non-standard control type.
                if let Ok(vp) =
                    el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    && let Ok(read_only) = vp.CurrentIsReadOnly()
                    && !read_only.as_bool()
                {
                    return true;
                }
                false
            }
        })
    }

    fn set_extra_terminals(&self, extra: Vec<String>) {
        *self.extra_terminals.borrow_mut() = extra;
    }

    fn set_extra_custom_text_editors(&self, extra: Vec<String>) {
        *self.extra_custom_text_editors.borrow_mut() = extra;
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
        QueryDosDeviceW,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_NUMLOCK, VK_SCROLL};
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
    static CLEAN_STALE_LINKS: std::sync::Once = std::sync::Once::new();

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
        // SAFETY: GetKeyState takes no pointers and merely returns a state word; any
        // virtual-key value is acceptable input.
        super::lock_state_is_on(unsafe { super::GetKeyState(vk as i32) })
    }

    /// UTF-16, NUL-terminated — for the `PCWSTR` Win32 string args.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn cleanup_stale_links() {
        CLEAN_STALE_LINKS.call_once(|| {
            let mut names = vec![0u16; 64 * 1024];
            // SAFETY: a null device name requests the NUL-separated device-name list;
            // `names` is a live writable slice for the duration of the call.
            let written = unsafe { QueryDosDeviceW(PCWSTR::null(), Some(&mut names)) } as usize;
            if written == 0 || written > names.len() {
                return;
            }
            for raw in names[..written]
                .split(|&c| c == 0)
                .filter(|s| !s.is_empty())
            {
                let name = String::from_utf16_lossy(raw);
                if !name.starts_with("DontSpeakKbd") {
                    continue;
                }
                let name_w = wide(&name);
                // SAFETY: name_w is NUL-terminated and alive across the call. Removing
                // all definitions for this private prefix cleans links left by a process
                // crash; the engine's single-instance guard prevents a live peer.
                unsafe {
                    let _ = DefineDosDeviceW(
                        DDD_REMOVE_DEFINITION,
                        PCWSTR(name_w.as_ptr()),
                        PCWSTR::null(),
                    );
                }
            }
        });
    }

    /// Light/clear the Caps-Lock LED as the dictation indicator, preserving the
    /// Num/Scroll LEDs and the logical Caps toggle. Best-effort: every Win32 call is
    /// fallible (no keyboard, access denied) and silently skipped — the indicator is
    /// cosmetic and must never break dictation.
    pub fn drive_caps(caps_on: bool) {
        cleanup_stale_links();
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
            // SAFETY: every pointer arg is a NUL-terminated wide buffer (`dos_w`/
            // `target_w`/`path_w`) or the stack `kip` struct (with its exact size
            // passed), each alive across the one call that reads it; `h` is used only
            // after CreateFileW returns Ok and non-invalid, and is closed exactly once.
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
                ) && !h.is_invalid()
                {
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
    fn is_caps_physically_down(&self) -> bool {
        // The live held state latched by the low-level hook — event-driven, not
        // polled, so it never misses a transition the way `GetAsyncKeyState` did.
        CAPS_DOWN.load(Ordering::Relaxed)
    }
    fn set_caps_lock(&self, on: bool) {
        // Drive the dictation indicator on the PHYSICAL Caps-Lock LED, matching the
        // macOS (IOHIDSetModifierLockState) and Linux (EV_LED) ports. The low-level
        // hook SUPPRESSES the key during steady-state gesture handling. Logical Caps is
        // normalized separately, once per acquisition; this writer remains deliberately
        // decoupled from win32k's toggle bit so the light tracks `holding` with no effect
        // on typed case. Num/Scroll LEDs are preserved
        // (the IOCTL writes the FULL indicator set), mirroring the other two ports
        // which only ever touch the Caps bit.
        caps_led::drive_caps(on);
    }
    fn is_caps_event_driven(&self) -> bool {
        true
    }
    fn drain_caps_events(&self) -> Vec<CapsEdge> {
        match CAPS_EDGES.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
    fn begin_caps_key_acquisition(&self) -> bool {
        ensure_caps_hook()
    }
    fn normalize_caps_lock(&self) {
        // The hook is ready before the shared acquisition sequence reaches this phase,
        // so a physical Caps press cannot race the state read. Injected events bypass
        // our hook by design (`LLKHF_INJECTED`) and therefore clear the Windows logical
        // toggle without becoming a DontSpeak gesture.
        if Self::logical_caps_lock_on() {
            Self::send(&[
                Self::key(VK_CAPITAL.0, false),
                Self::key(VK_CAPITAL.0, true),
            ]);
        }
        self.set_caps_lock(false);
    }
    fn release_caps_key(&self) {
        // Already clears CAPS_EDGES (see its doc) — no backlog survives for the next
        // acquire to replay in a burst.
        shutdown_caps_hook();
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
pub(crate) fn is_mic_active() -> bool {
    // Inside `mod windows`, the `windows` extern crate is named normally — the
    // lib.rs-scope `mod windows` shadow that forced a leading `::` does not apply here.
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, IAudioSessionControl, IAudioSessionEnumerator,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    };

    // SAFETY: COM FFI confined to this thread — CoInitializeEx runs first; every
    // interface is an owned `windows`-crate wrapper dropped inside the closure before
    // CoUninitialize, which only balances an init that returned S_OK/S_FALSE
    // (`did_init`).
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

/// Detach this process from whatever console it inherited or was implicitly given.
///
/// `dontspeak.exe` must be a console-subsystem binary so `dontspeak <client>` can block
/// an interactive shell for the launched TUI (a GUI-subsystem process returns control to
/// PowerShell/cmd immediately instead of waiting). But most of its roles (`notify`,
/// `provide`, the bare stdio MCP server) are spawned by a GUI host — Claude Code Desktop,
/// the WinUI app — that has no console of its own; Windows then allocates a new,
/// momentarily visible console window for a console-subsystem child unless it detaches.
/// Piped stdio (always used for these roles) is unaffected: those are independent OS
/// handles, not tied to console attachment.
pub(crate) fn detach_console() {
    use windows::Win32::System::Console::FreeConsole;

    // SAFETY: FreeConsole takes no arguments and has no preconditions; if this process
    // has no console attached the call just fails harmlessly, which we ignore.
    let _ = unsafe { FreeConsole() };
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

#[cfg(test)]
mod custom_text_exes {
    use super::*;

    #[test]
    fn zed_is_listed_and_lowercase() {
        // `frontmost_process_basename()` always lowercases before matching, so an
        // uppercase entry here would silently never match.
        assert!(CUSTOM_TEXT_EXES.contains(&"zed.exe"));
        for exe in CUSTOM_TEXT_EXES {
            assert_eq!(*exe, exe.to_ascii_lowercase(), "{exe} must be lowercase");
        }
    }

    #[test]
    fn disjoint_from_term_exes() {
        // The two lists gate different behavior (see CUSTOM_TEXT_EXES's doc comment);
        // an exe in both would be redundant and signals it belongs in one, not both.
        for exe in CUSTOM_TEXT_EXES {
            assert!(
                !is_known_terminal_exe(exe, &[]),
                "{exe} listed in both exe tables"
            );
        }
    }
}

#[cfg(test)]
mod extra_paste_targets {
    use super::*;

    #[test]
    fn is_known_terminal_exe_matches_extra_case_insensitively() {
        let extra = vec!["myterm.exe".to_string()];
        assert!(is_known_terminal_exe("myterm.exe", &extra));
        assert!(is_known_terminal_exe("MYTERM.EXE", &extra));
        assert!(!is_known_terminal_exe("otherterm.exe", &extra));
        // Empty extra behaves exactly as before the signature change (regression guard).
        assert!(is_known_terminal_exe("cmd.exe", &[]));
        assert!(!is_known_terminal_exe("notaterm.exe", &[]));
    }

    #[test]
    fn is_custom_text_exe_matches_extra_and_does_not_cross_contaminate() {
        let editor_extra = vec!["myeditor.exe".to_string()];
        assert!(is_custom_text_exe("myeditor.exe", &editor_extra));
        assert!(is_custom_text_exe("MYEDITOR.EXE", &editor_extra));
        assert!(!is_custom_text_exe("othereditor.exe", &editor_extra));
        // The two extra lists are independent slices, not shared state: an entry in
        // `extra_terminals` doesn't widen `is_custom_text_exe`'s match, and vice versa.
        let term_extra = vec!["myterm.exe".to_string()];
        assert!(!is_custom_text_exe("myterm.exe", &editor_extra));
        assert!(!is_known_terminal_exe("myeditor.exe", &term_extra));
    }
}

#[cfg(test)]
mod known_terminal_table {
    /// The exact literal `TERM_EXES` this crate carried before `KNOWN_TERMINALS`
    /// (ds-platform/src/lib.rs) replaced it — pinned here so a future edit to the
    /// shared table can't silently drop (or duplicate away) a Windows entry.
    const OLD_TERM_EXES: &[&str] = &[
        "windowsterminal.exe",
        "openconsole.exe",
        "conhost.exe",
        "powershell.exe",
        "pwsh.exe",
        "cmd.exe",
        "alacritty.exe",
        "wezterm-gui.exe",
        "wezterm.exe",
        "hyper.exe",
        "kitty.exe",
        "mintty.exe",
    ];

    #[test]
    fn matches_old_term_exes_exactly() {
        let entries: Vec<&str> = crate::KNOWN_TERMINALS
            .iter()
            .filter_map(|t| t.windows_exe)
            .collect();
        let derived: std::collections::BTreeSet<&str> = entries.iter().copied().collect();
        let old: std::collections::BTreeSet<&str> = OLD_TERM_EXES.iter().copied().collect();
        assert_eq!(
            derived, old,
            "KNOWN_TERMINALS' windows_exe entries drifted from the pre-refactor TERM_EXES list"
        );
        assert_eq!(
            entries.len(),
            derived.len(),
            "a windows_exe value is duplicated across two KNOWN_TERMINALS rows"
        );
    }
}

#[cfg(test)]
mod caps_key_ownership {
    use super::*;

    #[test]
    fn logical_toggle_uses_only_the_get_key_state_low_bit() {
        assert!(!lock_state_is_on(0));
        assert!(lock_state_is_on(1));
        assert!(!lock_state_is_on(0x8000_u16 as i16));
        assert!(lock_state_is_on(0x8001_u16 as i16));
    }

    /// The regression this closes: while `caps_enabled` was OFF, a still-installed hook
    /// would keep queuing every physical press into `CAPS_EDGES` even though nothing
    /// drains it — so the moment ownership is re-acquired, the whole backlog would
    /// replay in one burst and desync the tap/double-tap gesture state machine (see
    /// `Engine::set_caps_gate`). `release_caps_key` must leave no backlog for that
    /// replay to draw from — verified directly against `CAPS_EDGES`, independent of
    /// whether a real `WH_KEYBOARD_LL` hook happens to be installed (this crate's tests
    /// never install one for real — see `shutdown_caps_hook`'s idempotent no-hook path).
    #[test]
    fn release_caps_key_drops_any_queued_backlog() {
        {
            let mut q = CAPS_EDGES.lock().unwrap();
            q.push_back(CapsEdge {
                down: true,
                at: std::time::Instant::now(),
            });
            q.push_back(CapsEdge {
                down: false,
                at: std::time::Instant::now(),
            });
        }
        let plat = WindowsPlatform::new().unwrap();
        plat.release_caps_key();
        assert!(
            plat.drain_caps_events().is_empty(),
            "release_caps_key must discard any events queued before it ran"
        );
    }
}
