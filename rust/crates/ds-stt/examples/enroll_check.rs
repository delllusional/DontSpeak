//! On-device check for the enrollment primitive: embed three clips (two of the SAME
//! voice, one different), enroll the first as "Alex", and confirm `match_speaker`
//! recognizes the same voice and rejects the different one. macOS only; needs
//! `SMKOKORO_DYLIB_PATH`.
//!
//!   SMKOKORO_DYLIB_PATH=…/libsmkokoro.dylib cargo run -p ds-stt --example enroll_check -- \
//!     enroll.wav same.wav different.wav

#[cfg(target_os = "macos")]
fn main() {
    use ds_config::speakers::SpeakerStore;
    use ds_stt::diarize::{CoremlDiarizer, Diarizer, cosine, match_speaker};

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 || std::env::var_os("SMKOKORO_DYLIB_PATH").is_none() {
        eprintln!(
            "usage: SMKOKORO_DYLIB_PATH=… enroll_check <enroll.wav> <same.wav> <different.wav>"
        );
        std::process::exit(2);
    }

    let mut d = CoremlDiarizer::new();
    let embed = |d: &mut CoremlDiarizer, path: &str| -> Vec<f32> {
        let r = hound::WavReader::open(path).expect("open wav");
        let spec = r.spec();
        let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
        let mono: Vec<f32> = r
            .into_samples::<i32>()
            .map(|s| s.unwrap_or(0) as f32 / max)
            .collect();
        let pcm = if spec.sample_rate == 16_000 {
            mono
        } else {
            ds_stt::resample_to_16k(&mono, spec.sample_rate)
        };
        d.embed(&pcm).expect("embed")
    };

    let enroll = embed(&mut d, &args[0]);
    let same = embed(&mut d, &args[1]);
    let different = embed(&mut d, &args[2]);
    println!("embedding dim: {}", enroll.len());
    println!(
        "cosine(enroll, same  voice) = {:.3}",
        cosine(&enroll, &same)
    );
    println!(
        "cosine(enroll, other voice) = {:.3}",
        cosine(&enroll, &different)
    );

    let mut store = SpeakerStore::default();
    store.upsert("Alex", enroll);
    let th = 0.5;
    let same_match = match_speaker(&same, &store, th);
    let diff_match = match_speaker(&different, &store, th);
    println!("\nmatch(same  voice, threshold {th}) = {same_match:?}   (expect Some(\"Alex\"))");
    println!("match(other voice, threshold {th}) = {diff_match:?}   (expect None)");

    assert_eq!(
        same_match.as_deref(),
        Some("Alex"),
        "same voice should match the enrolled speaker"
    );
    assert_eq!(diff_match, None, "different voice should NOT match");
    println!("\n✅ enrollment recognizes the same voice and rejects a different one.");
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("enroll_check is macOS-only.");
}
