//! Cross-platform microphone-in-use WATCHER with a uniform PUSH interface.
//!
//! Where [`mic_active`](crate::mic_active) is a one-shot probe (an OS query per call), a
//! [`MicWatcher`] tracks the state continuously and exposes it as a cheap cached read plus
//! an on-change callback, so consumers (the TTS barge gate, the worker focus-hold) react to
//! an EVENT instead of querying the device on a timer.
//!
//! Backends:
//! * **macOS** — a native CoreAudio property listener on
//!   `kAudioDevicePropertyDeviceIsRunningSomewhere` (zero polling), re-registered onto the
//!   new device when the default input changes. Uses the FUNCTION-POINTER listener API
//!   (`AudioObjectAddPropertyListener`), NOT the block API: block-based REMOVAL is known to
//!   be unreliable, and we must detach cleanly on drop. Falls back to the poll thread below
//!   if registration fails.
//! * **Windows / Linux** — one centralized poll thread reusing [`mic_active`]. (Windows has
//!   a real WASAPI probe; Linux currently has none, so the watcher stays `false` — no gate,
//!   same as today.) Centralizing it means consumers no longer each poll the device.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Poll cadence for the portable backend (Windows/Linux, or the macOS fallback). Matches
/// the prior ad-hoc barge poll; resume latency here is generous.
const POLL_INTERVAL: Duration = Duration::from_millis(150);

type ChangeFn = Arc<dyn Fn(bool) + Send + Sync>;

/// A running watcher of system mic-in-use state. Drop it to stop watching (detaches the
/// native listener / joins the poll thread).
pub struct MicWatcher {
    state: Arc<AtomicBool>,
    _backend: Backend,
}

impl MicWatcher {
    /// Start watching. `on_change(active)` fires once per state flip — from the CoreAudio
    /// notification thread on macOS, or the poll thread elsewhere. The initial state is
    /// sampled synchronously here and is readable immediately via [`is_active`](Self::is_active).
    pub fn spawn<F: Fn(bool) + Send + Sync + 'static>(on_change: F) -> MicWatcher {
        let state = Arc::new(AtomicBool::new(crate::mic_active()));
        let cb: ChangeFn = Arc::new(on_change);
        let backend = Backend::start(state.clone(), cb);
        MicWatcher {
            state,
            _backend: backend,
        }
    }

    /// Last-known mic-in-use state — a cheap atomic read, no OS query.
    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::Relaxed)
    }

    /// A cheap, cloneable READ handle to this watcher's state. Share it with several
    /// consumers (the barge watcher, the TTS worker's focus-hold) so they all read the ONE
    /// watcher's cached value instead of each querying the device on a timer.
    pub fn handle(&self) -> MicState {
        MicState(self.state.clone())
    }
}

/// A cheap, cloneable read handle to a [`MicWatcher`]'s state. If the watcher is dropped the
/// value simply freezes at its last reading — reads stay safe.
#[derive(Clone)]
pub struct MicState(Arc<AtomicBool>);

impl MicState {
    /// Last-known mic-in-use state — a cheap atomic read, no OS query.
    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

// ── Backend selection ────────────────────────────────────────────────────────

// Variants are RAII guards held only for their `Drop` (detach listener / join thread); the
// inner value is never read back, hence the allow.
#[allow(dead_code)]
enum Backend {
    Poll(PollThread),
    #[cfg(target_os = "macos")]
    CoreAudio(macos::Listener),
}

impl Backend {
    fn start(state: Arc<AtomicBool>, cb: ChangeFn) -> Backend {
        #[cfg(target_os = "macos")]
        {
            match macos::Listener::start(state.clone(), cb.clone()) {
                Ok(l) => return Backend::CoreAudio(l),
                Err(()) => { /* registration failed → fall through to the poll backend */ }
            }
        }
        Backend::Poll(PollThread::start(state, cb))
    }
}

// ── Portable poll backend (Windows / Linux / macOS fallback) ─────────────────

struct PollThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PollThread {
    fn start(state: Arc<AtomicBool>, cb: ChangeFn) -> PollThread {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("mic-watch".into())
            .spawn(move || {
                let mut cur = state.load(Ordering::Relaxed);
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL_INTERVAL);
                    let now = crate::mic_active();
                    if now != cur {
                        cur = now;
                        state.store(now, Ordering::Relaxed);
                        cb(now);
                    }
                }
            })
            .ok();
        PollThread { stop, handle }
    }
}

impl Drop for PollThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ── macOS native CoreAudio listener backend ──────────────────────────────────

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use objc2_core_audio::{
        AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectPropertyAddress,
        AudioObjectRemovePropertyListener, kAudioDevicePropertyDeviceIsRunningSomewhere,
        kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    };

    use super::ChangeFn;

    const SYS: u32 = kAudioObjectSystemObject as u32;

    fn prop_addr(selector: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            mSelector: selector,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        }
    }

    fn default_input_device() -> u32 {
        let a = prop_addr(kAudioHardwarePropertyDefaultInputDevice);
        let mut id: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let rc = unsafe {
            AudioObjectGetPropertyData(
                SYS,
                NonNull::from(&a),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(&mut id as *mut u32 as *mut c_void).unwrap(),
            )
        };
        if rc == 0 { id } else { 0 }
    }

    /// Context handed to the C callbacks via their `*mut c_void` client-data slot. Heap-
    /// stable for the listener's lifetime. We INTENTIONALLY leak it on drop (see `Listener`)
    /// so a CoreAudio callback still in flight when we detach can never touch freed memory.
    struct Ctx {
        state: Arc<AtomicBool>,
        cb: ChangeFn,
        /// Device the run-state listener is currently attached to (mutated only from the
        /// device-change callback, which CoreAudio serializes per listener).
        dev: AtomicU32,
    }

    impl Ctx {
        fn recompute(&self) {
            let now = crate::mic_active();
            let prev = self.state.swap(now, Ordering::Relaxed);
            if now != prev {
                (self.cb)(now);
            }
        }
    }

    /// The default input device's run-state changed (mic started/stopped somewhere).
    unsafe extern "C-unwind" fn running_cb(
        _obj: u32,
        _n: u32,
        _addrs: NonNull<AudioObjectPropertyAddress>,
        ctx: *mut c_void,
    ) -> i32 {
        unsafe { &*(ctx as *const Ctx) }.recompute();
        0
    }

    /// The user switched the default input device → move the run-state listener to the new
    /// device, then recompute (the switch itself may change the running state).
    unsafe extern "C-unwind" fn device_cb(
        _obj: u32,
        _n: u32,
        _addrs: NonNull<AudioObjectPropertyAddress>,
        ctx: *mut c_void,
    ) -> i32 {
        let ctx_ref = unsafe { &*(ctx as *const Ctx) };
        let new_dev = default_input_device();
        let old_dev = ctx_ref.dev.swap(new_dev, Ordering::Relaxed);
        if new_dev != old_dev {
            let run = prop_addr(kAudioDevicePropertyDeviceIsRunningSomewhere);
            unsafe {
                if old_dev != 0 {
                    AudioObjectRemovePropertyListener(
                        old_dev,
                        NonNull::from(&run),
                        Some(running_cb),
                        ctx,
                    );
                }
                if new_dev != 0 {
                    AudioObjectAddPropertyListener(
                        new_dev,
                        NonNull::from(&run),
                        Some(running_cb),
                        ctx,
                    );
                }
            }
        }
        ctx_ref.recompute();
        0
    }

    pub(super) struct Listener {
        ctx: *mut Ctx,
    }

    // The raw ptr is used only to detach on drop; all concurrent access is owned by the
    // CoreAudio callbacks via atomics. Safe to move the handle across threads.
    unsafe impl Send for Listener {}
    unsafe impl Sync for Listener {}

    impl Listener {
        pub(super) fn start(state: Arc<AtomicBool>, cb: ChangeFn) -> Result<Listener, ()> {
            let dev = default_input_device();
            let ctx = Box::into_raw(Box::new(Ctx {
                state,
                cb,
                dev: AtomicU32::new(dev),
            }));
            let ctx_void = ctx as *mut c_void;

            let dev_addr = prop_addr(kAudioHardwarePropertyDefaultInputDevice);
            // Always watch default-device changes on the system object.
            let rc_dev = unsafe {
                AudioObjectAddPropertyListener(
                    SYS,
                    NonNull::from(&dev_addr),
                    Some(device_cb),
                    ctx_void,
                )
            };
            if rc_dev != 0 {
                // Couldn't even attach the device-change listener → free ctx, use poll.
                drop(unsafe { Box::from_raw(ctx) });
                return Err(());
            }
            // Watch the current device's run-state (best-effort: a missing device just means
            // no run events until one appears, which the device-change path then arms).
            if dev != 0 {
                let run_addr = prop_addr(kAudioDevicePropertyDeviceIsRunningSomewhere);
                unsafe {
                    AudioObjectAddPropertyListener(
                        dev,
                        NonNull::from(&run_addr),
                        Some(running_cb),
                        ctx_void,
                    );
                }
            }
            // Seed the cached state from a live read now that the listeners are armed.
            unsafe { &*ctx }.recompute();
            Ok(Listener { ctx })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let dev_addr = prop_addr(kAudioHardwarePropertyDefaultInputDevice);
            let run_addr = prop_addr(kAudioDevicePropertyDeviceIsRunningSomewhere);
            let ctx_void = self.ctx as *mut c_void;
            let dev = unsafe { &*self.ctx }.dev.load(Ordering::Relaxed);
            unsafe {
                AudioObjectRemovePropertyListener(
                    SYS,
                    NonNull::from(&dev_addr),
                    Some(device_cb),
                    ctx_void,
                );
                if dev != 0 {
                    AudioObjectRemovePropertyListener(
                        dev,
                        NonNull::from(&run_addr),
                        Some(running_cb),
                        ctx_void,
                    );
                }
            }
            // INTENTIONAL LEAK of `self.ctx`: removal guarantees no NEW callbacks, but an
            // in-flight one on a CoreAudio thread may still be running, so freeing here could
            // use-after-free. The Ctx is tiny and dropping an engine-lifetime watcher is rare.
        }
    }
}
