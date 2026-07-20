//! Mono WAV writer + reader — shared by `play`, `ds-helper`, `ab_short_tail`, and the
//! Chatterbox reference-voice loader.
//!
//! Hand-rolled, no dep: we only write the fixed 44-byte 16-bit header, and only read
//! mono PCM16 / float32 (the pinned reference clips). `hound` would be more surface
//! than value.

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

/// Read a mono WAV as `(sample_rate, f32 samples)`. PCM16 (format 1) and IEEE float32
/// (format 3) only — the shapes the pinned Chatterbox reference voices use.
pub fn read_wav_mono_f32(path: &Path) -> Result<(u32, Vec<f32>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_wav_mono_f32(&bytes)
}

/// Pure parser behind [`read_wav_mono_f32`]; walks RIFF chunks, skipping unknown ones.
pub fn parse_wav_mono_f32(bytes: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let err = |m: &str| Err(m.to_string());
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return err("not a RIFF/WAVE file");
    }
    let u16le = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (format, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    let mut off = 12usize;
    while off + 8 <= bytes.len() {
        let id = &bytes[off..off + 4];
        let size = u32le(off + 4) as usize;
        let body = off + 8;
        if body + size > bytes.len() {
            return err("truncated WAV chunk");
        }
        match id {
            b"fmt " if size >= 16 => {
                fmt = Some((
                    u16le(body),
                    u16le(body + 2),
                    u32le(body + 4),
                    u16le(body + 14),
                ));
            }
            b"data" => data = Some(&bytes[body..body + size]),
            _ => {}
        }
        // Chunks are word-aligned: odd sizes carry a pad byte.
        off = body + size + (size % 2);
    }
    let (format, channels, rate, bits) = fmt.ok_or("WAV has no fmt chunk")?;
    let data = data.ok_or("WAV has no data chunk")?;
    if channels != 1 {
        return Err(format!("expected mono WAV, got {channels} channels"));
    }
    let samples = match (format, bits) {
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        _ => return Err(format!("unsupported WAV format {format}/{bits}-bit")),
    };
    Ok((rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_round_trips_through_writer_and_parser() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rt.wav");
        let samples = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        write_wav16(&path, &samples, 24_000).unwrap();
        let (rate, back) = read_wav_mono_f32(&path).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(back.len(), samples.len());
        for (a, b) in samples.iter().zip(&back) {
            assert!((a - b).abs() < 2.0 / 32768.0, "{a} vs {b}");
        }
    }

    #[test]
    fn float32_wav_with_extra_chunks_parses() {
        // Mirror the pinned default_voice.wav's shape: fmt(3) + fact + PEAK + data.
        let samples = [0.25f32, -0.75];
        let mut data = Vec::new();
        for s in samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        let riff_len = 4 + (8 + 16) + (8 + 4) + (8 + data.len());
        wav.extend_from_slice(&(riff_len as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&24_000u32.to_le_bytes());
        wav.extend_from_slice(&96_000u32.to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&32u16.to_le_bytes());
        wav.extend_from_slice(b"fact");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(&2u32.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);

        let (rate, back) = parse_wav_mono_f32(&wav).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(back, samples);
    }

    #[test]
    fn stereo_and_garbage_are_rejected() {
        assert!(parse_wav_mono_f32(b"not a wav").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.wav");
        write_wav16(&path, &[0.1], 8_000).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[22] = 2; // channels = 2
        assert!(parse_wav_mono_f32(&bytes).is_err());
    }
}
