//! Smoke probe for duplex AEC: open, capture RMS, confirm echo stays near room floor.
//! Needs real mic + speakers (not headphones — needs acoustic speaker→mic coupling).

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

    // 100 ms tone chunks at 24 kHz synth rate (render_push resamples up).
    const SYNTH_RATE: f32 = 24_000.0;
    const CHUNK: usize = 2_400; // 100 ms
    const FREQ: f32 = 440.0;
    let mut phase = 0.0f32;

    for tick in 0..40 {
        // ~3 s tone, then ~1 s silence to compare floors.
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

// Capture-side only (no render_push). Play audio out speakers while running — RMS should
// stay near the no-playback floor if the Communications APO is cancelling.
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

#[cfg(target_os = "linux")]
fn main() {
    use std::time::Duration;

    let dx = match ds_aec::DuplexAudio::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ds-aec-probe: open failed: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "opened Linux capture; capture_rate = {} Hz",
        dx.capture_rate()
    );
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
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn main() {
    eprintln!("ds-aec-probe: native duplex AEC not implemented on this platform");
    std::process::exit(1);
}
