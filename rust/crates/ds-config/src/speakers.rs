//! Enrolled-speaker store — the persistent voiceprints that let diarization label
//! segments by NAME instead of "Speaker 1".
//!
//! One JSON file at [`crate::Paths::speakers_json`] (in the roaming config dir):
//! a list of `{name, embedding}` where `embedding` is a WeSpeaker voiceprint
//! (~256 `f32`s) extracted once at enroll time. The engine owns this file (single
//! writer); the diarize path loads it and cosine-matches each speaker cluster's
//! embedding against the enrolled set.
//!
//! Load is fail-open (a missing/corrupt file → empty store, never an error — the
//! same discipline as the rest of the config layer); save is atomic (temp + rename
//! via [`crate::atomic_write_json`]).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// One enrolled person: a display name and their voiceprint embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Speaker {
    pub name: String,
    pub embedding: Vec<f32>,
}

/// The whole enrolled set. `#[serde(default)]` on the field so an empty/partial file
/// still deserializes to an empty store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpeakerStore {
    #[serde(default)]
    pub speakers: Vec<Speaker>,
}

impl SpeakerStore {
    /// Load the store from `path`, failing OPEN: a missing or corrupt file yields an
    /// empty store rather than an error (mirrors `LifetimeSeconds::load`).
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomically persist the store to `path` (temp file in the same dir + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        crate::atomic_write_json(path, &value)
    }

    /// Add or replace the voiceprint for `name` (case-sensitive exact match).
    pub fn upsert(&mut self, name: impl Into<String>, embedding: Vec<f32>) {
        let name = name.into();
        if let Some(s) = self.speakers.iter_mut().find(|s| s.name == name) {
            s.embedding = embedding;
        } else {
            self.speakers.push(Speaker { name, embedding });
        }
    }

    /// Remove the speaker named `name`; returns whether one was removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.speakers.len();
        self.speakers.retain(|s| s.name != name);
        self.speakers.len() != before
    }

    /// Enrolled names, in insertion order.
    pub fn names(&self) -> Vec<String> {
        self.speakers.iter().map(|s| s.name.clone()).collect()
    }

    /// Number of enrolled speakers.
    pub fn len(&self) -> usize {
        self.speakers.len()
    }

    /// Whether no speakers are enrolled.
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
        // upsert replaces, not duplicates.
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
