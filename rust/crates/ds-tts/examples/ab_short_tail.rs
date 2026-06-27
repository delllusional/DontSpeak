//! A/B the last-word squeak fix. Synthesizes ONE reply that ends in a short
//! sentence three ways and plays each via `afplay`:
//!
//! 1. WHOLE   — the entire reply as a single synth call (the pre-streaming
//!    "fed the whole step to TTS" path that always sounded fine).
//! 2. OLD     — the previous `stream_batches` (ramped split, no floor/merge):
//!    the trailing "Got it." lands as its own tiny batch → squeak.
//! 3. NEW     — the current `ds_tts::batch::stream_batches` (floor + trailing
//!    merge + strong-boundary preference).
//!
//! ```text
//! cargo run -q --release -p ds-tts --example ab_short_tail
//! ```
use ds_tts::{batch::stream_batches, g2p, synth::KokoroSynth, wav::write_wav16};

/// Thin 24 kHz wrapper over the crate's shared WAV writer ([`ds_tts::wav::write_wav16`]),
/// so this debug example no longer duplicates the encoder.
fn write_wav16_24k(path: &std::path::Path, samples: &[f32]) {
    write_wav16(path, samples, 24_000).unwrap();
}

fn synth_batches(s: &mut KokoroSynth, batches: &[String], voice: &str) -> Vec<f32> {
    let mut out = Vec::new();
    for b in batches {
        if let Ok(p) = s.synthesize(b, voice, 1.0) {
            out.extend_from_slice(&p);
        }
    }
    out
}

fn play(label: &str, path: &std::path::Path) {
    println!("\n▶ playing {label}");
    let _ = std::process::Command::new("afplay").arg(path).status();
}

fn main() {
    let model = ds_model::model_path(ds_model::KOKORO_ONNX_FILE).unwrap();
    let voices = ds_model::model_path(ds_model::KOKORO_VOICES_FILE).unwrap();
    ds_model::set_ort_dylib_path(&ds_model::onnxruntime_dylib_path().unwrap());
    let mb = std::fs::read(&model).unwrap();
    let vb = std::fs::read(&voices).unwrap();
    let mut s = KokoroSynth::load(&mb, &vb).unwrap();
    let voice = "af_sarah";
    println!("provider: {}", s.provider());

    // Split body vs short closer so we can FORCE the failure mode (tail synth'd
    // as its own tiny batch), instead of hoping the old ramp happens to isolate it.
    let body = "I went through the whole diff and the change looks correct to me. \
        The tests cover the new batching, the floor, and the trailing merge, and they all pass. \
        The logic also matches what the reference Kokoro pipelines do.";
    let closer = "Got it.";
    let full_text = format!("{body} {closer}");

    let ph_full = g2p::phonemize_for(&full_text, voice);
    let ph_body = g2p::phonemize_for(body, voice);
    let ph_closer = g2p::phonemize_for(closer, voice);
    println!(
        "closer \"{closer}\" = {} phonemes (floor is below this if isolated)",
        ph_closer.chars().count()
    );

    let new = stream_batches(&ph_full);
    let lens: Vec<usize> = new.iter().map(|b| b.chars().count()).collect();
    println!("NEW: {} batches, phoneme lens = {lens:?}", new.len());

    let dir = std::env::temp_dir();
    let p_whole = dir.join("ab_whole.wav");
    let p_isolated = dir.join("ab_isolated.wav");
    let p_new = dir.join("ab_new.wav");
    let p_alone = dir.join("ab_alone.wav");

    // WHOLE: one synth call over the entire phoneme string (the "whole step" path).
    write_wav16_24k(&p_whole, &s.synthesize(&ph_full, voice, 1.0).unwrap());
    // ISOLATED TAIL: body synth'd, then the closer synth'd as its OWN tiny batch,
    // concatenated — exactly what the old streaming path does when the short final
    // sentence lands alone.
    let mut isolated = s.synthesize(&ph_body, voice, 1.0).unwrap();
    isolated.extend_from_slice(&s.synthesize(&ph_closer, voice, 1.0).unwrap());
    write_wav16_24k(&p_isolated, &isolated);
    // NEW: the fixed batching (tail merged back).
    write_wav16_24k(&p_new, &synth_batches(&mut s, &new, voice));
    // CLOSER ALONE: the tiny utterance by itself — the cleanest test of whether a
    // short isolated batch is what squeaks.
    write_wav16_24k(&p_alone, &s.synthesize(&ph_closer, voice, 1.0).unwrap());

    play("1/4 WHOLE         (baseline, single synth call)", &p_whole);
    play(
        "2/4 ISOLATED TAIL (old failure mode — \"Got it.\" synth'd alone, concatenated)",
        &p_isolated,
    );
    play("3/4 NEW           (floor + trailing merge)", &p_new);
    play("4/4 CLOSER ALONE  (just \"Got it.\" by itself)", &p_alone);
    println!("\nWAVs: {p_whole:?}  {p_isolated:?}  {p_new:?}  {p_alone:?}");
}
