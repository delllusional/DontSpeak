//! Append-only NDJSON file tail with partial-line buffer and truncation recovery.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// Byte-offset tailer for one `updates.jsonl`.
#[derive(Debug)]
pub(crate) struct JsonlTail {
    path: PathBuf,
    offset: u64,
    /// Bytes after the last complete newline (incomplete last line).
    partial: Vec<u8>,
}

impl JsonlTail {
    /// Open existing file and seek to EOF (new sessions: ignore history).
    pub(crate) fn attach_at_eof(path: PathBuf) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let offset = file.seek(SeekFrom::End(0))?;
        Ok(Self {
            path,
            offset,
            partial: Vec::new(),
        })
    }

    /// Open and start at `offset` (tests / reconnect). Clamps when past EOF.
    #[cfg(test)]
    pub(crate) fn attach_at(path: PathBuf, offset: u64) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let len = file.seek(SeekFrom::End(0))?;
        let offset = offset.min(len);
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            path,
            offset,
            partial: Vec::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Read newly appended bytes; return complete NDJSON lines (without trailing `\n`).
    /// On truncation (size < offset): reset to 0 and clear partial buffer.
    pub(crate) fn poll_lines(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Session file vanished — keep offset; next successful open re-attaches.
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let len = file.seek(SeekFrom::End(0))?;
        if len < self.offset {
            // Truncated / recreated — restart from the beginning.
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset && self.partial.is_empty() {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;

        if !self.partial.is_empty() {
            let mut combined = std::mem::take(&mut self.partial);
            combined.extend_from_slice(&buf);
            buf = combined;
        }

        let mut lines = Vec::new();
        let mut start = 0usize;
        for (i, b) in buf.iter().enumerate() {
            if *b == b'\n' {
                let slice = &buf[start..i];
                // Strip optional CR for Windows-written JSONL.
                let slice = slice.strip_suffix(b"\r").unwrap_or(slice);
                if !slice.is_empty() {
                    lines.push(String::from_utf8_lossy(slice).into_owned());
                }
                start = i + 1;
            }
        }
        if start < buf.len() {
            self.partial = buf[start..].to_vec();
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn attach_at_eof_skips_existing_then_reads_appends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "{\"old\":1}\n").unwrap();
        let mut tail = JsonlTail::attach_at_eof(path.clone()).unwrap();
        assert!(tail.poll_lines().unwrap().is_empty());

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, r#"{{"new":2}}"#).unwrap();
        drop(f);
        let lines = tail.poll_lines().unwrap();
        assert_eq!(lines, vec![r#"{"new":2}"#]);
    }

    #[test]
    fn partial_line_buffered_until_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut tail = JsonlTail::attach_at(path.clone(), 0).unwrap();

        std::fs::write(&path, r#"{"partial""#).unwrap();
        // File grew from empty; offset was 0.
        assert!(tail.poll_lines().unwrap().is_empty());
        assert!(!tail.partial.is_empty());

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#":true}"#).unwrap();
        f.write_all(b"\n").unwrap();
        drop(f);
        let lines = tail.poll_lines().unwrap();
        assert_eq!(lines, vec![r#"{"partial":true}"#]);
        assert!(tail.partial.is_empty());
    }

    #[test]
    fn truncation_resets_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, "line-one\nline-two\n").unwrap();
        let mut tail = JsonlTail::attach_at(path.clone(), 0).unwrap();
        assert_eq!(tail.poll_lines().unwrap().len(), 2);
        assert!(tail.offset() > 0);

        // Truncate / rewrite shorter.
        std::fs::write(&path, "fresh\n").unwrap();
        let lines = tail.poll_lines().unwrap();
        assert_eq!(lines, vec!["fresh"]);
    }
}
