//! Validate the Rust Parakeet STT path: load a 16 kHz mono WAV and transcribe it via
//! `LocalTranscriber::for_provider("ane", ..)` → the FluidAudio Core ML shim.
//!
//!   SMKOKORO_DYLIB_PATH=/path/to/libsmkokoro.dylib \
//!     cargo run -p ds-stt --example parakeet_check -- some_16k.wav
use std::path::PathBuf;

fn load_wav_16k(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("read wav");
    // Scan chunks for "data", read int16 PCM → f32.
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let csz =
            u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        if &bytes[i..i + 4] == b"data" {
            let pcm = &bytes[i + 8..i + 8 + csz];
            return pcm
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
                .collect();
        }
        i += 8 + csz + (csz & 1);
    }
    panic!("no data chunk in {path}");
}

fn main() {
    let wav = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "warm16k.wav".into());
    let samples = load_wav_16k(&wav);
    println!(
        "loaded {wav}: {} samples ({:.2}s)",
        samples.len(),
        samples.len() as f32 / 16_000.0
    );

    let mut t = ds_stt::LocalTranscriber::for_provider("ane", PathBuf::new());
    match t.transcribe_pcm_16k(&samples) {
        Ok(text) => println!("TRANSCRIPT: {text}"),
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}
