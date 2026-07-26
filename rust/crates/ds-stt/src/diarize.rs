//! Speaker diarization — who spoke when, optionally labelled by enrolled name.
//!
//! Platform-agnostic [`crate::diarize::Diarizer`] with macOS MLX/Fluid backends; ONNX room later.
//! One-shot on the full utterance buffer (not streamed). Enrollment:
//! [`crate::diarize::Diarizer::embed`] → [`ds_config::speakers::SpeakerStore`]; diarize
//! returns cluster embeddings and the engine relabels via pure [`crate::diarize::match_speaker`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use ds_config::DiarizerProvider;
use ds_config::speakers::SpeakerStore;

/// Contiguous speaker span. Times in seconds from buffer start. `speaker` is the
/// within-utterance cluster id (key in [`DiarizationOutput::speakers`]); `name` is the
/// enrolled match when set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
    /// Enrolled name after voiceprint match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Segments plus per-cluster WeSpeaker embeddings (keyed by `speaker` id).
#[derive(Debug, Clone, Default)]
pub struct DiarizationOutput {
    pub segments: Vec<SpeakerSegment>,
    pub speakers: HashMap<String, Vec<f32>>,
}

/// Shim JSON: `{"segments":[…], "speakers":{…}}`. `speakers` optional on older shims.
#[derive(Deserialize)]
struct DiarizationJson {
    segments: Vec<SpeakerSegment>,
    #[serde(default)]
    speakers: HashMap<String, Vec<f32>>,
}

/// Shared parse for every backend's diarization JSON contract.
pub fn parse_output(json: &str) -> Result<DiarizationOutput, String> {
    let d = serde_json::from_str::<DiarizationJson>(json)
        .map_err(|e| format!("diarization JSON parse: {e}"))?;
    if d.segments
        .iter()
        .any(|s| !s.start.is_finite() || !s.end.is_finite() || s.start < 0.0 || s.end < s.start)
    {
        return Err("diarization JSON contains invalid segment times".to_string());
    }
    Ok(DiarizationOutput {
        segments: d.segments,
        speakers: d.speakers,
    })
}

/// Cosine similarity (−1..=1); 0.0 for length mismatch, empty, or zero magnitude.
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

/// Closest enrolled speaker at or above `threshold`, else `None`.
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

/// Speaker-diarization backend. Object-safe for `Box<dyn Diarizer>`.
pub trait Diarizer {
    fn preload(&mut self) -> Result<(), String>;

    fn diarize_pcm_16k_full(&mut self, pcm: &[f32]) -> Result<DiarizationOutput, String>;

    fn embed(&mut self, pcm: &[f32]) -> Result<Vec<f32>, String>;

    fn unload(&mut self) -> bool;

    fn diarize_pcm_16k(&mut self, pcm: &[f32]) -> Result<Vec<SpeakerSegment>, String> {
        Ok(self.diarize_pcm_16k_full(pcm)?.segments)
    }
}

/// Gate via [`DiarizerProvider::is_diarizer_usable`] (not a local `cfg!` — that drifted
/// from config and failed late at dlopen, #211). Message covers off-platform and unwired rungs.
pub fn ensure_backend(provider: DiarizerProvider) -> Result<(), String> {
    if provider.is_diarizer_usable() {
        Ok(())
    } else {
        Err(format!(
            "diarizer={} is not available on this platform",
            provider.as_str()
        ))
    }
}

#[cfg(target_os = "macos")]
pub use fluid_impl::FluidDiarizer;
#[cfg(target_os = "macos")]
pub use mlx_impl::MlxDiarizer;

/// MLX Sortformer + WeSpeaker via `libdontspeak_mlx`. Rust owns downloads.
#[cfg(target_os = "macos")]
mod mlx_impl {
    use std::ffi::{c_char, c_void};

    use libloading::{Library, Symbol};

    use super::{DiarizationOutput, Diarizer, parse_output};
    use ds_model::shim::{PcmCb, StrCb};

    // Result via collect_{str,pcm}; init/shutdown plain int.
    type DiarInitFn = unsafe extern "C" fn(*const c_char, f32) -> i32;
    type DiarizeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
    type EmbedFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, PcmCb) -> i32;
    type DiarShutdownFn = unsafe extern "C" fn();

    pub struct MlxDiarizer {
        lib: Option<Library>,
        loaded: bool,
        /// Sortformer activity cutoff for init (0.1–0.9; 0.0 → shim 0.5).
        activity_threshold: f32,
    }

    impl MlxDiarizer {
        pub fn new() -> Self {
            MlxDiarizer {
                lib: None,
                loaded: false,
                activity_threshold: 0.0,
            }
        }

        /// `0.0` → shim default; positive clamps to 0.1–0.9.
        pub fn with_threshold(threshold: f32) -> Self {
            MlxDiarizer {
                lib: None,
                loaded: false,
                activity_threshold: if threshold.is_finite() && threshold > 0.0 {
                    threshold.clamp(0.1, 0.9)
                } else {
                    0.0
                },
            }
        }

        fn ensure_lib(&mut self) -> Result<(), String> {
            if self.lib.is_none() {
                self.lib = Some(ds_model::shim::open(ds_model::shim::Shim::Mlx)?);
            }
            Ok(())
        }
    }

    impl Diarizer for MlxDiarizer {
        fn preload(&mut self) -> Result<(), String> {
            if self.loaded {
                return Ok(());
            }
            self.ensure_lib()?;
            let lib = self.lib.as_ref().expect("lib opened above");
            // SAFETY: `DiarInitFn`; `dir` lives across the call.
            let rc = unsafe {
                let init: Symbol<DiarInitFn> = lib
                    .get(b"ds_mlx_diar_init\0")
                    .map_err(|e| format!("ds_mlx_diar_init symbol: {e}"))?;
                let dir = ds_model::shim::mlx_model_root_arg();
                init(dir.as_ptr(), self.activity_threshold)
            };
            if rc != 0 {
                return Err(format!("ds_mlx_diar_init failed (rc={rc})"));
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
            // SAFETY: `DiarizeFn`; `pcm` + collect_str outlive the call.
            let dz: Symbol<DiarizeFn> = unsafe { lib.get(b"ds_mlx_diarize\0") }
                .map_err(|e| format!("ds_mlx_diarize symbol: {e}"))?;
            let json = ds_model::shim::collect_str(|ctx, cb| {
                // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking
                // call; `collect_str` supplies a synchronous pair the shim does not retain.
                unsafe { dz(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
            })
            .map_err(|rc| format!("ds_mlx_diarize failed (rc={rc})"))?;
            parse_output(&json)
        }

        fn embed(&mut self, pcm: &[f32]) -> Result<Vec<f32>, String> {
            if pcm.is_empty() {
                return Err("embed: empty audio".into());
            }
            self.preload()?;
            let lib = self.lib.as_ref().expect("lib loaded above");
            // SAFETY: `EmbedFn`; `pcm` + collect_pcm outlive the call.
            let ex: Symbol<EmbedFn> = unsafe { lib.get(b"ds_mlx_diar_embed\0") }
                .map_err(|e| format!("ds_mlx_diar_embed symbol: {e}"))?;
            let emb = ds_model::shim::collect_pcm(|ctx, cb| {
                // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking
                // call; `collect_pcm` supplies a synchronous pair the shim does not retain.
                unsafe { ex(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
            })
            .map_err(|rc| format!("ds_mlx_diar_embed failed (rc={rc})"))?;
            if emb.is_empty() {
                return Err("embed: empty embedding".into());
            }
            Ok(emb)
        }

        fn unload(&mut self) -> bool {
            if !self.loaded {
                return false;
            }
            if let Some(lib) = &self.lib {
                // SAFETY: idempotent shim shutdown.
                unsafe {
                    if let Ok(sd) = lib.get::<DiarShutdownFn>(b"ds_mlx_diar_shutdown\0") {
                        sd();
                    }
                }
            }
            self.loaded = false;
            true
        }
    }

    impl Default for MlxDiarizer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for MlxDiarizer {
        fn drop(&mut self) {
            self.unload();
        }
    }
}

/// FluidAudio pyannote + WeSpeaker (Core ML / ANE) via `libdontspeak_fluid`.
/// Same [`Diarizer`] + collect bridge as MLX; `ds_fluid_diar_*` symbols / Core ML set.
#[cfg(target_os = "macos")]
mod fluid_impl {
    use std::ffi::{c_char, c_void};

    use libloading::{Library, Symbol};

    use super::{DiarizationOutput, Diarizer, parse_output};
    use ds_model::shim::{PcmCb, StrCb};

    // Result via collect_{str,pcm}; init/shutdown plain int.
    type DiarInitFn = unsafe extern "C" fn(*const c_char, f32) -> i32;
    type DiarizeFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, StrCb) -> i32;
    type EmbedFn = unsafe extern "C" fn(*const f32, usize, i32, *mut c_void, PcmCb) -> i32;
    type DiarShutdownFn = unsafe extern "C" fn();

    pub struct FluidDiarizer {
        lib: Option<Library>,
        loaded: bool,
        /// Clustering cutoff for init (0.1–0.9; 0.0 → shim 0.7).
        clustering_threshold: f32,
    }

    impl FluidDiarizer {
        pub fn new() -> Self {
            FluidDiarizer {
                lib: None,
                loaded: false,
                clustering_threshold: 0.0,
            }
        }

        /// `0.0` → shim default; positive clamps to 0.1–0.9.
        pub fn with_threshold(threshold: f32) -> Self {
            FluidDiarizer {
                lib: None,
                loaded: false,
                clustering_threshold: if threshold.is_finite() && threshold > 0.0 {
                    threshold.clamp(0.1, 0.9)
                } else {
                    0.0
                },
            }
        }

        fn ensure_lib(&mut self) -> Result<(), String> {
            if self.lib.is_none() {
                self.lib = Some(ds_model::shim::open(ds_model::shim::Shim::Fluid)?);
            }
            Ok(())
        }
    }

    impl Diarizer for FluidDiarizer {
        fn preload(&mut self) -> Result<(), String> {
            if self.loaded {
                return Ok(());
            }
            self.ensure_lib()?;
            let lib = self.lib.as_ref().expect("lib opened above");
            // SAFETY: `DiarInitFn`; `dir` lives across the call.
            let rc = unsafe {
                let init: Symbol<DiarInitFn> = lib
                    .get(b"ds_fluid_diar_init\0")
                    .map_err(|e| format!("ds_fluid_diar_init symbol: {e}"))?;
                let dir = ds_model::shim::fluid_diarization_dir_arg();
                init(dir.as_ptr(), self.clustering_threshold)
            };
            if rc != 0 {
                return Err(format!("ds_fluid_diar_init failed (rc={rc})"));
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
            // SAFETY: `DiarizeFn`; `pcm` + collect_str outlive the call.
            let dz: Symbol<DiarizeFn> = unsafe { lib.get(b"ds_fluid_diarize\0") }
                .map_err(|e| format!("ds_fluid_diarize symbol: {e}"))?;
            let json = ds_model::shim::collect_str(|ctx, cb| {
                // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking
                // call; `collect_str` supplies a synchronous pair the shim does not retain.
                unsafe { dz(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
            })
            .map_err(|rc| format!("ds_fluid_diarize failed (rc={rc})"))?;
            parse_output(&json)
        }

        fn embed(&mut self, pcm: &[f32]) -> Result<Vec<f32>, String> {
            if pcm.is_empty() {
                return Err("embed: empty audio".into());
            }
            self.preload()?;
            let lib = self.lib.as_ref().expect("lib loaded above");
            // SAFETY: `EmbedFn`; `pcm` + collect_pcm outlive the call.
            let ex: Symbol<EmbedFn> = unsafe { lib.get(b"ds_fluid_diar_embed\0") }
                .map_err(|e| format!("ds_fluid_diar_embed symbol: {e}"))?;
            let emb = ds_model::shim::collect_pcm(|ctx, cb| {
                // SAFETY: `pcm` is readable for `pcm.len()` floats through this blocking
                // call; `collect_pcm` supplies a synchronous pair the shim does not retain.
                unsafe { ex(pcm.as_ptr(), pcm.len(), 16_000, ctx, cb) }
            })
            .map_err(|rc| format!("ds_fluid_diar_embed failed (rc={rc})"))?;
            if emb.is_empty() {
                return Err("embed: empty embedding".into());
            }
            Ok(emb)
        }

        fn unload(&mut self) -> bool {
            if !self.loaded {
                return false;
            }
            if let Some(lib) = &self.lib {
                // SAFETY: idempotent shim shutdown.
                unsafe {
                    if let Ok(sd) = lib.get::<DiarShutdownFn>(b"ds_fluid_diar_shutdown\0") {
                        sd();
                    }
                }
            }
            self.loaded = false;
            true
        }
    }

    impl Default for FluidDiarizer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for FluidDiarizer {
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
    fn ensure_backend_gates_every_provider_per_platform() {
        // Must match `is_diarizer_usable` (not a local cfg! — #211). Err string is user-facing.
        for provider in DiarizerProvider::ALL.iter().copied() {
            let res = ensure_backend(provider);
            assert_eq!(res.is_ok(), provider.is_diarizer_usable(), "{provider:?}");
            if let Err(msg) = res {
                assert_eq!(
                    msg,
                    format!(
                        "diarizer={} is not available on this platform",
                        provider.as_str()
                    )
                );
            }
        }
    }

    #[test]
    fn match_speaker_picks_closest_above_threshold() {
        let mut store = SpeakerStore::default();
        store.upsert("Alex", vec![1.0, 0.0, 0.0]);
        store.upsert("Sam", vec![0.0, 1.0, 0.0]);
        assert_eq!(
            match_speaker(&[0.99, 0.05, 0.0], &store, 0.65).as_deref(),
            Some("Alex")
        );
        assert_eq!(match_speaker(&[0.0, 0.0, 1.0], &store, 0.65), None);
        assert_eq!(
            match_speaker(&[1.0, 0.0, 0.0], &SpeakerStore::default(), 0.5),
            None
        );
    }
}
