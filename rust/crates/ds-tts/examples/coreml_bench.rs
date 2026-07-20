//! CPU vs CoreML synth benchmark. Run both:
//!   cargo run -q --release -p ds-tts --example coreml_bench
//!   DONTSPEAK_PROVIDER=coreml cargo run -q --release -p ds-tts --example coreml_bench
use ds_tts::{batch::stream_batches, g2p, synth::KokoroSynth};
use std::time::Instant;

fn main() {
    let model = ds_model::model_path(ds_model::KOKORO_ONNX_FILE).unwrap();
    let voices = ds_model::model_path(ds_model::KOKORO_VOICES_FILE).unwrap();
    ds_model::set_ort_dylib_path(&ds_model::onnxruntime_dylib_path().unwrap());
    let model_bytes = std::fs::read(&model).unwrap();
    let voice_bytes = std::fs::read(&voices).unwrap();
    let mut synth = KokoroSynth::load(&model_bytes, &voice_bytes).unwrap();
    println!("provider: {}", synth.provider());
    let voice = "af_sarah";
    let text = "The quick brown fox jumps over the lazy dog. \
        Engine stats are now live, so the realtime factor has something to measure. \
        This sentence exists only to give the synthesizer a representative workload to time.";
    let phonemes = g2p::phonemize_for(text, voice);
    // Warm up because Core ML compiles the model on first use.
    for batch in stream_batches(&phonemes) {
        let _ = synth.synthesize(&batch, voice, 1.0);
    }
    for run in 0..3 {
        let started = Instant::now();
        let mut samples = 0usize;
        for batch in stream_batches(&phonemes) {
            samples += synth.synthesize(&batch, voice, 1.0).unwrap().len();
        }
        let synth_seconds = started.elapsed().as_secs_f32();
        let audio_seconds = samples as f32 / 24_000.0;
        println!(
            "run{run}: audio={audio_seconds:.2}s synth={synth_seconds:.2}s rtf={:.3} ({:.1}x faster)",
            synth_seconds / audio_seconds,
            audio_seconds / synth_seconds
        );
    }
}
