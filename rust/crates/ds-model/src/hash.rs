//! SHA-256 hashing + checksum verification (pure, network-free, unit-tested).

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// Lowercase-hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// Stream a file through SHA-256 (constant memory) and return the lowercase hex,
/// or `None` if it cannot be read.
pub fn sha256_file(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Some(hex(&h.finalize()))
}

/// True iff `path` exists and its streamed SHA-256 equals `expected`
/// (case-insensitive hex compare). An empty `expected` means "skip verification"
/// and only checks existence (fixture/test convenience).
pub fn verify_sha256(path: &Path, expected: &str) -> bool {
    if expected.is_empty() {
        return path.is_file();
    }
    match sha256_file(path) {
        Some(got) => got.eq_ignore_ascii_case(expected.trim()),
        None => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_known_vector() {
        // SHA-256("abc") canonical test vector.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // Empty input.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_sha256_matches_and_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("blob.bin");
        std::fs::write(&p, b"abc").unwrap();
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_sha256(&p, good));
        assert!(!verify_sha256(&p, "deadbeef"));
        // Case-insensitive hex compare.
        assert!(verify_sha256(&p, &good.to_uppercase()));
        // Missing file is never valid (unless expected is empty == existence).
        assert!(!verify_sha256(&dir.path().join("nope.bin"), good));
        // Empty expected == existence-only.
        assert!(verify_sha256(&p, ""));
        assert!(!verify_sha256(&dir.path().join("nope.bin"), ""));
    }
}
