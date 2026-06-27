//! Manual on-device check for the Core ML diarizer: feed a WAV file straight into
//! [`ds_stt::diarize::CoremlDiarizer`] (bypassing the mic) and print the speaker
//! segments. macOS only — needs `SMKOKORO_DYLIB_PATH` pointing at a built
//! `libsmkokoro.dylib`; the Pyannote + WeSpeaker models download on first run.
//!
//!   SMKOKORO_DYLIB_PATH=…/libsmkokoro.dylib \
//!     cargo run -p ds-stt --example diarize_wav -- /path/to/two-speakers.wav

#[cfg(target_os = "macos")]
fn main() {
    use ds_stt::diarize::{CoremlDiarizer, Diarizer};

    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: diarize_wav <file.wav>  (needs SMKOKORO_DYLIB_PATH)");
        std::process::exit(2);
    });
    if std::env::var_os("SMKOKORO_DYLIB_PATH").is_none() {
        eprintln!("SMKOKORO_DYLIB_PATH is not set — point it at a built libsmkokoro.dylib");
        std::process::exit(2);
    }
    // Optional 2nd arg: clustering threshold (0.5–0.9, lower = more speakers); 0 = default.
    let threshold: f32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    // Read the WAV → mono f32, then resample to the 16 kHz mono the diarizer expects.
    let reader = hound::WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let mono = read_mono_f32(reader, spec);
    let pcm16k = if spec.sample_rate == 16_000 {
        mono
    } else {
        ds_stt::resample_to_16k(&mono, spec.sample_rate)
    };
    eprintln!(
        "input: {} Hz, {} ch → {} mono samples @16k ({:.1}s)",
        spec.sample_rate,
        spec.channels,
        pcm16k.len(),
        pcm16k.len() as f64 / 16_000.0
    );

    let mut diarizer = if threshold > 0.0 {
        eprintln!("clustering threshold: {threshold}");
        CoremlDiarizer::with_threshold(threshold)
    } else {
        eprintln!("clustering threshold: FluidAudio default (0.7)");
        CoremlDiarizer::new()
    };

    // Time model-load (preload) and inference separately so the per-call cost is clear.
    let audio_secs = pcm16k.len() as f64 / 16_000.0;
    let t_load = std::time::Instant::now();
    diarizer.preload().expect("preload diarizer models");
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    let t_inf = std::time::Instant::now();
    let out = diarizer.diarize_pcm_16k(&pcm16k);
    let inf_ms = t_inf.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "⏱  model load: {load_ms:.0} ms (one-time)  |  inference: {inf_ms:.0} ms for {audio_secs:.1}s audio  →  {:.1}× realtime",
        audio_secs * 1000.0 / inf_ms
    );
    match out {
        Ok(segments) => {
            let speakers: std::collections::BTreeSet<&str> =
                segments.iter().map(|s| s.speaker.as_str()).collect();
            println!(
                "\n✅ {} speaker(s) across {} segment(s):",
                speakers.len(),
                segments.len()
            );
            for s in &segments {
                println!("  {:>6.2}s – {:>6.2}s  {}", s.start, s.end, s.speaker);
            }
        }
        Err(e) => {
            eprintln!("\n❌ diarize failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Average channels to mono, normalizing the sample type to f32 in [-1, 1].
#[cfg(target_os = "macos")]
fn read_mono_f32(
    reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
    spec: hound::WavSpec,
) -> Vec<f32> {
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .map(|s| s.unwrap_or(0.0))
            .collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.unwrap_or(0) as f32 / max)
                .collect()
        }
    };
    if ch == 1 {
        return interleaved;
    }
    interleaved
        .chunks(ch)
        .map(|frame| frame.iter().sum::<f32>() / ch as f32)
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("diarize_wav is macOS-only (Core ML diarizer).");
}
