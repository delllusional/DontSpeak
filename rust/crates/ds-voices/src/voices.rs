//! Kokoro voices `.npz` parser + style-row indexing (port of the Kokoro npz parser
//! and `styleFor`/style-row math).
//!
//! `voices-v1.0.bin` is a NumPy `.npz` — an UNCOMPRESSED zip of `<voice>.npy`
//! entries, each little-endian `'<f4'` (f32) of shape (510, 1, 256), i.e. 510*256
//! contiguous floats per voice. We read the zip central directory (entries may use
//! streaming local headers, so sizes there can be 0 — we trust the central directory)
//! and the npy v1/v2 header, with no zip crate.
//!
//! No network, no audio: pure byte parsing + a slice index, unit-tested against a
//! hand-built fixture npz.

use std::collections::HashMap;

/// Parse a Kokoro voices npz into `voice name → 510*256 f32 style array`.
/// Error string on malformed file (callers fail-quiet / log).
pub fn parse_voices_npz(bytes: &[u8]) -> Result<HashMap<String, Vec<f32>>, String> {
    let mut voices = HashMap::new();
    let eocd = find_eocd(bytes)?;
    let count = u16(bytes, eocd + 10)? as usize;
    let mut offset = u32(bytes, eocd + 16)? as usize;
    for _ in 0..count {
        if u32(bytes, offset)? != 0x0201_4b50 {
            return Err("bad central directory entry".into());
        }
        let method = u16(bytes, offset + 10)?;
        let size = u32(bytes, offset + 24)? as usize;
        let name_len = u16(bytes, offset + 28)? as usize;
        let extra_len = u16(bytes, offset + 30)? as usize;
        let comment_len = u16(bytes, offset + 32)? as usize;
        let local_offset = u32(bytes, offset + 42)? as usize;
        let name = decode_str(bytes, offset + 46, offset + 46 + name_len)?;
        if method != 0 {
            return Err(format!(
                "voices npz entry {name} is compressed; expected stored"
            ));
        }
        // Local header sizes may be 0 (streaming); trust the central directory.
        let local_name_len = u16(bytes, local_offset + 26)? as usize;
        let local_extra_len = u16(bytes, local_offset + 28)? as usize;
        let data_start = local_offset + 30 + local_name_len + local_extra_len;
        let arr = parse_npy_floats(bytes, data_start, size)?;
        let key = name.strip_suffix(".npy").unwrap_or(&name).to_string();
        voices.insert(key, arr);
        offset += 46 + name_len + extra_len + comment_len;
    }
    Ok(voices)
}

/// Voice names in a voices npz (picker), without retaining the arrays.
pub fn voice_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = parse_voices_npz(bytes)?.into_keys().collect();
    names.sort();
    Ok(names)
}

/// 256-float style row for a token count (port of Kokoro
/// `style.copyOfRange(tokens.size * 256, (tokens.size + 1) * 256)` /
/// kokoro-onnx `_create_audio`). `style` is the 510*256 per-voice array;
/// `token_count` is UNPADDED (before bos/eos). Errors if out of range
/// (token count ≥ 510 or short array).
pub fn style_row(style: &[f32], token_count: usize) -> Result<Vec<f32>, String> {
    let start = token_count * 256;
    let end = start + 256;
    if end > style.len() {
        return Err(format!(
            "style row {token_count} out of range (need {end}, have {})",
            style.len()
        ));
    }
    Ok(style[start..end].to_vec())
}

// ── npy + zip byte helpers (port of Npz.kt private fns) ──────────────────────

fn find_eocd(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() < 22 {
        return Err("file too small to be a zip/npz".into());
    }
    let lo = bytes.len().saturating_sub(22 + 65535);
    let mut i = bytes.len() - 22;
    loop {
        if u32(bytes, i)? == 0x0605_4b50 {
            return Ok(i);
        }
        if i == lo {
            break;
        }
        i -= 1;
    }
    Err("not a zip/npz file (no end-of-central-directory record)".into())
}

/// Validate npy header at `start` and return `[from, to)` of the LE f32 payload.
/// `size` is the stored entry size from the zip central directory. Shared by
/// float-decoding [`parse_npy_floats`].
fn npy_payload_range(bytes: &[u8], start: usize, size: usize) -> Result<(usize, usize), String> {
    if start + 10 > bytes.len() {
        return Err("npy entry truncated".into());
    }
    if bytes[start] != 0x93 || decode_str(bytes, start + 1, start + 6)? != "NUMPY" {
        return Err("bad npy magic".into());
    }
    let major = bytes[start + 6] as usize;
    let (header_len, header_field) = if major == 1 {
        (u16(bytes, start + 8)? as usize, 10usize)
    } else {
        (u32(bytes, start + 8)? as usize, 12usize)
    };
    let header_end = start + header_field + header_len;
    let header = decode_str(bytes, start + header_field, header_end)?;
    if !header.contains("'<f4'") {
        return Err(format!("expected little-endian float32 npy, got: {header}"));
    }
    if !header.contains("'fortran_order': False") {
        return Err("expected C-order npy".into());
    }
    let data_end = start + size;
    if data_end > bytes.len() || header_end > data_end {
        return Err("npy data region out of bounds".into());
    }
    Ok((header_end, data_end))
}

fn parse_npy_floats(bytes: &[u8], start: usize, size: usize) -> Result<Vec<f32>, String> {
    let (data_start, data_end) = npy_payload_range(bytes, start, size)?;
    let float_count = (data_end - data_start) / 4;
    let mut out = Vec::with_capacity(float_count);
    for i in 0..float_count {
        let b = data_start + i * 4;
        out.push(f32::from_le_bytes([
            bytes[b],
            bytes[b + 1],
            bytes[b + 2],
            bytes[b + 3],
        ]));
    }
    Ok(out)
}

fn u16(b: &[u8], i: usize) -> Result<u32, String> {
    if i + 2 > b.len() {
        return Err("u16 read out of bounds".into());
    }
    Ok((b[i] as u32) | ((b[i + 1] as u32) << 8))
}

fn u32(b: &[u8], i: usize) -> Result<u32, String> {
    if i + 4 > b.len() {
        return Err("u32 read out of bounds".into());
    }
    Ok((b[i] as u32)
        | ((b[i + 1] as u32) << 8)
        | ((b[i + 2] as u32) << 16)
        | ((b[i + 3] as u32) << 24))
}

fn decode_str(b: &[u8], start: usize, end: usize) -> Result<String, String> {
    if end > b.len() || start > end {
        return Err("string slice out of bounds".into());
    }
    Ok(String::from_utf8_lossy(&b[start..end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal STORED zip with one `.npy` entry of LE f32 floats + v1 npy header.
    /// Exercises the exact layout parse_voices_npz reads (no zip crate, no network).
    fn build_npz(entry_name: &str, floats: &[f32]) -> Vec<u8> {
        let mut npy = Vec::new();
        npy.extend_from_slice(&[0x93]);
        npy.extend_from_slice(b"NUMPY");
        npy.push(1); // major
        npy.push(0); // minor
        let shape = format!("({}, 1, 256)", floats.len() / 256);
        let mut header = format!("{{'descr': '<f4', 'fortran_order': False, 'shape': {shape}, }}");
        // numpy pads so (10 + header_len) % 64 == 0, ending '\n'.
        let unpadded = 10 + header.len() + 1;
        let pad = (64 - (unpadded % 64)) % 64;
        header.push_str(&" ".repeat(pad));
        header.push('\n');
        let hlen = header.len() as u16;
        npy.extend_from_slice(&hlen.to_le_bytes());
        npy.extend_from_slice(header.as_bytes());
        for f in floats {
            npy.extend_from_slice(&f.to_le_bytes());
        }

        let name = format!("{entry_name}.npy");
        let name_bytes = name.as_bytes();
        let mut zip = Vec::new();

        let local_offset = zip.len() as u32;
        zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes()); // method = stored
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes()); // crc32 unused by reader
        zip.extend_from_slice(&(npy.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(npy.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(name_bytes);
        let data_at = zip.len();
        zip.extend_from_slice(&npy);

        let cd_offset = zip.len() as u32;
        zip.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&20u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&(npy.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(npy.len() as u32).to_le_bytes());
        zip.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u32.to_le_bytes());
        zip.extend_from_slice(&local_offset.to_le_bytes());
        zip.extend_from_slice(name_bytes);
        let cd_size = zip.len() as u32 - cd_offset;

        zip.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());
        zip.extend_from_slice(&1u16.to_le_bytes());
        zip.extend_from_slice(&1u16.to_le_bytes());
        zip.extend_from_slice(&cd_size.to_le_bytes());
        zip.extend_from_slice(&cd_offset.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes());

        debug_assert_eq!(data_at, local_offset as usize + 30 + name_bytes.len());
        zip
    }

    #[test]
    fn parses_one_voice_with_510x256_floats() {
        // Fill row r col 0 with r so style_row indexing can be verified.
        let mut floats = vec![0.0f32; 510 * 256];
        for r in 0..510 {
            floats[r * 256] = r as f32;
            floats[r * 256 + 255] = (r as f32) + 0.5;
        }
        let npz = build_npz("af_test", &floats);
        let voices = parse_voices_npz(&npz).expect("npz parses");
        assert_eq!(voices.len(), 1);
        let arr = voices.get("af_test").expect("voice present by stem name");
        assert_eq!(arr.len(), 510 * 256);
        assert_eq!(arr[0], 0.0);
        assert_eq!(arr[3 * 256], 3.0);
    }

    #[test]
    fn voice_names_strips_npy_suffix() {
        let floats = vec![0.0f32; 510 * 256];
        let npz = build_npz("bm_george", &floats);
        let names = voice_names(&npz).unwrap();
        assert_eq!(names, vec!["bm_george".to_string()]);
    }

    #[test]
    fn style_row_indexes_by_token_count() {
        let mut style = vec![0.0f32; 510 * 256];
        for r in 0..510 {
            for c in 0..256 {
                style[r * 256 + c] = (r * 1000 + c) as f32;
            }
        }
        let row5 = style_row(&style, 5).unwrap();
        assert_eq!(row5.len(), 256);
        assert_eq!(row5[0], 5000.0);
        assert_eq!(row5[255], 5255.0);
        assert_eq!(style_row(&style, 0).unwrap()[0], 0.0);
        assert!(style_row(&style, 509).is_ok());
        // 510 is out of range (rows are 0..=509).
        assert!(style_row(&style, 510).is_err());
    }

    #[test]
    fn rejects_non_zip() {
        assert!(parse_voices_npz(b"not a zip at all").is_err());
        assert!(parse_voices_npz(&[]).is_err());
    }
}
