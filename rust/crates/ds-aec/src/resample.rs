//! A tiny persistent streaming linear resampler (mono f32).
//!
//! VPIO negotiates its own rate (~48 kHz); we synth at 24 kHz and Parakeet wants
//! 16 kHz, so render and capture each cross a rate. Unlike `ds_stt::resample_to_16k`
//! (which builds a fresh rubato resampler PER CALL — fine for a one-shot stop, but
//! wrong for a continuous duplex loop), this keeps its phase across calls so it can
//! be fed arbitrary-length chunks tick after tick with no clicks at the seams.
//!
//! Linear interpolation is intentionally cheap. M1/M2 use it to prove the path; a
//! higher-quality (rubato/polyphase) streaming resampler can drop in behind the
//! same `process()` signature later if the reference-signal quality ever matters.

/// Mono streaming linear resampler from `in_rate` to `out_rate`. Carries the
/// previous input sample and the fractional read position across `process` calls.
pub struct LinearResampler {
    /// Input samples advanced per output sample (`in_rate / out_rate`).
    step: f64,
    /// Fractional position within the current `[prev, cur)` input interval.
    pos: f64,
    /// The previous input sample (left endpoint of the interpolation interval).
    prev: f32,
    /// False until the first input sample seeds `prev`.
    have_prev: bool,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        Self {
            step: in_rate as f64 / out_rate as f64,
            pos: 0.0,
            prev: 0.0,
            have_prev: false,
        }
    }

    /// Resample `input` (at `in_rate`) and append the result (at `out_rate`) to
    /// `out`. Emits every output sample whose position falls in a consumed input
    /// interval, interpolating linearly between the bracketing input samples.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &cur in input {
            if !self.have_prev {
                self.prev = cur;
                self.have_prev = true;
                continue;
            }
            // Emit all output points lying in [prev, cur): pos is the fraction
            // between prev (0.0) and cur (1.0).
            while self.pos < 1.0 {
                let y = self.prev + (cur - self.prev) * self.pos as f32;
                out.push(y);
                self.pos += self.step;
            }
            self.pos -= 1.0; // consumed one input interval
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output length tracks the rate ratio (within a small edge tolerance).
    fn approx_len(in_rate: u32, out_rate: u32, n_in: usize) {
        let mut rs = LinearResampler::new(in_rate, out_rate);
        let input: Vec<f32> = (0..n_in).map(|i| i as f32 / n_in as f32).collect();
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        let expected = (n_in as f64 * out_rate as f64 / in_rate as f64) as usize;
        let tol = expected / 20 + 2; // ~5% + a couple priming samples
        let got = out.len() as i64;
        assert!(
            (got - expected as i64).unsigned_abs() as usize <= tol,
            "{in_rate}->{out_rate}: got {got} expected ~{expected} (±{tol})"
        );
    }

    #[test]
    fn upsample_24k_to_48k_doubles() {
        approx_len(24_000, 48_000, 1000);
    }

    #[test]
    fn downsample_48k_to_16k_thirds() {
        approx_len(48_000, 16_000, 3000);
    }

    #[test]
    fn streaming_matches_one_shot_length() {
        // Feeding the same data in two chunks yields the same total as one chunk.
        let whole: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut one = LinearResampler::new(48_000, 16_000);
        let mut a = Vec::new();
        one.process(&whole, &mut a);

        let mut split = LinearResampler::new(48_000, 16_000);
        let mut b = Vec::new();
        split.process(&whole[..400], &mut b);
        split.process(&whole[400..], &mut b);

        // Same phase carried across the seam ⇒ identical length (±1 boundary).
        assert!((a.len() as i64 - b.len() as i64).abs() <= 1);
    }

    #[test]
    fn passthrough_same_rate() {
        let mut rs = LinearResampler::new(16_000, 16_000);
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        // 1:1 ratio: ~one output per input (minus the single priming sample).
        assert!((out.len() as i64 - 100).abs() <= 2);
    }
}
