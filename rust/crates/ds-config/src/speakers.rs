//! Enrolled-speaker store — voiceprints so diarization labels by name, not "Speaker 1".
//!
//! JSON at [`crate::Paths::speakers_json`]: `{name, embedding}` (WeSpeaker ~256 f32).
//! Engine is sole writer; diarize path cosine-matches clusters against the set.
//! Load fail-open (missing/corrupt → empty); save atomic via [`crate::atomic_write_json`].

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One enrolled person: display name + voiceprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Speaker {
    pub name: String,
    pub embedding: Vec<f32>,
}

/// Full enrolled set. `#[serde(default)]` so empty/partial files deserialize.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpeakerStore {
    #[serde(default)]
    pub speakers: Vec<Speaker>,
}

impl SpeakerStore {
    /// Fail-open load: missing/corrupt → empty (mirrors `VoiceConfig::load`).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomic persist (temp + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let value = serde_json::to_value(self)
            .map_err(|e| std::io::Error::other(format!("serialize speakers: {e}")))?;
        crate::atomic_write_json(path, &value)
    }

    /// Add or replace voiceprint for `name` (case-sensitive).
    pub fn upsert(&mut self, name: impl Into<String>, embedding: Vec<f32>) {
        let name = name.into();
        if let Some(s) = self.speakers.iter_mut().find(|s| s.name == name) {
            s.embedding = embedding;
        } else {
            self.speakers.push(Speaker { name, embedding });
        }
    }

    /// Remove by name; returns whether one was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.speakers.len();
        self.speakers.retain(|s| s.name != name);
        self.speakers.len() != before
    }

    /// Enrolled names, insertion order.
    pub fn names(&self) -> Vec<String> {
        self.speakers.iter().map(|s| s.name.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.speakers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.speakers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_remove() {
        let mut s = SpeakerStore::default();
        s.upsert("Alex", vec![1.0, 2.0]);
        s.upsert("Sam", vec![3.0]);
        assert_eq!(s.len(), 2);
        s.upsert("Alex", vec![9.0]);
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.speakers
                .iter()
                .find(|x| x.name == "Alex")
                .unwrap()
                .embedding,
            vec![9.0]
        );
        assert!(s.remove("Sam"));
        assert!(!s.remove("Nobody"));
        assert_eq!(s.names(), vec!["Alex".to_string()]);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speakers.json");
        let mut s = SpeakerStore::default();
        s.upsert("Alex", vec![0.1, 0.2, 0.3]);
        s.save(&path).unwrap();
        let loaded = SpeakerStore::load(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.speakers[0].name, "Alex");
        assert_eq!(loaded.speakers[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn load_missing_is_empty() {
        assert!(SpeakerStore::load(Path::new("/nonexistent/speakers.json")).is_empty());
    }
}
