//! Smoke-test the MLX Audio Kokoro backend through the libdontspeak_mlx shim.
//! Requires DONTSPEAK_MLX_DYLIB_PATH to point at a built libdontspeak_mlx.dylib.
//!   DONTSPEAK_MLX_DYLIB_PATH=.../libdontspeak_mlx.dylib \
//!     cargo run -q --release -p ds-tts --example mlx_check
//!
//! macOS-only: the MLX backend (`ds_tts::synth_mlx`) is `#[cfg(target_os = "macos")]`,
//! so this example is gated to match — otherwise `cargo …--all-targets` (clippy/CI on Linux
//! + the Windows dev box) fails to compile the unconditional import.
#[cfg(target_os = "macos")]
use ds_tts::synth_mlx::MlxTts;
#[cfg(target_os = "macos")]
use std::time::Instant;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mlx_check is macOS-only (the MLX backend is not built on this target)");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    let t = Instant::now();
    let synth = match MlxTts::load(ds_config::TtsModel::Kokoro) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("MLX load FAILED: {e}");
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
    let phonemes = ds_tts::g2p::phonemize(text);
    match synth.synthesize(&phonemes, "af_heart", "en", 1.0) {
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
            println!("OK: MLX Kokoro produced audio");
        }
        Err(e) => {
            eprintln!("synthesize FAILED: {e}");
            std::process::exit(1);
        }
    }
}
