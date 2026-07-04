//! Physical Caps-key monitor via `IOHIDManager` (the robust path).
//!
//! Why this exists: on this machine (macOS 26, external/Bluetooth keyboard) BOTH
//! lock-state reads are blind to the Caps key:
//!   * `IOHIDGetModifierLockState` poll (`iokit::CapsReader::read`) — never sees
//!     a toggle (a 15 s probe toggling caps saw 0 changes).
//!   * A CGEvent `FlagsChanged` AlphaShift tap — lock-coupled and unreliable; a
//!     hold starting from lock-ON is invisible.
//!
//! `IOHIDManager` reads the PHYSICAL key value straight off the device's HID
//! input reports (usage page 0x07 `kHIDPage_KeyboardOrKeypad`, usage 0x39
//! `kHIDUsage_KeyboardCapsLock`; value 1 = down, 0 = up). It bypasses the
//! virtual-HID layer entirely, so it is immune to the macOS 26 built-in→virtual
//! HID regression.
//!
//! PERMISSION: only **Accessibility** is required — confirmed both empirically and
//! by Apple's own dev-forum guidance: an app already trusted for Accessibility is
//! automatically permitted to listen to input, i.e. **Accessibility SUBSUMES Input
//! Monitoring** for `IOHIDManagerOpen`. The engine already holds the Accessibility
//! grant for CGEventPost injection, so `IOHIDManagerOpen` succeeds with NO separate
//! Input Monitoring grant or row — which is why the app tracks only Accessibility
//! and does not surface a distinct Input Monitoring permission. IF YOU LAND HERE
//! LATER because caps HOLD is dead: do NOT go looking for a separate Input
//! Monitoring toggle in System Settings. DontSpeak has never needed one, it isn't
//! listed there, and adding one would not fix anything (re-confirmed 2026-07-04
//! chasing exactly this dead end before finding the real cause below).
//!
//! THE ACTUAL GOTCHA (found 2026-07-04): a grant made WHILE this process is
//! already running does not retroactively unstick `IOHIDManagerOpen` in that SAME
//! process — even though `AXIsProcessTrusted()` (`iokit::ax_is_process_trusted`)
//! correctly flips live and the AX-gated caps-TOGGLE loop re-probes and turns on
//! immediately (see `engine::refresh_caps_gate`). Building a brand-new
//! `IOHIDManager` each retry (see `spawn_caps_hid_monitor`'s doc) does NOT pick up
//! that fresh grant either — the denial is apparently cached per-process at a
//! layer `AXIsProcessTrusted` doesn't see. Only a fresh process (full quit +
//! relaunch) clears it. Rather than leave caps HOLD silently dead until the user
//! notices and manually restarts, the retry loop below detects this exact shape
//! (denied while AX is ALREADY trusted, repeatedly) and latches it via
//! [`is_caps_hid_stuck`]; the engine (`boot::engine_run`) polls that next to its own
//! AX re-probe and relaunches the whole process itself when it's set. If
//! `IOHIDManagerOpen` ever returns `kIOReturnNotPermitted` (0xE00002E2) and caps
//! HOLD never arms, THIS self-relaunch is the mechanism to check, not Input
//! Monitoring.
//!
//! Symbols come from the IOKit framework (linked in build.rs); the CFRunLoop
//! symbols come from CoreFoundation (also linked in build.rs).

use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::stuck_grant::StuckGrantLatch;

// IOKit / CoreFoundation opaque + scalar typedefs.
type IoHidManagerRef = *mut c_void;
type IoHidValueRef = *mut c_void;
type IoHidElementRef = *mut c_void;
type IoReturn = c_int;
type IoOptionBits = c_uint;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFStringRef = *const c_void;
type CFIndex = isize;

const KERN_SUCCESS: IoReturn = 0; // kIOReturnSuccess
const KIO_HID_OPTIONS_TYPE_NONE: IoOptionBits = 0;

// HID usage page / usage for the physical Caps Lock key.
const K_HID_PAGE_KEYBOARD: u32 = 0x07; // kHIDPage_KeyboardOrKeypad
const K_HID_USAGE_CAPSLOCK: u32 = 0x39; // kHIDUsage_KeyboardCapsLock
// F18 — the dst `capskey::own_caps_key` remapped Caps Lock to BEFORE the switch to the
// null "No Action" dst (F18 leaked `ESC[32~` into focused terminals). We read the raw
// device element (usage 0x39, below the system remap), so the dst never reaches us;
// F18 stays watched as free insurance for a stale F18 remap left by a hard-killed older
// build. Both usages drive the SAME caps-held state; no real F18 key exists on the
// built-in keyboard, so this can't double-fire. See `capskey.rs`.
const K_HID_USAGE_F18: u32 = 0x6D; // kHIDUsage_KeyboardF18

/// `IOHIDValueCallback` — C function pointer the manager invokes per input value.
type IoHidValueCallback = extern "C" fn(
    context: *mut c_void,
    result: IoReturn,
    sender: *mut c_void,
    value: IoHidValueRef,
);

unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: IoOptionBits) -> IoHidManagerRef;
    // Passing a NULL matching dictionary matches ALL devices; we filter to the
    // Caps usage inside the callback, which avoids building a CFDictionary.
    fn IOHIDManagerSetDeviceMatching(manager: IoHidManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerRegisterInputValueCallback(
        manager: IoHidManagerRef,
        callback: IoHidValueCallback,
        context: *mut c_void,
    );
    fn IOHIDManagerScheduleWithRunLoop(
        manager: IoHidManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    // Detach a denied manager from the run loop before releasing it (recreate-on-
    // retry teardown — see `spawn_caps_hid_monitor`).
    fn IOHIDManagerUnscheduleFromRunLoop(
        manager: IoHidManagerRef,
        run_loop: CFRunLoopRef,
        run_loop_mode: CFStringRef,
    );
    fn IOHIDManagerOpen(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    // Pairs with `IOHIDManagerOpen` on a clean shutdown (see `stop_caps_hid_monitor`) —
    // the retry-teardown path above never opened its discarded managers, so it never
    // needed this; a manager the run loop actually served input from does.
    fn IOHIDManagerClose(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    // CoreFoundation release for the manager we discard on each failed retry.
    fn CFRelease(cf: *const c_void);

    fn IOHIDValueGetElement(value: IoHidValueRef) -> IoHidElementRef;
    fn IOHIDValueGetIntegerValue(value: IoHidValueRef) -> CFIndex;
    fn IOHIDElementGetUsagePage(element: IoHidElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IoHidElementRef) -> u32;

    // CoreFoundation run-loop plumbing for the dedicated monitor thread.
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
    // Unblocks a `CFRunLoopRun()` call in progress on ANY thread — documented safe to
    // call from a thread other than the one running the loop, which is exactly how
    // `stop_caps_hid_monitor` uses it (called from whichever thread drives shutdown).
    fn CFRunLoopStop(rl: CFRunLoopRef);
    static kCFRunLoopDefaultMode: CFStringRef;
}

/// Input-value callback: filter to the Caps key and publish its down/up state
/// into the shared `AtomicBool`. `context` is a leaked `Arc<AtomicBool>` raw
/// pointer (the monitor thread runs forever, so the leak is intentional — never
/// reconstruct the Arc here, that would drop it).
extern "C" fn caps_value_callback(
    context: *mut c_void,
    _result: IoReturn,
    _sender: *mut c_void,
    value: IoHidValueRef,
) {
    if context.is_null() || value.is_null() {
        return;
    }
    unsafe {
        let element = IOHIDValueGetElement(value);
        if element.is_null() {
            return;
        }
        let usage = IOHIDElementGetUsage(element);
        if IOHIDElementGetUsagePage(element) == K_HID_PAGE_KEYBOARD
            && (usage == K_HID_USAGE_CAPSLOCK || usage == K_HID_USAGE_F18)
        {
            let down = IOHIDValueGetIntegerValue(value) != 0;
            // Borrow, don't take ownership: context outlives this call.
            let caps_down = &*(context as *const AtomicBool);
            caps_down.store(down, Ordering::Relaxed);
        }
    }
}

/// How long the monitor waits between `IOHIDManagerOpen` retries while it's still
/// being denied (Accessibility not yet granted). Matches the engine's AX re-probe
/// cadence so the key source and the caps gate (green dot) arm in the same beat.
/// The SAME constant `led.rs`'s `RetryingCapsLed` uses (via `stuck_grant`, the
/// shared home for both — this is about the stuck-grant bug class, not anything
/// HID-monitor-specific), not a duplicate declaration.
use super::stuck_grant::RETRY_INTERVAL as HID_OPEN_RETRY;
use super::stuck_grant::STUCK_RETRIES_BEFORE_RELAUNCH;

/// The monitor thread's `CFRunLoopRef`, published once it starts (before the retry loop,
/// since `CFRunLoopGetCurrent` is valid immediately) so [`stop_caps_hid_monitor`] can
/// `CFRunLoopStop` it from another thread. Stored as `usize` — `CFRunLoopRef` is a raw
/// `*mut c_void` and isn't `Sync`, but `CFRunLoopStop` is documented safe to call, given a
/// valid ref, from any thread. 0 = no monitor thread currently running.
static MONITOR_RUN_LOOP: AtomicUsize = AtomicUsize::new(0);
/// One-shot "tear down and exit" signal, checked at the top of the retry loop (so a stop
/// requested while still waiting on an ungranted Accessibility permission — which retries
/// forever otherwise — exits within one [`HID_OPEN_RETRY`] tick instead of never) and
/// raced against just before the blocking `CFRunLoopRun()` call (so a stop that lands in
/// the narrow window between publishing the run loop and entering it is still observed).
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
/// The monitor thread's `JoinHandle`, so [`stop_caps_hid_monitor`] can wait for the
/// teardown (manager close/release, leaked `Arc` drop) to actually finish before
/// returning, not just fire the stop signal and hope.
static MONITOR_JOIN: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// Latched once the retry loop has seen [`STUCK_RETRIES_BEFORE_RELAUNCH`]
/// consecutive denials while Accessibility was ALREADY trusted — this process's
/// grant is stale and will never self-heal in place (see the module doc's "ACTUAL
/// GOTCHA"). A `static` (see [`StuckGrantLatch`]'s doc for why), not a
/// thread-local in the retry loop: `dontspeakd`'s engine can stop+restart the
/// whole in-process engine (config reload, RPC-driven restart) WITHOUT the OS
/// process exiting, which respawns this monitor thread fresh each time — a
/// per-instance counter would reset on each of those and could let denials
/// spread across two restarts never cross the threshold. Read by
/// [`is_caps_hid_stuck`], which `MacOsPlatform::caps_monitor_stuck` exposes to the
/// engine's poll loop as the signal to relaunch.
static STUCK_GRANT: StuckGrantLatch = StuckGrantLatch::new(STUCK_RETRIES_BEFORE_RELAUNCH);

/// Whether this process's caps-HID monitor is confirmed stuck (denied despite an
/// already-trusted Accessibility grant) — see [`STUCK_GRANT`].
pub fn is_caps_hid_stuck() -> bool {
    STUCK_GRANT.is_stuck()
}

/// Spawn the dedicated `IOHIDManager` run-loop thread that publishes the PHYSICAL
/// Caps-key down state into `caps_down` (true = held). Replaces the lock-coupled
/// CGEvent AlphaShift tap as the HOLD signal source.
///
/// RETRY, then RELAUNCH: on a fresh install the Accessibility grant lands AFTER
/// launch, so the first `IOHIDManagerOpen` returns `kIOReturnNotPermitted`. We
/// build a brand-new manager and retry every `HID_OPEN_RETRY` — which DOES pick up
/// a grant made before this thread's first attempt. What it can NOT do (see the
/// module doc's "ACTUAL GOTCHA") is pick up a grant made WHILE already stuck
/// retrying: if AX trust flips true and the open keeps failing anyway for
/// `STUCK_RETRIES_BEFORE_RELAUNCH` more attempts, [`STUCK_GRANT`] latches true and the
/// engine relaunches the whole process — the only thing that actually clears it.
/// Until either happens the engine still runs as a pure RPC host (`caps_down`
/// stays false). A `manager`-create failure is the only unrecoverable case.
pub fn spawn_caps_hid_monitor(caps_down: Arc<AtomicBool>) {
    // A prior `stop_caps_hid_monitor()` call may have left this set — clear it
    // before starting fresh so the new monitor thread doesn't see a stale stop and
    // exit immediately (this is the ds_engine_stop() + ds_engine_start() restart
    // path). Deliberately NOT resetting `STUCK_GRANT` here — see its doc: it must
    // survive exactly this kind of in-process restart to accumulate denials
    // correctly across it.
    SHOULD_STOP.store(false, Ordering::SeqCst);
    let handle = std::thread::Builder::new()
        .name("ds-caps-hid".into())
        .spawn(move || unsafe {
            let run_loop = CFRunLoopGetCurrent();
            // Published immediately (before the retry loop, which can otherwise spin
            // for a while waiting on Accessibility) so `stop_caps_hid_monitor` can
            // always find this thread's run loop to `CFRunLoopStop`.
            MONITOR_RUN_LOOP.store(run_loop as usize, Ordering::SeqCst);
            // Leak ONE Arc clone as the callback context, reused across retry
            // attempts; the manager that finally opens owns it until shutdown (see
            // the teardown below, which un-leaks it via `Arc::from_raw`). The
            // discarded managers never fire the callback, so handing them the same
            // pointer is safe — CFRelease frees the manager, not ctx.
            let ctx = Arc::into_raw(caps_down) as *mut c_void;
            let mut warned = false;

            // RECREATE-on-retry: a manager whose open was denied does NOT pick up a
            // later grant (the denial sticks to that instance), so we build a FRESH
            // manager each attempt until one opens — which arms caps HOLD live IF the
            // grant landed before this thread's first attempt. It does NOT arm live if
            // the grant instead lands WHILE already stuck retrying — see the module
            // doc's "ACTUAL GOTCHA" and [`STUCK_GRANT`] for that case, which needs a real
            // process restart. Until either happens the engine still runs as a pure
            // RPC host (`caps_down` stays false). Warn ONCE so a long-untrusted run
            // doesn't spam the log every 2 s.
            loop {
                if SHOULD_STOP.load(Ordering::SeqCst) {
                    // Shutdown requested while still waiting on Accessibility (or
                    // between retries) — nothing is open yet, so just release the
                    // context and exit; nothing to unschedule/close/release.
                    MONITOR_RUN_LOOP.store(0, Ordering::SeqCst);
                    drop(Arc::from_raw(ctx as *const AtomicBool));
                    return;
                }
                let manager = IOHIDManagerCreate(ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
                if manager.is_null() {
                    if !warned {
                        eprintln!("[dontspeak] IOHIDManagerCreate failed; retrying caps HOLD");
                        warned = true;
                    }
                    std::thread::sleep(HID_OPEN_RETRY);
                    continue;
                }
                // NULL = match all devices; the callback filters to caps usage.
                IOHIDManagerSetDeviceMatching(manager, ptr::null());
                IOHIDManagerRegisterInputValueCallback(manager, caps_value_callback, ctx);
                IOHIDManagerScheduleWithRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
                let rc = IOHIDManagerOpen(manager, KIO_HID_OPTIONS_TYPE_NONE);
                if rc == KERN_SUCCESS {
                    STUCK_GRANT.record_success();
                    if warned {
                        eprintln!("[dontspeak] caps HOLD armed (Accessibility granted)");
                    }
                    // Race guard: if `stop_caps_hid_monitor` already fired between the
                    // loop-top check above and here, skip the blocking call entirely
                    // rather than trust a `CFRunLoopStop` that may have landed before
                    // this run loop was actually running.
                    if !SHOULD_STOP.load(Ordering::SeqCst) {
                        CFRunLoopRun(); // blocks until `stop_caps_hid_monitor`'s CFRunLoopStop
                    }
                    // Torn down: stop delivering input, close + release the manager
                    // (pairs `IOHIDManagerOpen`/`Create`), publish "no monitor running",
                    // and un-leak the context — dropping this Arc clone brings the
                    // strong count back down to just the caller's own clone (see
                    // `macos.rs`'s `caps_down.clone()` call site), instead of pinning
                    // it at +1 for the rest of the process.
                    IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
                    IOHIDManagerClose(manager, KIO_HID_OPTIONS_TYPE_NONE);
                    CFRelease(manager);
                    MONITOR_RUN_LOOP.store(0, Ordering::SeqCst);
                    drop(Arc::from_raw(ctx as *const AtomicBool));
                    return;
                }
                // 0xE00002E2 = kIOReturnNotPermitted → Accessibility not (yet) granted,
                // OR granted after this process was already running (see the module
                // doc's "ACTUAL GOTCHA") — Input Monitoring is never the cause either
                // way. Tell the two cases apart by checking AX trust directly: if it's
                // already true, this denial can NEVER self-heal by retrying in place.
                if !warned {
                    eprintln!(
                        "[dontspeak] IOHIDManagerOpen denied (0x{:08X}); waiting for the \
                         Accessibility grant",
                        rc as u32
                    );
                    warned = true;
                }
                // `record_denial` itself checks AX trust (untrusted ⇒ the normal,
                // unbounded "waiting for the user" wait; trusted ⇒ accumulate toward
                // the stuck latch) and returns `Some(count)` only on the exact call
                // that just crosses the threshold, so this never logs more than once
                // per stuck episode.
                if let Some(count) = STUCK_GRANT.record_denial() {
                    super::stuck_grant::log_stuck("caps HOLD", count);
                }
                // Tear down the denied manager before the next attempt so we don't
                // leak one per retry.
                IOHIDManagerUnscheduleFromRunLoop(manager, run_loop, kCFRunLoopDefaultMode);
                CFRelease(manager);
                std::thread::sleep(HID_OPEN_RETRY);
            }
        })
        .ok();
    *MONITOR_JOIN.lock().unwrap() = handle;
}

/// Tear down the dedicated `IOHIDManager` run-loop thread and release its HID grab —
/// [`spawn_caps_hid_monitor`]'s counterpart, and this platform's contribution to a
/// `Platform::shutdown()` (see the trait-boundary note in `ds-platform/src/lib.rs`'s
/// module docs). Without this, every `ds_engine_stop()` + `ds_engine_start()` cycle
/// permanently orphaned one more monitor thread + open `IOHIDManager` (each still
/// holding the HID grab and writing into its OWN leaked `Arc<AtomicBool>`, invisible to
/// the new monitor's `caps_down` and therefore to the engine — a slow, silent leak of
/// both threads and file-descriptor-like HID handles).
///
/// Signals [`SHOULD_STOP`] (observed at the top of the retry loop, so a monitor still
/// waiting on an ungranted Accessibility permission exits within one [`HID_OPEN_RETRY`]
/// tick instead of retrying forever) and, if a manager is currently open and blocked in
/// `CFRunLoopRun`, calls `CFRunLoopStop` to unblock it immediately. Then JOINS the
/// monitor thread so the manager close/release and the `Arc` un-leak are CONFIRMED
/// complete before returning — not just requested. Idempotent: a no-op if no monitor
/// thread is currently running (never spawned, or already stopped).
///
/// `#[allow(dead_code)]`: not called yet — wiring this in is a one-line addition where
/// `MacOsPlatform`'s teardown/`Platform::shutdown()` lives (`macos.rs`), left for the
/// central pass so this crate keeps building in the meantime.
#[allow(dead_code)]
pub fn stop_caps_hid_monitor() {
    SHOULD_STOP.store(true, Ordering::SeqCst);
    let rl = MONITOR_RUN_LOOP.load(Ordering::SeqCst);
    if rl != 0 {
        unsafe {
            CFRunLoopStop(rl as CFRunLoopRef);
        }
    }
    if let Some(handle) = MONITOR_JOIN.lock().unwrap().take() {
        let _ = handle.join();
    }
}
