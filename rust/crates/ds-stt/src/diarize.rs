//! Speaker diarization — "who spoke when", optionally labelled by enrolled NAME.
//!
//! Mirrors the [`crate::Stt`] split: ONE platform-agnostic [`Diarizer`](crate::diarize::Diarizer)
//! trait with a Core ML / ANE backend now (`CoremlDiarizer`,
//! macOS only) and room for a cross-platform ONNX backend later (Pyannote + WeSpeaker over
//! `ort`). Diarization runs on the FULL utterance buffer at end-of-capture, one-shot (not
//! streamed).
//!
//! Enrollment: [`Diarizer::embed`](crate::diarize::Diarizer::embed) extracts a WeSpeaker
//! voiceprint from a sample; the engine persists it in
//! [`ds_config::speakers::SpeakerStore`]. At diarize time the shim returns each cluster's
//! embedding ([`DiarizationOutput::speakers`](crate::diarize::DiarizationOutput::speakers)) and
//! the engine cosine-matches them against the store via
//! [`match_speaker`](crate::diarize::match_speaker) to relabel
//! segments with names. The matching is pure (here) so it's unit-tested.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use ds_config::speakers::SpeakerStore;

/// One contiguous span attributed to a single speaker. Times are seconds from the start
/// of the diarized buffer. `speaker` is the within-utterance cluster id assigned by the
/// backend (FluidAudio's `speakerId`) — the SAME string used as the key in
/// [`DiarizationOutput::speakers`], which is how the engine joins a segment to its
/// embedding to relabel it. `name` is the enrolled person the engine matched it to, if any.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    /// Enrolled name (set by the engine after voiceprint matching); absent until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Full diarizer result: the segments plus each cluster's WeSpeaker embedding (keyed by
/// the same `speaker` id), so the caller can match clusters to enrolled voiceprints.
#[derive(Debug, Clone, Default)]
pub struct DiarizationOutput {
    pub segments: Vec<SpeakerSegment>,
    pub speakers: HashMap<String, Vec<f32>>,
}

/// The shim emits `{"segments":[…], "speakers":{"<id>":[…floats…]}}`. `speakers` is
/// absent on older shims / when embeddings aren't available, hence `#[serde(default)]`.
#[derive(Deserialize)]
struct DiarizationJson {
    segments: Vec<SpeakerSegment>,
    #[serde(default)]
    speakers: HashMap<String, Vec<f32>>,
}

/// Parse the shim's diarization JSON into the full output (segments + per-speaker
/// embeddings). Shared by every backend returning the same JSON contract.
pub fn parse_output(json: &str) -> Result<DiarizationOutput, String> {
    serde_json::from_str::<DiarizationJson>(json)
        .map(|d| DiarizationOutput {
            segments: d.segments,
            speakers: d.speakers,
        })
        .map_err(|e| format!("diarization JSON parse: {e}"))
}

/// Cosine similarity of two equal-length embedding vectors (0.0 for mismatched/empty
/// or zero-magnitude inputs). Range −1..=1; identical direction → 1.0.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Match an embedding to the closest enrolled speaker whose cosine similarity is at or
/// above `threshold`; `None` if no enrolled voiceprint is close enough.
pub fn match_speaker(embedding: &[f32], store: &SpeakerStore, threshold: f32) -> Option<String> {
    let mut best: Option<(&str, f32)> = None;
    for sp in &store.speakers {
        let sim = cosine(embedding, &sp.embedding);
        if best.is_none_or(|(_, b)| sim > b) {
            best = Some((sp.name.as_str(), sim));
        }
    }
    best.filter(|&(_, sim)| sim >= threshold)
        .map(|(name, _)| name.to_string())
}

/// A speaker-diarization backend. Object-safe so a factory can hand back
/// `Box<dyn Diarizer>` once a second (ONNX) backend exists.
pub trait Diarizer {
    /// Download (first use) + load the segmentation/embedding models so the first
    /// diarization doesn't pay the load cost. The eager counterpart to [`unload`](Self::unload).
    fn preload(&mut self) -> Result<(), String>;

    /// Diarize 16 kHz mono f32 PCM → segments + per-speaker embeddings. Empty input →
    /// an empty output.
    fn diarize_pcm_16k_full(&mut self, pcm: &[f32]) -> Result<DiarizationOutput, String>;

    /// Extract one WeSpeaker voiceprint from 16 kHz mono f32 PCM (the enrollment
    /// primitive). Empty input → an error.
    fn embed(&mut self, pcm: &[f32]) -> Result<Vec<f32>, String>;

    /// Download the diarization models if absent (explicit, e.g. the Settings button).
    fn download(&mut self) -> Result<(), String>;

    /// Free the loaded models; the next diarization lazily reloads them. Returns whether
    /// anything was loaded.
    fn unload(&mut self) -> bool;

    /// Diarize → just the segments (convenience over [`diarize_pcm_16k_full`](Self::diarize_pcm_16k_full)).
    fn diarize_pcm_16k(&mut self, pcm: &[f32]) -> Result<Vec<SpeakerSegment>, String> {
        Ok(self.diarize_pcm_16k_full(pcm)?.segments)
    }

    /// Whether this backend is usable right now (models present, supported platform).
    fn is_available(&self) -> bool {
        true
    }
}

#[cfg(target_os = "macos")]
pub use coreml_impl::CoremlDiarizer;

/// FluidAudio's offline diarizer behind the `libsmkokoro.dylib` C ABI (the SAME shim
/// as the apple-native Kokoro TTS + Parakeet STT backends). macOS only.
#[cfg(target_os = "macos")]
mod coreml_impl {
    use std::ffi::{c_char, c_void};

    use libloading::{Library, Symbol};

    use super::{DiarizationOutput, Diarizer, parse_output};
    use crate::shim::{PcmCb, StrCb};

    // diarize/embed still BLOCK and return their status; the JSON / embedding comes back through a
    // borrowed callback (copied out by `crate::shim::collect_{str,pcm}`), so there's no out-param
    // and no `smk_free_str` / `smk_free`. init/download/shutdown carry no buffer → plain int32.
    type DiarInitFn = unsafe extern "C" fn(*const c_char, f32) -> i32;
    type DiarizeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
    type EmbedFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, PcmCb) -> i32;
    type DownloadFn = unsafe extern "C" fn() -> i32;
    type DiarShutdownFn = unsafe extern "C" fn();

    /// Pyannote + WeSpeaker diarization over Core ML / ANE. Models download on first
    /// `preload`/diarize. Mirrors [`crate::coreml::CoremlTranscriber`]'s lazy-load shape.
    pub struct CoremlDiarizer {
        lib: Option<Library>,
        loaded: bool,
        /// Clustering threshold passed to `smk_diar_init` (0.5–0.9, lower = more
        /// speakers); 0.0 = use FluidAudio's default (0.7).
        threshold: f32,
    }

    impl CoremlDiarizer {
        /// Not loaded until the first [`preload`](Diarizer::preload) / diarization.
        /// Uses FluidAudio's default clustering threshold.
        pub fn new() -> Self {
            CoremlDiarizer {
                lib: None,
                loaded: false,
                threshold: 0.0,
            }
        }

        /// Like [`new`](Self::new) but with an explicit clustering threshold (0.5–0.9,
        /// lower = more speakers). `0.0` keeps FluidAudio's default.
        pub fn with_threshold(threshold: f32) -> Self {
            CoremlDiarizer {
                lib: None,
                loaded: false,
                threshold,
            }
        }

        /// Ensure the shim dylib is open (resolves `SMKOKORO_DYLIB_PATH`).
        fn ensure_lib(&mut self) -> Result<(), String> {
            if self.lib.is_none() {
                self.lib = Some(crate::shim::open()?);
            }
            Ok(())
        }
    }

    impl Diarizer for CoremlDiarizer {
        fn preload(&mut self) -> Result<(), String> {
            if self.loaded {
                return Ok(());
            }
            self.ensure_lib()?;
            let lib = self.lib.as_ref().expect("lib opened above");
            let rc = unsafe {
                let init: Symbol<DiarInitFn> = lib
                    .get(b"smk_diar_init\0")
                    .map_err(|e| format!("smk_diar_init symbol: {e}"))?;
                // Our DontSpeak-controlled Core ML cache dir (not "" → FluidAudio's default).
                let dir = crate::shim::model_dir_arg();
                init(dir.as_ptr(), self.threshold)
            };
            if rc != 0 {
                return Err(format!("smk_diar_init failed (rc={rc})"));
            }
            self.loaded = true;
            Ok(())
        }

        fn diarize_pcm_16k_full(&mut self, pcm: &[f32]) -> Result<DiarizationOutput, String> {
            if pcm.is_empty() {
                return Ok(DiarizationOutput::default());
            }
            self.preload()?;
            let lib = self.lib.as_ref().expect("lib loaded above");
            let dz: Symbol<DiarizeFn> = unsafe { lib.get(b"smk_diarize\0") }
                .map_err(|e| format!("smk_diarize symbol: {e}"))?;
            // The shim borrows the JSON to our sink, which copies it out (no smk_free_str).
            // The call blocks; `pcm` lives across it.
            let json = crate::shim::collect_str(|ctx, cb| unsafe {
                dz(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
            })
            .map_err(|rc| format!("smk_diarize failed (rc={rc})"))?;
            parse_output(&json)
        }

        fn embed(&mut self, pcm: &[f32]) -> Result<Vec<f32>, String> {
            if pcm.is_empty() {
                return Err("embed: empty audio".into());
            }
            self.preload()?;
            let lib = self.lib.as_ref().expect("lib loaded above");
            let ex: Symbol<EmbedFn> = unsafe { lib.get(b"smk_diar_embed\0") }
                .map_err(|e| format!("smk_diar_embed symbol: {e}"))?;
            // The shim borrows the embedding to our sink, which copies it out (no smk_free).
            // The call blocks; `pcm` lives across it.
            let emb = crate::shim::collect_pcm(|ctx, cb| unsafe {
                ex(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb)
            })
            .map_err(|rc| format!("smk_diar_embed failed (rc={rc})"))?;
            if emb.is_empty() {
                return Err("embed: empty embedding".into());
            }
            Ok(emb)
        }

        fn download(&mut self) -> Result<(), String> {
            self.ensure_lib()?;
            let lib = self.lib.as_ref().expect("lib opened above");
            let rc = unsafe {
                let dl: Symbol<DownloadFn> = lib
                    .get(b"smk_diar_download\0")
                    .map_err(|e| format!("smk_diar_download symbol: {e}"))?;
                dl()
            };
            if rc != 0 {
                return Err(format!("smk_diar_download failed (rc={rc})"));
            }
            Ok(())
        }

        fn unload(&mut self) -> bool {
            if !self.loaded {
                return false;
            }
            if let Some(lib) = &self.lib {
                // SAFETY: idempotent shim shutdown.
                unsafe {
                    if let Ok(sd) = lib.get::<DiarShutdownFn>(b"smk_diar_shutdown\0") {
                        sd();
                    }
                }
            }
            self.loaded = false;
            true
        }
    }

    impl Default for CoremlDiarizer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for CoremlDiarizer {
        fn drop(&mut self) {
            self.unload();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_segments_and_speakers() {
        let json = r#"{"segments":[{"speaker":"1","start":0.0,"end":2.5},
                                    {"speaker":"2","start":2.5,"end":4.0}],
                       "speakers":{"1":[1.0,0.0],"2":[0.0,1.0]}}"#;
        let out = parse_output(json).expect("valid JSON");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.segments[0].speaker, "1");
        assert_eq!(out.segments[0].name, None);
        assert_eq!(out.speakers.len(), 2);
        assert_eq!(out.speakers["1"], vec![1.0, 0.0]);
    }

    #[test]
    fn parses_without_speakers_map() {
        // Older shim / no embeddings: speakers defaults to empty.
        let out = parse_output(r#"{"segments":[]}"#).unwrap();
        assert!(out.segments.is_empty());
        assert!(out.speakers.is_empty());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_output("not json").is_err());
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0); // length mismatch
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0); // zero magnitude
    }

    #[test]
    fn match_speaker_picks_closest_above_threshold() {
        let mut store = SpeakerStore::default();
        store.upsert("Alex", vec![1.0, 0.0, 0.0]);
        store.upsert("Sam", vec![0.0, 1.0, 0.0]);
        // Almost exactly Alex's direction.
        assert_eq!(
            match_speaker(&[0.99, 0.05, 0.0], &store, 0.65).as_deref(),
            Some("Alex")
        );
        // Orthogonal to everyone → no match.
        assert_eq!(match_speaker(&[0.0, 0.0, 1.0], &store, 0.65), None);
        // Empty store → no match.
        assert_eq!(
            match_speaker(&[1.0, 0.0, 0.0], &SpeakerStore::default(), 0.5),
            None
        );
    }
}
