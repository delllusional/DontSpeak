//! Smoke-test the FluidAudio Core ML / ANE Kokoro backend (the libsmkokoro shim).
//! Requires SMKOKORO_DYLIB_PATH to point at a built libsmkokoro.dylib.
//!   SMKOKORO_DYLIB_PATH=.../libsmkokoro.dylib \
//!     cargo run -q --release -p ds-tts --example ane_check
//!
//! macOS-only: the Core ML backend (`ds_tts::synth_coreml`) is `#[cfg(target_os = "macos")]`,
//! so this example is gated to match — otherwise `cargo …--all-targets` (clippy/CI on Linux
//! + the Windows dev box) fails to compile the unconditional import.
#[cfg(target_os = "macos")]
use ds_tts::synth_coreml::CoremlKokoro;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("ane_check is macOS-only (the Core ML / ANE backend is not built on this target)");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let t = Instant::now();
    let synth = match CoremlKokoro::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ANE load FAILED: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "provider: {}  (loaded in {:.2}s)",
        synth.provider(),
        t.elapsed().as_secs_f32()
    );

    let text = "The neural engine is now synthesizing speech on device.";
    let t2 = Instant::now();
    match synth.synthesize_text(text, "af_heart", 1.0) {
        Ok(pcm) => {
            let audio_s = pcm.len() as f32 / 24_000.0;
            let synth_s = t2.elapsed().as_secs_f32();
            println!(
                "synthesized {} samples ({audio_s:.2}s audio) in {synth_s:.2}s  rtf={:.3} ({:.1}x faster)",
                pcm.len(),
                synth_s / audio_s.max(0.0001),
                audio_s / synth_s.max(0.0001),
            );
            if pcm.is_empty() {
                eprintln!("WARNING: empty PCM");
                std::process::exit(2);
            }
            println!("OK: ANE Kokoro produced audio");
        }
        Err(e) => {
            eprintln!("synthesize FAILED: {e}");
            std::process::exit(1);
        }
    }
}
