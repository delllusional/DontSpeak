//! 16-bit mono PCM WAV writer — the one encoder shared by `play`, the
//! `ds-helper` bin, and the `ab_short_tail` example.
//!
//! DELIBERATELY hand-rolled, NO dependency: the only WAV we ever write is canonical
//! 16-bit mono PCM (afplay input / listen-dump / A-B debug), so a full crate like
//! `hound` would be more surface than this fixed 44-byte header + clamped samples.

use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Write 16-bit PCM mono WAV at `rate` Hz (f32 samples clamped from [-1, 1]).
///
/// `#[doc(hidden)] pub` (not `pub(crate)`) ONLY so the in-crate `ab_short_tail`
/// example — a separate compilation unit — can call it through the crate; it is
/// not part of the advertised API.
#[doc(hidden)]
pub fn write_wav16(path: &Path, samples: &[f32], rate: u32) -> io::Result<()> {
    let data_len = (samples.len() * 2) as u32; // 16-bit mono
    let mut w = BufWriter::new(std::fs::File::create(path)?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // audio format = PCM
    w.write_all(&1u16.to_le_bytes())?; // channels = mono
    w.write_all(&rate.to_le_bytes())?;
    w.write_all(&(rate * 2).to_le_bytes())?; // byte rate = rate * channels * 2
    w.write_all(&2u16.to_le_bytes())?; // block align = channels * 2
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()
}
