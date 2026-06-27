//! ds-aec-probe — M1 smoke test for the macOS VPIO duplex unit.
//!
//! Opens [`ds_aec::DuplexAudio`], renders a 440 Hz tone for a few seconds, and every
//! 100 ms drains the echo-cancelled capture and prints its RMS. What to look for:
//!   • `open` succeeds and `capture_rate` prints (the VPIO unit started),
//!   • `cap_n` is non-zero each tick (the INPUT callback fires — capture works),
//!   • with the tone playing OUT the speakers, capture RMS stays near the room
//!     floor rather than spiking to the tone level (AEC is removing our render).
//! Run on-device with a real mic + speakers (not headphones — the echo path needs
//! the speaker→mic acoustic coupling to be worth cancelling).

#[cfg(target_os = "macos")]
fn main() {
    use std::f32::consts::PI;
    use std::time::Duration;

    use ds_aec::DuplexAudio;

    let dx = match DuplexAudio::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ds-aec-probe: open failed: {e}");
            std::process::exit(1);
        }
    };
    println!("opened VPIO; capture_rate = {} Hz", dx.capture_rate());

    // 100 ms tone chunks at the 24 kHz synth rate (render_push resamples up).
    const SYNTH_RATE: f32 = 24_000.0;
    const CHUNK: usize = 2_400; // 100 ms
    const FREQ: f32 = 440.0;
    let mut phase = 0.0f32;

    for tick in 0..40 {
        // ~3 s of tone, then ~1 s of silence to compare floors.
        let mut chunk = Vec::with_capacity(CHUNK);
        let playing = tick < 30;
        for _ in 0..CHUNK {
            let s = if playing {
                (phase * 2.0 * PI).sin() * 0.3
            } else {
                0.0
            };
            chunk.push(s);
            phase += FREQ / SYNTH_RATE;
            if phase >= 1.0 {
                phase -= 1.0;
            }
        }
        dx.render_push(&chunk);

        std::thread::sleep(Duration::from_millis(100));

        let cap = dx.capture_drain();
        let rms = if cap.is_empty() {
            0.0
        } else {
            (cap.iter().map(|x| x * x).sum::<f32>() / cap.len() as f32).sqrt()
        };
        println!(
            "t={:>4}ms  render={}  cap_n={:>5}  rms={:.4}  pending={}",
            tick * 100,
            if playing { "tone " } else { "quiet" },
            cap.len(),
            rms,
            dx.render_pending(),
        );
    }
    println!("done");
}

// Windows: capture-side AEC (no render_push — rodio renders TTS elsewhere). Open the
// Communications-category capture and print its RMS every 100 ms. The echo-suppression
// check (§9): play a tone / a TTS reply through the speakers WHILE this runs and confirm
// the captured RMS stays near the no-playback floor (the APO is cancelling the render).
#[cfg(windows)]
fn main() {
    use std::time::Duration;

    use ds_aec::DuplexAudio;

    let dx = match DuplexAudio::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ds-aec-probe: open failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "opened WASAPI Communications capture; capture_rate = {} Hz",
        dx.capture_rate()
    );
    println!("speak/play audio out the speakers now; captured RMS should stay near the room floor");

    for tick in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        let cap = dx.capture_drain();
        let rms = if cap.is_empty() {
            0.0
        } else {
            (cap.iter().map(|x| x * x).sum::<f32>() / cap.len() as f32).sqrt()
        };
        println!(
            "t={:>4}ms  cap_n={:>5}  rms={:.4}",
            tick * 100,
            cap.len(),
            rms
        );
    }
    println!("done");
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    eprintln!(
        "ds-aec-probe: native duplex AEC not implemented on this platform (see docs/AEC.md §6/§7)"
    );
    std::process::exit(1);
}
