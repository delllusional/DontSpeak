//! Persistent streaming linear resampler (mono f32).
//!
//! VPIO negotiates ~48 kHz; synth is 24 kHz; Parakeet wants 16 kHz. Unlike
//! `ds_stt::resample_to_16k` (fresh rubato per call — fine one-shot, wrong for continuous
//! duplex), this keeps phase across chunks so seams don't click. Linear is intentionally
//! cheap; a better streaming resampler can share the same `process()` later.

/// Mono streaming linear resampler `in_rate` → `out_rate`.
pub struct LinearResampler {
    /// Input samples advanced per output sample (`in_rate / out_rate`).
    step: f64,
    /// Fractional position in the current `[prev, cur)` interval.
    pos: f64,
    prev: f32,
    have_prev: bool,
    /// One-pole anti-alias state (downsampling only).
    filtered: f32,
    have_filtered: bool,
    filter_alpha: f32,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_rate = in_rate.max(1);
        let out_rate = out_rate.max(1);
        let step = in_rate as f64 / out_rate as f64;
        // Cutoff just below destination Nyquist; upsampling needs no prefilter.
        let filter_alpha = if step > 1.0 {
            (1.0 - (-std::f64::consts::PI * 0.9 / step).exp()) as f32
        } else {
            1.0
        };
        Self {
            step,
            pos: 0.0,
            prev: 0.0,
            have_prev: false,
            filtered: 0.0,
            have_filtered: false,
            filter_alpha,
        }
    }

    /// Last consumed input sample — seed a replacement resampler without a click.
    /// Used on Windows WASAPI reconnect (`windows.rs`); dead elsewhere outside tests.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn last_sample(&self) -> Option<f32> {
        self.have_prev.then_some(self.prev)
    }

    /// Seed left endpoint from a prior resampler (e.g. after rate-changing reconnect).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn seed_prev(&mut self, prev: f32) {
        self.prev = prev;
        self.have_prev = true;
    }

    /// Resample `input` and append at `out_rate` to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &raw in input {
            let cur = if self.filter_alpha < 1.0 {
                if !self.have_filtered {
                    self.filtered = raw;
                    self.have_filtered = true;
                } else {
                    self.filtered += self.filter_alpha * (raw - self.filtered);
                }
                self.filtered
            } else {
                raw
            };
            if !self.have_prev {
                self.prev = cur;
                self.have_prev = true;
                continue;
            }
            // Emit points in [prev, cur).
            while self.pos < 1.0 {
                let y = self.prev + (cur - self.prev) * self.pos as f32;
                out.push(y);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_len(in_rate: u32, out_rate: u32, n_in: usize) {
        let mut rs = LinearResampler::new(in_rate, out_rate);
        let input: Vec<f32> = (0..n_in).map(|i| i as f32 / n_in as f32).collect();
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        let expected = (n_in as f64 * out_rate as f64 / in_rate as f64) as usize;
        let tol = expected / 20 + 2;
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
        let whole: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut one = LinearResampler::new(48_000, 16_000);
        let mut a = Vec::new();
        one.process(&whole, &mut a);

        let mut split = LinearResampler::new(48_000, 16_000);
        let mut b = Vec::new();
        split.process(&whole[..400], &mut b);
        split.process(&whole[400..], &mut b);

        assert!((a.len() as i64 - b.len() as i64).abs() <= 1);
    }

    #[test]
    fn passthrough_same_rate() {
        let mut rs = LinearResampler::new(16_000, 16_000);
        let input: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut out = Vec::new();
        rs.process(&input, &mut out);
        assert!((out.len() as i64 - 100).abs() <= 2);
    }

    #[test]
    fn seeded_replacement_continues_from_prior_tail() {
        let mut rs = LinearResampler::new(24_000, 48_000);
        rs.seed_prev(0.25);
        let mut out = Vec::new();
        rs.process(&[0.75], &mut out);

        assert_eq!(out, vec![0.25, 0.5]);
        assert_eq!(rs.last_sample(), Some(0.75));
    }
}
