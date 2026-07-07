//! Direct keyboard Caps-Lock LED writer via the HID Manager — the robust,
//! external-keyboard-safe way to drive the LED as the dictation indicator.
//!
//! Why this exists (the gap it closes): the engine drives the Caps LED as a pure
//! dictation indicator. The lock-coupled `IOHIDSetModifierLockState` write in
//! `iokit.rs` does NOT reliably reach EXTERNAL / Bluetooth keyboards — the same
//! blind spot that forces `iohid.rs` to read the physical key instead of the lock
//! state. So a press that should leave the LED OFF (e.g. a tap that cancels TTS
//! playback while idle) left the light stuck ON, whereas the key-owning Windows
//! port never lights it. Setting the device's `kHIDPage_LEDs` / Caps-Lock element
//! directly drives the PHYSICAL LED on every keyboard, decoupled from the logical
//! caps state (verified to NOT change the caps lock state) — the macOS analogue of
//! Linux's `EV_LED`/`LED_CAPSL` and the Windows `IOCTL_KEYBOARD_SET_INDICATORS`
//! path. `iokit::CapsReader::set_caps_lock` keeps driving the LOGICAL lock (so a
//! physical toggle can't leave capitals stuck on); this adds the physical-LED half.
//!
//! Uses the MODERN HID Manager API: the legacy `IOHIDDeviceInterface122` plug-in
//! path breaks on macOS 14+. Match keyboards, copy the (manager-opened) devices,
//! find each one's Caps-Lock LED element, and `IOHIDDeviceSetValue` an integer
//! on/off. Output `SetValue` is synchronous, so no run-loop scheduling is needed.
//! Symbols come from IOKit / CoreFoundation (linked in build.rs).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::os::raw::{c_int, c_uint};
use std::ptr;
use std::time::Instant;

use super::stuck_grant::StuckGrantLatch;

// IOKit / CoreFoundation opaque + scalar typedefs.
type IoHidManagerRef = *mut c_void;
type IoHidDeviceRef = *mut c_void;
type IoHidElementRef = *mut c_void;
type IoHidValueRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFArrayRef = *const c_void;
type CFSetRef = *const c_void;
type CFIndex = isize;
type IoReturn = c_int;
type IoOptionBits = c_uint;
type Boolean = u8;

const KIO_HID_OPTIONS_TYPE_NONE: IoOptionBits = 0;
const KIO_RETURN_SUCCESS: IoReturn = 0;

// HID usage pages / usages (from the HID usage tables).
const K_HID_PAGE_GENERIC_DESKTOP: u32 = 0x01;
const K_HID_USAGE_GD_KEYBOARD: u32 = 0x06;
const K_HID_PAGE_LEDS: u32 = 0x08;
const K_HID_USAGE_LED_CAPSLOCK: u32 = 0x02;

unsafe extern "C" {
    fn IOHIDManagerCreate(allocator: CFAllocatorRef, options: IoOptionBits) -> IoHidManagerRef;
    // NULL matching = match ALL devices; we filter to keyboards (IOHIDDeviceConformsTo)
    // and to the Caps LED element (by usage) below, so no CFDictionary is built.
    fn IOHIDManagerSetDeviceMatching(manager: IoHidManagerRef, matching: CFDictionaryRef);
    fn IOHIDManagerOpen(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    fn IOHIDManagerClose(manager: IoHidManagerRef, options: IoOptionBits) -> IoReturn;
    fn IOHIDManagerCopyDevices(manager: IoHidManagerRef) -> CFSetRef;
    fn IOHIDDeviceConformsTo(device: IoHidDeviceRef, usage_page: u32, usage: u32) -> Boolean;
    fn IOHIDDeviceCopyMatchingElements(
        device: IoHidDeviceRef,
        matching: CFDictionaryRef,
        options: IoOptionBits,
    ) -> CFArrayRef;
    fn IOHIDElementGetUsagePage(element: IoHidElementRef) -> u32;
    fn IOHIDElementGetUsage(element: IoHidElementRef) -> u32;
    fn IOHIDValueCreateWithIntegerValue(
        allocator: CFAllocatorRef,
        element: IoHidElementRef,
        timestamp: u64,
        value: CFIndex,
    ) -> IoHidValueRef;
    // Output `SetValue` needs the device OPEN — guaranteed here by IOHIDManagerOpen,
    // which opens every device the manager owns (else SetValue → kIOReturnNotOpen).
    fn IOHIDDeviceSetValue(
        device: IoHidDeviceRef,
        element: IoHidElementRef,
        value: IoHidValueRef,
    ) -> IoReturn;

    fn CFSetGetCount(set: CFSetRef) -> CFIndex;
    fn CFSetGetValues(set: CFSetRef, values: *mut *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// Owns an open HID Manager (matching all devices) used to drive the physical
/// Caps-Lock LED on every keyboard.
pub struct CapsLed {
    manager: IoHidManagerRef,
}

// SAFETY: `CapsLed` wraps only the IOHIDManagerRef it owns, and HID Manager calls are not
// bound to the creating thread; it is touched only from the engine's single poll thread —
// same contract as `iokit::CapsReader` (whose connection carries the identical Send impl).
unsafe impl Send for CapsLed {}

/// Why [`CapsLed::try_open`] returns this instead of a bare `Option`: `RetryingCapsLed`
/// only counts a failure toward its stuck-grant latch when it's actually the
/// Accessibility-gated `IOHIDManagerOpen` denial the latch exists to detect — NOT a
/// generic `IOHIDManagerCreate` failure (a rare, likely transient allocation issue
/// unrelated to permissions), mirroring the exact distinction `iohid.rs`'s own retry
/// loop already draws between the two.
enum OpenFailure {
    ManagerCreateFailed,
    Denied,
}

impl CapsLed {
    /// Create + open the manager, distinguishing WHY it failed (see [`OpenFailure`]).
    fn try_open() -> Result<Self, OpenFailure> {
        // SAFETY: IOHIDManagerCreate's result is null-checked before use;
        // SetDeviceMatching accepts a NULL dictionary (match all devices); and on the open-
        // denied path the manager is released exactly once here — on success `CapsLed`
        // owns it and `Drop` closes/releases it.
        unsafe {
            let manager = IOHIDManagerCreate(ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
            if manager.is_null() {
                return Err(OpenFailure::ManagerCreateFailed);
            }
            IOHIDManagerSetDeviceMatching(manager, ptr::null());
            if IOHIDManagerOpen(manager, KIO_HID_OPTIONS_TYPE_NONE) != KIO_RETURN_SUCCESS {
                CFRelease(manager);
                return Err(OpenFailure::Denied);
            }
            Ok(CapsLed { manager })
        }
    }

    /// Drive the Caps-Lock LED on every keyboard to `on`, WITHOUT changing the
    /// logical caps state. Best-effort: re-enumerates devices each call (so a
    /// hot-plugged keyboard is covered) and ignores per-device failures.
    pub fn set(&self, on: bool) {
        // SAFETY: `self.manager` is the manager `try_open` created and opened, owned until
        // `Drop`. IOHIDManagerCopyDevices returns a +1 CFSet (null-checked, released
        // exactly once below); CFSetGetValues fills a Vec sized to CFSetGetCount, and the
        // device refs it yields are borrowed from that set (never released individually)
        // and null-checked before use.
        unsafe {
            let devices = IOHIDManagerCopyDevices(self.manager);
            if devices.is_null() {
                return;
            }
            let count = CFSetGetCount(devices);
            if count > 0 {
                let mut refs: Vec<*const c_void> = vec![ptr::null(); count as usize];
                CFSetGetValues(devices, refs.as_mut_ptr());
                for &d in &refs {
                    let device = d as IoHidDeviceRef;
                    if device.is_null()
                        || IOHIDDeviceConformsTo(
                            device,
                            K_HID_PAGE_GENERIC_DESKTOP,
                            K_HID_USAGE_GD_KEYBOARD,
                        ) == 0
                    {
                        continue;
                    }
                    set_caps_led_on_device(device, on);
                }
            }
            // `IOHIDManagerCopyDevices` returns a +1 CFSet; the device refs inside are
            // borrowed (do NOT release them individually).
            CFRelease(devices);
        }
    }
}

/// Find the device's Caps-Lock LED element and set it. `device` is borrowed.
unsafe fn set_caps_led_on_device(device: IoHidDeviceRef, on: bool) {
    // SAFETY: `device` is a live, manager-opened device for the duration of the call
    // (caller contract — `set` borrows it from a CFSet it still holds, and
    // IOHIDManagerOpen opened every device the manager owns, as IOHIDDeviceSetValue
    // requires). IOHIDDeviceCopyMatchingElements returns a +1 CFArray (null-checked,
    // released exactly once below) whose elements are borrowed and null-checked; index
    // reads stay within CFArrayGetCount; the created IOHIDValue is a +1 ref released right
    // after the synchronous SetValue.
    unsafe {
        // NULL matching = all elements; filter to the Caps LED by usage page/usage.
        let elements =
            IOHIDDeviceCopyMatchingElements(device, ptr::null(), KIO_HID_OPTIONS_TYPE_NONE);
        if elements.is_null() {
            return;
        }
        let n = CFArrayGetCount(elements);
        for i in 0..n {
            let element = CFArrayGetValueAtIndex(elements, i) as IoHidElementRef;
            if element.is_null() {
                continue;
            }
            if IOHIDElementGetUsagePage(element) == K_HID_PAGE_LEDS
                && IOHIDElementGetUsage(element) == K_HID_USAGE_LED_CAPSLOCK
            {
                // +1 value; release after the set.
                let value =
                    IOHIDValueCreateWithIntegerValue(ptr::null(), element, 0, on as CFIndex);
                if !value.is_null() {
                    let _ = IOHIDDeviceSetValue(device, element, value);
                    CFRelease(value);
                }
                // One Caps-LED element per keyboard — stop after the first.
                break;
            }
        }
        // `IOHIDDeviceCopyMatchingElements` returns a +1 CFArray; elements borrowed.
        CFRelease(elements);
    }
}

impl Drop for CapsLed {
    fn drop(&mut self) {
        // SAFETY: `self.manager` is the manager `try_open` created and opened; it is
        // closed and released at most once (nulled right after), pairing
        // IOHIDManagerCreate/IOHIDManagerOpen.
        unsafe {
            if !self.manager.is_null() {
                IOHIDManagerClose(self.manager, KIO_HID_OPTIONS_TYPE_NONE);
                CFRelease(self.manager);
                self.manager = ptr::null_mut();
            }
        }
    }
}

/// How long [`RetryingCapsLed`] waits between re-open attempts while the LED writer
/// is missing. The SAME constant `iohid.rs`'s monitor thread uses — both live in
/// `stuck_grant`, their shared home, since this is about the stuck-grant bug
/// class both hit, not anything specific to either caller.
use super::stuck_grant::RETRY_INTERVAL as LED_RETRY_INTERVAL;

/// Consecutive `CapsLed::open()` denials tolerated WHILE Accessibility is already
/// trusted before concluding this process's grant is stale for the LED writer too
/// (see `stuck_grant`'s doc, and `iohid.rs`'s module doc for the full "ACTUAL
/// GOTCHA" story — this is the exact same bug, hitting a second `IOHIDManagerOpen`
/// call site). A `static`, for the same reason `iohid.rs`'s equivalent is one: it
/// must survive `RetryingCapsLed` being reconstructed across an in-process engine
/// restart. Shares `stuck_grant`'s threshold constant rather than its own literal —
/// both call sites are tuned as one decision, not two that could drift apart.
static LED_STUCK: StuckGrantLatch =
    StuckGrantLatch::new(super::stuck_grant::STUCK_RETRIES_BEFORE_RELAUNCH);

/// A [`CapsLed`] that retries opening itself on a throttled cadence when missing,
/// instead of giving up forever after one failed attempt at construction time.
///
/// Why this exists: `CapsLed::open()` is a single synchronous attempt with no retry
/// of its own. If Accessibility wasn't granted at the exact instant `MacOsPlatform::new`
/// called it, the LED indicator went dark for the rest of the process's life — even
/// in cases where `iohid.rs`'s caps-HID monitor (which DOES retry) recovers fine on
/// its own a moment later, well before ever reaching ITS stuck threshold. Unlike that
/// monitor, the LED writer doesn't need a dedicated thread blocked in a run loop — it
/// has no callback to keep receiving, just a synchronous `set()` call driven by caps
/// edges — so retrying opportunistically, throttled, whenever `set()` is next called
/// is enough: there is no LED update to show if `set()` is never called anyway. Shares
/// [`StuckGrantLatch`] with `iohid.rs` rather than reimplementing the denied-while-
/// trusted counter, and feeds the SAME relaunch mechanism via [`is_stuck`](Self::is_stuck)
/// (see `MacOsPlatform::caps_monitor_stuck`).
///
/// TRADE-OFF, accepted deliberately: `iohid.rs`'s stuck threshold reliably bounds
/// WALL-CLOCK time (~6s: a dedicated thread ticks every `HID_OPEN_RETRY` regardless
/// of caps activity), but this one only bounds ATTEMPT count — if `set()` is called
/// rarely (the user isn't pressing Caps), reaching the same threshold can take much
/// longer, or never happen at all in a given session. That's fine here: an unlit
/// LED with no caps activity to show it isn't user-visible, and it's just a visual
/// indicator, not the dictation-breaking failure `iohid.rs`'s monitor guards. Giving
/// this its own dedicated polling thread purely to bound worst-case wall-clock time
/// would reintroduce exactly the complexity this design avoids for a cosmetic-only
/// indicator.
pub struct RetryingCapsLed {
    led: RefCell<Option<CapsLed>>,
    // Seeded a full `LED_RETRY_INTERVAL` in the PAST (see `new()`), not to
    // `Instant::now()`: the first `retry_if_needed()` call — from `new()` itself —
    // must actually attempt an open, not have the throttle immediately swallow it
    // by comparing "now" against a timestamp stamped microseconds earlier.
    last_attempt: Cell<Instant>,
    /// Warn on `IOHIDManagerCreate` failure ONCE per streak, not every retry —
    /// mirrors `iohid.rs`'s own `warned` flag for the identical failure mode.
    /// Reset on a later success (see `retry_if_needed`) so a fresh failure streak
    /// warns again.
    manager_create_warned: Cell<bool>,
}

// `RetryingCapsLed` auto-derives `Send` (its only fields, `RefCell<Option<CapsLed>>`
// and `Cell<Instant>`, are `Send` since `CapsLed` already is — no manual impl
// needed). Touched only from the engine's single poll thread regardless, same
// contract as `CapsLed`.

impl RetryingCapsLed {
    /// Attempts to open immediately (matching the old one-shot `CapsLed::open()`
    /// call site in `MacOsPlatform::new`), then leaves further retries to `set()`.
    pub fn new() -> Self {
        let this = Self {
            led: RefCell::new(None),
            last_attempt: Cell::new(
                Instant::now()
                    .checked_sub(LED_RETRY_INTERVAL)
                    .unwrap_or_else(Instant::now),
            ),
            manager_create_warned: Cell::new(false),
        };
        this.retry_if_needed();
        this
    }

    /// Drive the Caps-Lock LED to `on`. Best-effort: a no-op while the writer is
    /// still missing (retried, throttled, on this same call).
    pub fn set(&self, on: bool) {
        self.retry_if_needed();
        if let Some(led) = self.led.borrow().as_ref() {
            led.set(on);
        }
    }

    fn retry_if_needed(&self) {
        if self.led.borrow().is_some() {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_attempt.get()) < LED_RETRY_INTERVAL {
            return;
        }
        self.last_attempt.set(now);
        match CapsLed::try_open() {
            Ok(led) => {
                LED_STUCK.record_success();
                self.manager_create_warned.set(false);
                *self.led.borrow_mut() = Some(led);
            }
            // Only a genuine `IOHIDManagerOpen` denial counts toward the stuck
            // latch — mirrors `iohid.rs`'s own retry loop, which likewise never
            // counts a bare `IOHIDManagerCreate` failure (a rare, likely-transient
            // allocation issue, NOT the Accessibility-gated denial this latch
            // exists to detect) toward its threshold.
            Err(OpenFailure::Denied) => {
                if let Some(count) = LED_STUCK.record_denial() {
                    super::stuck_grant::log_stuck("Caps LED writer", count);
                }
            }
            Err(OpenFailure::ManagerCreateFailed) => {
                // Same one-shot-per-streak warn as `iohid.rs`'s parallel path —
                // without it, a persistently failing `IOHIDManagerCreate` for the
                // LED writer left zero diagnostic trace anywhere for the process's
                // whole life.
                if !self.manager_create_warned.replace(true) {
                    eprintln!("[dontspeak] Caps LED: IOHIDManagerCreate failed; retrying");
                }
            }
        }
    }

    /// Whether this process's LED writer is confirmed stuck — see [`LED_STUCK`].
    pub fn is_stuck(&self) -> bool {
        LED_STUCK.is_stuck()
    }
}
