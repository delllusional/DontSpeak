//! FluidAudio Core ML / ANE Kokoro voice packs from the shared ONNX `voices-v1.0.bin`.
//!
//! HF ships only `ANE/af_heart.bin`, and FluidAudio's `ensureVoicePack` throws (no fallback)
//! for any other id, while Kokoro's default voices are `af_sarah` / `bf_emma`. The graph accepts
//! any Kokoro voice (`[510, 256]` fp32), so materialize the requested pack from the shared npz
//! into the ANE cache the chain loads from. Layout = the shipped pack: 510*256 LE f32.

use std::path::PathBuf;

use ds_model::hf_repo::ModelRoots;

/// One ANE voice pack: `[510, 256]` LE f32.
const VOICE_PACK_FLOATS: usize = 510 * 256;
const VOICE_PACK_BYTES: usize = VOICE_PACK_FLOATS * 4;

/// Where FluidAudio's English ANE chain loads packs: `<model>/coreml/kokoro-82m-coreml/ANE`.
/// DontSpeak inits the shim with its OWN root, not FluidAudio's `~/.cache/fluidaudio`, so a
/// pack materialized for that chain MUST land here. Via shared
/// [`ds_model::coreml_repo::kokoro_ane_dir`] so the materialize target cannot drift from the
/// shim's read path.
fn ane_dir(roots: &ModelRoots) -> PathBuf {
    ds_model::coreml_repo::kokoro_ane_dir(roots)
}

/// Filename FluidAudio's `ensureVoicePack` looks up (ASCII alnum + `_`). Kokoro ids are all
/// ASCII; `None` if empty after filtering.
fn sanitize(voice: &str) -> Option<String> {
    let s: String = voice
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Path `voice`'s ANE pack would live at. `None` if the model dir is unresolvable.
pub fn voice_pack_path(voice: &str) -> Option<PathBuf> {
    let roots = ModelRoots::ambient()?;
    Some(ane_dir(&roots).join(format!("{}.bin", sanitize(voice)?)))
}

/// ANE pack already on disk?
pub fn is_materialized(voice: &str) -> bool {
    voice_pack_path(voice).is_some_and(|p| p.is_file())
}

/// Extract `voice` from the local voices npz into the ANE cache. Idempotent; atomic install.
pub fn materialize(voice: &str) -> Result<PathBuf, String> {
    let id = sanitize(voice).ok_or_else(|| format!("invalid voice id: {voice:?}"))?;
    let roots = ModelRoots::ambient().ok_or("cannot resolve the model directory")?;
    let dir = ane_dir(&roots);
    let dest = dir.join(format!("{id}.bin"));
    if dest.is_file() {
        return Ok(dest);
    }
    let npz_path = roots.model.join(ds_model::KOKORO_VOICES_FILE);
    if !npz_path.is_file() {
        return Err(format!(
            "{} not downloaded yet; run download_models first",
            ds_model::KOKORO_VOICES_FILE
        ));
    }
    let npz = std::fs::read(&npz_path).map_err(|e| format!("read voices npz: {e}"))?;
    let pack = crate::voices::parse_voices_npz(&npz)?
        .remove(&id)
        .ok_or_else(|| format!("voice {id} is not in {}", ds_model::KOKORO_VOICES_FILE))?;
    if pack.len() != VOICE_PACK_FLOATS {
        return Err(format!(
            "voice {id} is {} floats, expected {VOICE_PACK_FLOATS}",
            pack.len()
        ));
    }
    let mut bytes = Vec::with_capacity(VOICE_PACK_BYTES);
    for sample in &pack {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    debug_assert_eq!(bytes.len(), VOICE_PACK_BYTES);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // Atomic write+rename: concurrent synth never sees a half-written pack, and two writers
    // cannot clobber temps. Content is deterministic -- last rename wins harmlessly.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{id}.bin.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("install {}: {e}", dest.display())
    })?;
    Ok(dest)
}

/// Voices npz on disk? Decide extract-now vs kick download.
pub fn voices_npz_present() -> bool {
    ModelRoots::ambient()
        .is_some_and(|roots| roots.model.join(ds_model::KOKORO_VOICES_FILE).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sanitize_matches_fluidaudio_rules() {
        assert_eq!(sanitize("af_sarah").as_deref(), Some("af_sarah"));
        assert_eq!(sanitize("../etc/passwd").as_deref(), Some("etcpasswd"));
        assert_eq!(sanitize("af-sarah!").as_deref(), Some("afsarah"));
        assert_eq!(sanitize("///"), None);
    }

    /// Pure path math (no ambient, no FS): a pack lands in the ANE cache under the given roots.
    #[test]
    fn voice_pack_lands_in_the_ane_cache_under_roots() {
        let roots = ModelRoots::under(Path::new("/roots"));
        let pack = ane_dir(&roots).join("am_adam.bin");
        assert!(pack.ends_with("kokoro-82m-coreml/ANE/am_adam.bin"));
        assert!(pack.starts_with("/roots"));
    }
}
