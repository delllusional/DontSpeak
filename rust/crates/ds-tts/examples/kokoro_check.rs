//! End-to-end check of the ONNX Kokoro backend against the installed pinned model:
//! G2P → session → PCM, reporting the amplitude statistics that distinguish a silent
//! render (all-zero / non-finite samples) from a synthesis or playback fault.
//!
//!   cargo run -q --release -p ds-tts --example kokoro_check -- [text] [voice] [out.wav]
//!
//! Exits non-zero when the run produces no audio or inaudible audio, so it doubles as a
//! manual gate after a model re-pin (a wrong tensor name errors; a wrong dtype or style
//! layout stays silent).

fn main() {
    let mut args = std::env::args().skip(1);
    let text = args
        .next()
        .unwrap_or_else(|| "Kokoro end to end check. One two three four five.".to_string());
    let voice = args.next().unwrap_or_else(|| "af_sarah".to_string());
    let out_wav = args.next();

    let preference = std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".into());
    let want_gpu = ds_config::provider_pref_wants_gpu(&preference);
    match ds_model::ensure_ort_dylib_gpu(want_gpu) {
        Ok(path) => println!("ort dylib: {}", path.display()),
        Err(error) => fail(&format!("ORT dylib unavailable: {error}")),
    }

    // KOKORO_CHECK_MODEL evaluates a candidate export before it is pinned, without
    // disturbing the installed model set.
    let model_path = match std::env::var_os("KOKORO_CHECK_MODEL") {
        Some(path) => std::path::PathBuf::from(path),
        None => ds_model::model_path(ds_model::KOKORO_ONNX_FILE)
            .unwrap_or_else(|| fail("cannot resolve the model dir")),
    };
    let voices_path = ds_model::model_path(ds_model::KOKORO_VOICES_FILE)
        .unwrap_or_else(|| fail("cannot resolve the model dir"));
    println!("model:     {}", model_path.display());
    println!("voices:    {}", voices_path.display());

    let model_bytes = ds_model::read_model_file(&model_path)
        .unwrap_or_else(|error| fail(&format!("read model: {error}")));
    let voices_bytes = ds_model::read_model_file(&voices_path)
        .unwrap_or_else(|error| fail(&format!("read voices: {error}")));

    if std::env::var_os("KOKORO_CHECK_DUMP_IO").is_some() {
        dump_graph_io(&model_bytes);
    }

    let mut synth = ds_tts::synth::KokoroSynth::load(&model_bytes, &voices_bytes)
        .unwrap_or_else(|error| fail(&format!("load: {error}")));
    println!("provider:  {}", synth.provider().as_str());

    // The real path: detect the language, then run the language-aware frontend (English
    // G2P, or espeak / Japanese / Mandarin for the others) rather than assuming English.
    let language = ds_tts::detect_language(&text, ds_config::TtsModel::Kokoro);
    println!("language:  {language}");
    let chunks = ds_tts::g2p::phoneme_batches_for(&text, &voice, &language)
        .unwrap_or_else(|error| fail(&format!("frontend: {error}")));
    let phonemes = chunks
        .iter()
        .map(|chunk| chunk.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if phonemes.trim().is_empty() {
        fail("frontend produced no phonemes");
    }
    println!("phonemes:  {phonemes}");

    let started = std::time::Instant::now();
    let pcm = synth
        .synthesize(&phonemes, &voice, 1.0)
        .unwrap_or_else(|error| fail(&format!("synthesize: {error}")));
    let synth_s = started.elapsed().as_secs_f32();
    if pcm.is_empty() {
        fail("synthesis returned no samples");
    }

    let audio_s = pcm.len() as f32 / ds_tts::SAMPLE_RATE as f32;
    let non_finite = pcm.iter().filter(|s| !s.is_finite()).count();
    let peak = pcm
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .fold(0.0f32, |m, s| m.max(s.abs()));
    let rms = (pcm
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .map(|s| (s * s) as f64)
        .sum::<f64>()
        / pcm.len().max(1) as f64)
        .sqrt();
    println!(
        "samples:   {} ({audio_s:.2}s audio in {synth_s:.2}s, rtf {:.3})",
        pcm.len(),
        synth_s / audio_s.max(0.0001)
    );
    println!("peak:      {peak:.6}");
    println!("rms:       {rms:.6}");
    println!("non-finite:{non_finite}");

    if let Some(path) = out_wav {
        let path = std::path::PathBuf::from(path);
        match ds_tts::wav::write_wav16(&path, &pcm, ds_tts::SAMPLE_RATE) {
            Ok(()) => println!("wrote:     {}", path.display()),
            Err(error) => fail(&format!("write wav: {error}")),
        }
    }

    if non_finite > 0 {
        fail(&format!(
            "{non_finite} non-finite samples — the render would be silent"
        ));
    }
    // 16-bit playback quantizes below ~3e-5; anything under this is inaudible, which is
    // the failure a "speaking but no sound" report actually describes.
    if peak < 0.001 {
        fail(&format!(
            "peak {peak:.6} is inaudible — synthesis ran but produced silence"
        ));
    }
    println!("OK: audible Kokoro audio");
}

/// Graph boundary dtypes. An export whose I/O is float32 while its weights are float16
/// is doing the precision change internally, which is where an overflow would live.
fn dump_graph_io(model_bytes: &[u8]) {
    let session = match ort::session::Session::builder()
        .and_then(|mut builder| builder.commit_from_memory(model_bytes))
    {
        Ok(session) => session,
        Err(error) => fail(&format!("dump load: {error}")),
    };
    for input in session.inputs() {
        println!("input:     {} {:?}", input.name(), input.dtype());
    }
    for output in session.outputs() {
        println!("output:    {} {:?}", output.name(), output.dtype());
    }
}

fn fail(message: &str) -> ! {
    eprintln!("FAIL: {message}");
    std::process::exit(1);
}
