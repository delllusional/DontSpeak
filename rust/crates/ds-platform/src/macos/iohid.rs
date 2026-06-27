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
//! PERMISSION: only **Accessibility** is required. Reading input via
//! `IOHIDManagerOpen` is, on its own, gated by the Input Monitoring TCC service
//! (`kTCCServiceListenEvent`) — but an app already trusted for Accessibility is
//! permitted to listen to input, i.e. **Accessibility SUBSUMES Input Monitoring**.
//! The engine already holds the Accessibility grant for CGEventPost injection, so
//! `IOHIDManagerOpen` succeeds with NO separate Input Monitoring grant or row —
//! which is why the app tracks only Accessibility and does not surface a distinct
//! Input Monitoring permission. If open ever returns `kIOReturnNotPermitted`
//! (0xE00002E2) and the HOLD signal stays false, the cause is a MISSING
//! Accessibility grant (fix that), NOT a separate Input Monitoring toggle.
//!
//! Symbols come from the IOKit framework (linked in build.rs); the CFRunLoop
//! symbols come from CoreFoundation (also linked in build.rs).

use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
// F18 — the inert key `capskey::own_caps_key` remaps Caps Lock to. We read the raw device
// element (so we normally still see 0x39 below the system remap), but watch F18 too as a
// hedge in case the monitor ever observes the POST-remap usage instead. Both drive the
// SAME caps-held state; no real F18 key exists on the built-in keyboard, so this can't
// double-fire. See `capskey.rs`.
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
    fn IOHIDManagerOpen(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;

    fn IOHIDValueGetElement(value: IoHidValueRef) -> IoHidElementRef;
    fn IOHIDValueGetIntegerValue(value: IoHidValueRef) -> CFIndex;
    fn IOHIDElementGetUsagePage(element: IoHidElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IoHidElementRef) -> u32;

    // CoreFoundation run-loop plumbing for the dedicated monitor thread.
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopRun();
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

/// Spawn the dedicated `IOHIDManager` run-loop thread that publishes the PHYSICAL
/// Caps-key down state into `caps_down` (true = held). Replaces the lock-coupled
/// CGEvent AlphaShift tap as the HOLD signal source.
///
/// On failure (manager create or — most likely — Accessibility not yet granted) it
/// logs a one-line diagnostic and leaves `caps_down` at false: the engine keeps
/// running as a pure RPC host, dictation just never starts until the grant lands.
pub fn spawn_caps_hid_monitor(caps_down: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("ds-caps-hid".into())
        .spawn(move || unsafe {
            let manager = IOHIDManagerCreate(ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
            if manager.is_null() {
                eprintln!("[dontspeak] IOHIDManagerCreate failed; caps HOLD disabled");
                return;
            }
            // NULL = match all devices; the callback filters to caps usage.
            IOHIDManagerSetDeviceMatching(manager, ptr::null());
            // Leak an Arc clone as the callback context; the thread (and thus the
            // manager + context) lives for the whole process.
            let ctx = Arc::into_raw(caps_down) as *mut c_void;
            IOHIDManagerRegisterInputValueCallback(manager, caps_value_callback, ctx);
            IOHIDManagerScheduleWithRunLoop(manager, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
            let rc = IOHIDManagerOpen(manager, KIO_HID_OPTIONS_TYPE_NONE);
            if rc != KERN_SUCCESS {
                // 0xE00002E2 = kIOReturnNotPermitted → Accessibility not granted.
                // Accessibility subsumes Input Monitoring for this read, so the fix is
                // the Accessibility grant the engine already needs for key injection —
                // there is no separate Input Monitoring toggle to flip.
                eprintln!(
                    "[dontspeak] IOHIDManagerOpen failed (0x{:08X}); grant Accessibility \
                     in System Settings > Privacy & Security > Accessibility, then reload \
                     the engine — caps HOLD disabled until then",
                    rc as u32
                );
                return;
            }
            CFRunLoopRun(); // never returns; owns the manager + leaked context.
        })
        .ok();
}
