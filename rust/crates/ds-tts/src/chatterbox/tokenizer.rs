//! Thin wrapper over the HF `tokenizers` runtime for Chatterbox's pinned
//! `tokenizer.json`. Special tokens are added per the file's own post-processor,
//! mirroring the reference pipeline.

use std::path::Path;

pub struct ChatterboxTokenizer {
    inner: tokenizers::Tokenizer,
}

impl ChatterboxTokenizer {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        tokenizers::Tokenizer::from_file(path)
            .map(|inner| Self { inner })
            .map_err(|e| format!("chatterbox tokenizer load: {e}"))
    }

    /// Text → i64 ids for the `embed_tokens` graph.
    pub fn encode_ids(&self, text: &str) -> Result<Vec<i64>, String> {
        let enc = self
            .inner
            .encode(text, true)
            .map_err(|e| format!("chatterbox tokenize: {e}"))?;
        Ok(enc.get_ids().iter().map(|&id| id as i64).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny handcrafted tokenizer.json — never the real 3.5 MB pin.
    const FIXTURE: &str = r#"{
        "version": "1.0",
        "truncation": null,
        "padding": null,
        "added_tokens": [],
        "normalizer": { "type": "Lowercase" },
        "pre_tokenizer": { "type": "Whitespace" },
        "post_processor": null,
        "decoder": null,
        "model": {
            "type": "WordLevel",
            "vocab": { "hello": 0, "world": 1, "[UNK]": 2 },
            "unk_token": "[UNK]"
        }
    }"#;

    #[test]
    fn loads_a_fixture_and_encodes_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokenizer.json");
        std::fs::write(&path, FIXTURE).unwrap();
        let tok = ChatterboxTokenizer::from_file(&path).expect("fixture loads");
        assert_eq!(tok.encode_ids("Hello world").unwrap(), vec![0, 1]);
        assert_eq!(tok.encode_ids("hello unknown").unwrap(), vec![0, 2]);
    }

    #[test]
    fn missing_or_invalid_file_is_a_string_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ChatterboxTokenizer::from_file(&dir.path().join("nope.json")).is_err());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{}").unwrap();
        assert!(ChatterboxTokenizer::from_file(&bad).is_err());
    }
}
