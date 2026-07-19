//! Retry-on-transient-`NotFound` file read — shared by every model-file loader.
//!
//! AV/EDR or indexers can make `std::fs::read` on a present model file fail with
//! `NotFound` for tens of ms (stat/open race after the caller already ensured download).
//! Bounded retry absorbs that. Other error kinds fail immediately.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Read fully; retry up to 3× (150ms) on transient `NotFound` only (see module docs).
pub fn read_model_file(path: &Path) -> Result<Vec<u8>, String> {
    read_model_file_with(path, 3, Duration::from_millis(150))
}

/// [`read_model_file`] + UTF-8 — same AV-scan race window for text assets (`tokens.txt`, …).
pub fn read_model_file_to_string(path: &Path) -> Result<String, String> {
    let bytes = read_model_file(path)?;
    String::from_utf8(bytes).map_err(|e| format!("read {}: not valid utf-8: {e}", path.display()))
}

/// Parameterized attempts/delay so tests stay fast.
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
        // Huge delay: would hang if the happy path incorrectly retried.
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
        // Directory-as-file → non-NotFound kind; must not retry.
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
