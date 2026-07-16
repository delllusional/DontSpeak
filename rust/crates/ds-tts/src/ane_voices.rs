//! FluidAudio Core ML / ANE Kokoro voice packs, materialized from the local npz.
//!
//! ANE ships only `af_heart.bin` on HF, but the graph accepts any Kokoro voice
//! (`[510, 256]` fp32 style tensor). We hold all 54 in ONNX `voices-v1.0.bin`, so
//! extract via `crate::voices::voice_pack_bytes` into FluidAudio's on-disk cache
//! instead of depending on per-voice upstream `.bin`s.
//!
//! `ensureVoicePack` prefers local files — materialized packs avoid network/404→
//! `af_heart` fallback. Layout matches shipped `af_heart.bin` (522_240 LE f32 bytes).

use std::path::PathBuf;

/// One ANE voice pack: `[510, 256]` LE f32.
const VOICE_PACK_BYTES: usize = 510 * 256 * 4;

/// Where FluidAudio's English ANE chain LOADS packs
/// (`coreml_dir()/kokoro-82m-coreml/ANE/`). Shim inits with [`ds_config::coreml_dir`],
/// not FluidAudio's `~/.cache/fluidaudio` — materialize HERE or 404→`af_heart`.
/// Via shared [`ds_model::coreml_repo::kokoro_ane_dir`] so materialize target can't
/// drift from synth reads. `None` if `$HOME` unset.
pub fn ane_dir() -> Option<PathBuf> {
    ds_model::coreml_repo::kokoro_ane_dir()
}

/// Filename FluidAudio's `ensureVoicePack` looks up (ASCII alnum + `_`).
/// FluidAudio is Unicode-aware; Kokoro ids are all-ASCII. `None` if empty.
fn sanitize(voice: &str) -> Option<String> {
    let s: String = voice
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Path `voice`'s ANE pack would live at.
pub fn voice_pack_path(voice: &str) -> Option<PathBuf> {
    Some(ane_dir()?.join(format!("{}.bin", sanitize(voice)?)))
}

/// ANE pack already on disk?
pub fn is_materialized(voice: &str) -> bool {
    voice_pack_path(voice).is_some_and(|p| p.is_file())
}

/// Extract `voice` from local `voices-v1.0.bin` into ANE cache. Idempotent.
pub fn materialize(voice: &str) -> Result<PathBuf, String> {
    let id = sanitize(voice).ok_or_else(|| format!("invalid voice id: {voice:?}"))?;
    let dir = ane_dir().ok_or("cannot resolve FluidAudio cache dir ($HOME unset)")?;
    let dest = dir.join(format!("{id}.bin"));
    if dest.is_file() {
        return Ok(dest);
    }
    let npz_path = ds_model::model_path(ds_model::KOKORO_VOICES_FILE)
        .ok_or("cannot resolve voices npz path")?;
    if !npz_path.is_file() {
        return Err(format!(
            "{} not downloaded yet; run download_models first",
            ds_model::KOKORO_VOICES_FILE
        ));
    }
    let npz = std::fs::read(&npz_path).map_err(|e| format!("read voices npz: {e}"))?;
    let pack = crate::voices::voice_pack_bytes(&npz, &id)?;
    if pack.len() != VOICE_PACK_BYTES {
        return Err(format!(
            "voice {id} pack is {} bytes, expected {VOICE_PACK_BYTES}",
            pack.len()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // Atomic write+rename: concurrent synth never sees half-written pack; two writers
    // can't clobber temps. Content is deterministic — last rename wins harmlessly.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{id}.bin.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, &pack).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("install {}: {e}", dest.display())
    })?;
    Ok(dest)
}

/// Voices npz on disk? Decide extract-now vs kick download.
pub fn voices_npz_present() -> bool {
    ds_model::model_path(ds_model::KOKORO_VOICES_FILE).is_some_and(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_matches_fluidaudio_rules() {
        assert_eq!(sanitize("af_sarah").as_deref(), Some("af_sarah"));
        assert_eq!(sanitize("../etc/passwd").as_deref(), Some("etcpasswd"));
        assert_eq!(sanitize("af-sarah!").as_deref(), Some("afsarah"));
        assert_eq!(sanitize("///"), None);
    }

    #[test]
    fn voice_pack_path_under_ane_dir() {
        // `None` only if `$HOME` unset — expect so that fails loudly.
        let p = voice_pack_path("am_adam").expect("ane_dir resolves ($HOME is set)");
        assert!(p.ends_with("kokoro-82m-coreml/ANE/am_adam.bin"));
    }
}
