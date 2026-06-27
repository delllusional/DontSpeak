//! Full-duplex dev probe: the VPIO duplex unit + a separate cpal capture coexist
//! check ([`coexist_probe`]). The render/capture wiring the warm serve loop drives
//! lives in [`crate::serve`]; this is the standalone `--coexist-probe` diagnostic.

#[cfg(target_os = "macos")]
use ds_aec::DuplexAudio;

use crate::_exit;

/// Dev probe: open the VPIO duplex unit AND a separate cpal `Capture` on the same
/// default input, and confirm the cpal stream still receives audio. This is the
/// risk for full-duplex always-listening (the engine's `Listener` opens its own
/// cpal mic while the warm helper's VPIO unit stays open). Prints per-tick sample
/// counts for both; cpal `cap_n > 0` ⇒ they coexist.
#[cfg(target_os = "macos")]
pub(crate) fn coexist_probe() -> ! {
    use std::time::Duration;
    let dx = match DuplexAudio::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("coexist-probe: VPIO open failed: {e}");
            unsafe { _exit(1) }
        }
    };
    let cpal = match ds_stt::Capture::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("coexist-probe: cpal Capture open FAILED alongside VPIO: {e}");
            unsafe { _exit(2) }
        }
    };
    println!(
        "both open: VPIO {} Hz + cpal {} Hz",
        dx.capture_rate(),
        cpal.input_rate()
    );
    let mut cpal_total = 0usize;
    for t in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        let v = dx.capture_drain().len();
        let c = cpal.drain_new().len();
        cpal_total += c;
        println!("t={:>4}ms  vpio_n={:>5}  cpal_n={:>5}", t * 100, v, c);
    }
    println!(
        "result: cpal {} alongside VPIO",
        if cpal_total > 0 {
            "RECEIVES AUDIO (coexist OK)"
        } else {
            "got NOTHING (conflict)"
        }
    );
    unsafe { _exit(if cpal_total > 0 { 0 } else { 3 }) }
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn coexist_probe() -> ! {
    eprintln!("coexist-probe: macOS-only");
    unsafe { _exit(1) }
}
