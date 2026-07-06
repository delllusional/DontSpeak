//! Retry-on-transient-`NotFound` file read — shared by every model-file loader.
//!
//! A momentary external actor (AV/EDR scan, indexer) can make `std::fs::read` on an
//! existing, correctly-sized model file spuriously fail with `NotFound` (os error 2)
//! for a few dozen milliseconds. This is NOT a missing-file condition (the caller
//! already established the file is present/downloaded) — it's a transient stat/open
//! race, so a bounded retry clears it without the caller ever seeing an error. Any
//! OTHER error kind (permissions, "is a directory", genuine corruption downstream)
//! is NOT retried — it fails immediately, since retrying it would only delay a real
//! failure by up to `attempts * delay`.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Read `path` fully, retrying up to 3 times (150ms apart) on a transient `NotFound`.
/// See the module docs for why `NotFound` specifically is retried and nothing else.
pub fn read_model_file(path: &Path) -> Result<Vec<u8>, String> {
    read_model_file_with(path, 3, Duration::from_millis(150))
}

/// [`read_model_file`], then decode as UTF-8 — for text model assets (e.g. a tokenizer's
/// `tokens.txt`) that sit in the exact same AV-scan-race window as the binary model files,
/// but are consumed as `String` rather than raw bytes.
pub fn read_model_file_to_string(path: &Path) -> Result<String, String> {
    let bytes = read_model_file(path)?;
    String::from_utf8(bytes).map_err(|e| format!("read {}: not valid utf-8: {e}", path.display()))
}

/// The retry loop, parameterized by attempt count + delay so tests run fast and
/// deterministic instead of waiting on the real production timing.
pub(crate) fn read_model_file_with(
    path: &Path,
    attempts: u32,
    delay: Duration,
) -> Result<Vec<u8>, String> {
    let mut remaining = attempts.max(1);
    loop {
        match std::fs::read(path) {
            Ok(bytes) => return Ok(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound && remaining > 1 => {
                remaining -= 1;
                std::thread::sleep(delay);
            }
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_until_the_file_appears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appears-late.bin");
        let path2 = path.clone();
        let handle = std::thread::spawn(move || {
            // Give the reader a couple of NotFound attempts before the file lands.
            std::thread::sleep(Duration::from_millis(80));
            std::fs::write(&path2, b"hello").unwrap();
        });
        let bytes = read_model_file_with(&path, 5, Duration::from_millis(30))
            .expect("should eventually succeed once the file appears");
        handle.join().unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn gives_up_after_exhausting_attempts_if_the_file_never_appears() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-appears.bin");
        let err = read_model_file_with(&path, 3, Duration::from_millis(1))
            .expect_err("must error once attempts are exhausted");
        assert!(err.contains("never-appears.bin"), "{err}");
    }

    #[test]
    fn succeeds_on_first_try_with_no_delay_when_file_is_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("present.bin");
        std::fs::write(&path, b"already here").unwrap();
        let t0 = std::time::Instant::now();
        // A huge delay would make this test take forever if a retry were incorrectly
        // triggered on the happy path.
        let bytes = read_model_file_with(&path, 3, Duration::from_secs(5)).expect("first try ok");
        assert_eq!(bytes, b"already here");
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "must not have slept at all on the happy path"
        );
    }

    #[test]
    fn a_non_not_found_error_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();
        // Reading a directory as a file gives a distinct, non-NotFound io error kind
        // (e.g. `IsADirectory`/`PermissionDenied` depending on platform) — must NOT retry.
        let t0 = std::time::Instant::now();
        let err = read_model_file_with(dir.path(), 3, Duration::from_secs(5))
            .expect_err("reading a directory as a file must fail");
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "a non-NotFound error must fail immediately on the first attempt, no retry sleep"
        );
        assert!(err.contains(&dir.path().display().to_string()), "{err}");
    }
}
