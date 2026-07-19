//! SHA-256 hashing + checksum verification (pure, network-free, unit-tested).

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// Stream a file through SHA-256 (constant memory). `None` if unreadable.
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

/// Streamed SHA-256 equals `expected` (case-insensitive). Empty `expected` always false —
/// production must use an explicit trust path, never silent bypass.
pub fn verify_sha256(path: &Path, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    match sha256_file(path) {
        Some(got) => got.eq_ignore_ascii_case(expected.trim()),
        None => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    len: u64,
    modified: std::time::SystemTime,
}

#[derive(Debug)]
struct CachedVerification {
    identity: FileIdentity,
    expected: String,
    valid: bool,
}

type VerificationCache = std::collections::HashMap<PathBuf, CachedVerification>;

fn file_identity(path: &Path) -> Option<FileIdentity> {
    let metadata = path.metadata().ok()?;
    metadata.is_file().then_some(FileIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok()?,
    })
}

fn verification_cache() -> &'static std::sync::Mutex<VerificationCache> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<VerificationCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Verify a model file once per `(path, size, mtime, expected digest)` state.
/// Presence probes call this frequently; hashing the same multi-hundred-megabyte
/// model on every status poll provides no additional integrity signal while the
/// file metadata is unchanged.
pub(crate) fn verify_sha256_cached(path: &Path, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(before) = file_identity(path) else {
        return false;
    };
    let normalized_expected = expected.trim().to_ascii_lowercase();
    if let Some(cached) = verification_cache().lock().unwrap().get(path)
        && cached.identity == before
        && cached.expected == normalized_expected
    {
        return cached.valid;
    }

    let valid = verify_sha256(path, &normalized_expected);
    let Some(after) = file_identity(path) else {
        return false;
    };
    if before != after {
        return false;
    }
    verification_cache().lock().unwrap().insert(
        path.to_path_buf(),
        CachedVerification {
            identity: after,
            expected: normalized_expected,
            valid,
        },
    );
    valid
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
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
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
        assert!(verify_sha256(&p, &good.to_uppercase()));
        // Empty expected must never verify (see `verify_sha256` contract).
        assert!(!verify_sha256(&dir.path().join("nope.bin"), good));
        assert!(!verify_sha256(&p, ""));
        assert!(!verify_sha256(&dir.path().join("nope.bin"), ""));
    }

    #[test]
    fn cached_verification_invalidates_when_file_metadata_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.bin");
        std::fs::write(&path, b"abc").unwrap();
        let digest = sha256_hex(b"abc");

        assert!(verify_sha256_cached(&path, &digest));
        let cached_identity = file_identity(&path).unwrap();
        let cache = verification_cache().lock().unwrap();
        assert_eq!(cache.get(&path).unwrap().identity, cached_identity);
        drop(cache);

        std::fs::write(&path, b"different length").unwrap();
        assert!(!verify_sha256_cached(&path, &digest));
        assert_ne!(file_identity(&path).unwrap(), cached_identity);
    }
}
