//! 16-bit mono PCM WAV writer — shared by `play`, `ds-helper`, and `ab_short_tail`.
//!
//! Hand-rolled, no dep: we only ever write this fixed 44-byte header + clamped
//! samples (afplay / listen-dump / A-B). `hound` would be more surface than value.

use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Write 16-bit PCM mono WAV at `rate` Hz (f32 clamped [-1, 1]).
///
/// `#[doc(hidden)] pub` only so the in-crate `ab_short_tail` example (separate
/// compilation unit) can call it — not advertised API.
#[doc(hidden)]
pub fn write_wav16(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * 2).to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}
