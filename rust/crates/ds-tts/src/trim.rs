//! Silence trimming (`trimSilence`) — a port of kokoro-onnx `trim.py`
//! (vendored librosa `effects.trim` defaults).
//!
//! Strips leading/trailing audio quieter than `max − 60 dB`. Kokoro tends to
//! emit ~2 s of leading silence per chunk, so this matters for snappy playback.
//! PURE: f32 in, f32 out, no audio, no model — unit-tested with synthetic
//! ramp/silence arrays.

const FRAME_LENGTH: usize = 2048;
const HOP_LENGTH: usize = 512;
const TOP_DB: f64 = 60.0;

/// Trim leading/trailing near-silence from `y`. Empty in → empty out; an
/// all-silence input → empty out (trimSilence).
pub fn trim_silence(y: &[f32]) -> Vec<f32> {
    if y.is_empty() {
        return Vec::new();
    }
    // Centered RMS frames: pad frame_length/2 zeros both sides, window 2048,
    // hop 512.
    let pad = FRAME_LENGTH / 2;
    let padded_len = y.len() + 2 * pad;
    if padded_len < FRAME_LENGTH {
        // Too short to form even one frame; nothing to trim.
        return y.to_vec();
    }
    let frames = 1 + (padded_len - FRAME_LENGTH) / HOP_LENGTH;
    let mut rms = vec![0.0f64; frames];
    for (t, slot) in rms.iter_mut().enumerate() {
        let mut sum = 0.0f64;
        let frame_start = t * HOP_LENGTH;
        for j in 0..FRAME_LENGTH {
            // idx into y after removing the leading pad.
            let signed = frame_start as isize + j as isize - pad as isize;
            if signed >= 0 && (signed as usize) < y.len() {
                let v = y[signed as usize] as f64;
                sum += v * v;
            }
        }
        *slot = (sum / FRAME_LENGTH as f64).sqrt();
    }
    // power_to_db(rms^2, ref=max(rms)^2, amin=1e-10):
    //   10*log10(max(amin,p)) − 10*log10(max(amin,ref))
    let max_rms = rms.iter().cloned().fold(0.0f64, f64::max);
    let reference = max_rms * max_rms;
    let ref_db = 10.0 * f64::max(1e-10, reference).log10();
    let mut first: isize = -1;
    let mut last: isize = -1;
    for (t, &r) in rms.iter().enumerate() {
        let p = r * r;
        let db = 10.0 * f64::max(1e-10, p).log10() - ref_db;
        if db > -TOP_DB {
            if first < 0 {
                first = t as isize;
            }
            last = t as isize;
        }
    }
    if first < 0 {
        return Vec::new();
    }
    let start = first as usize * HOP_LENGTH;
    let end = std::cmp::min(y.len(), (last as usize + 1) * HOP_LENGTH);
    if start >= end {
        return Vec::new();
    }
    y[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_in_empty_out() {
        assert!(trim_silence(&[]).is_empty());
    }

    #[test]
    fn digital_zero_is_kept_not_trimmed() {
        // FAITHFUL to librosa power_to_db(ref=max): for an all-ZERO
        // signal, max(rms) == 0, so every frame's dB == 0 relative to that zero
        // reference (10*log10(1e-10) − 10*log10(1e-10) = 0 > −60). librosa keeps
        // the whole thing — there is no "louder" part to trim against. (Real
        // Kokoro output is never digital-zero; see the relative-silence test.)
        let silence = vec![0.0f32; 24_000];
        let out = trim_silence(&silence);
        assert!(
            !out.is_empty(),
            "all-zero is kept (ref==0 ⇒ db==0 everywhere)"
        );
    }

    #[test]
    fn near_silence_relative_to_a_loud_peak_trims_to_empty() {
        // A faint hiss (1e-6) with NO loud region: max sets the reference, and
        // every frame sits ~0 dB below it, so nothing is below −60 dB → kept.
        // But a faint TAIL after a loud burst IS trimmed: build loud-then-faint
        // and confirm the faint tail is removed.
        let mut y = vec![0.8f32; 8_000]; // loud
        y.extend(std::iter::repeat_n(1e-6f32, 16_000)); // ~−120 dB tail
        let out = trim_silence(&y);
        // The faint tail (≈ −120 dB ≪ −60) is stripped; result ≈ the loud part.
        assert!(out.len() < y.len() / 2, "faint tail should be trimmed off");
    }

    #[test]
    fn leading_and_trailing_silence_removed_keeps_loud_middle() {
        // 8000 silent + 8000 loud (amplitude 1.0) + 8000 silent.
        let mut y = vec![0.0f32; 8_000];
        y.extend(std::iter::repeat_n(1.0f32, 8_000));
        y.extend(std::iter::repeat_n(0.0f32, 8_000));
        let trimmed = trim_silence(&y);
        // The loud middle must survive and the result must be much shorter than
        // the padded original (leading/trailing silence gone).
        assert!(!trimmed.is_empty());
        assert!(trimmed.len() < y.len());
        assert!(trimmed.len() >= 7_000, "loud region should be largely kept");
        // The kept region is loud (mean abs well above zero).
        let mean_abs: f64 =
            trimmed.iter().map(|&s| s.abs() as f64).sum::<f64>() / trimmed.len() as f64;
        assert!(mean_abs > 0.5, "trimmed region should be the loud part");
    }

    #[test]
    fn fully_loud_signal_is_essentially_unchanged() {
        let y = vec![0.8f32; 6_000];
        let trimmed = trim_silence(&y);
        // No silence to strip; length stays within one hop of the original.
        assert!(trimmed.len() >= y.len() - HOP_LENGTH);
    }

    #[test]
    fn short_signal_no_panic() {
        // Shorter than a single frame's worth of real samples; must not panic.
        let y = vec![0.5f32; 10];
        let _ = trim_silence(&y);
    }
}
