//! CPU vs CoreML synth benchmark. Run both:
//!   cargo run -q --release -p ds-tts --example coreml_bench
//!   DONTSPEAK_COREML=1 cargo run -q --release -p ds-tts --example coreml_bench
use ds_tts::{batch::stream_batches, g2p, synth::KokoroSynth};
use std::time::Instant;

fn main() {
    let model = ds_model::model_path(ds_model::KOKORO_ONNX_FILE).unwrap();
    let voices = ds_model::model_path(ds_model::KOKORO_VOICES_FILE).unwrap();
    ds_model::set_ort_dylib_path(&ds_model::onnxruntime_dylib_path().unwrap());
    let mb = std::fs::read(&model).unwrap();
    let vb = std::fs::read(&voices).unwrap();
    let mut s = KokoroSynth::load(&mb, &vb).unwrap();
    println!("provider: {}", s.provider());
    let voice = "af_sarah";
    let text = "The quick brown fox jumps over the lazy dog. \
        Engine stats are now live, so the realtime factor has something to measure. \
        This sentence exists only to give the synthesizer a representative workload to time.";
    let ph = g2p::phonemize_for(text, voice);
    // Warm up (CoreML compiles the model on first run).
    for b in stream_batches(&ph) {
        let _ = s.synthesize(&b, voice, 1.0);
    }
    for run in 0..3 {
        let t = Instant::now();
        let mut samples = 0usize;
        for b in stream_batches(&ph) {
            samples += s.synthesize(&b, voice, 1.0).unwrap().len();
        }
        let synth_s = t.elapsed().as_secs_f32();
        let audio_s = samples as f32 / 24_000.0;
        println!(
            "run{run}: audio={audio_s:.2}s synth={synth_s:.2}s rtf={:.3} ({:.1}x faster)",
            synth_s / audio_s,
            audio_s / synth_s
        );
    }
}
