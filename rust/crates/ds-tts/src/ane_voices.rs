//! FluidAudio Core ML / ANE Kokoro voice packs, materialized from the local npz.
//!
//! FluidAudio's ANE Kokoro variant ships exactly ONE voice pack on HuggingFace
//! (`af_heart.bin`), even though its 7-stage graph accepts ANY Kokoro voice: a
//! voice pack is just a model-independent `[510, 256]` fp32 style tensor. We
//! already hold all 54 of those tensors locally inside the ONNX `voices-v1.0.bin`
//! that the Kokoro backend downloads regardless — so rather than depend on
//! upstream hosting per-voice ANE `.bin` files, we extract the requested voice
//! straight from that npz (via `crate::voices::voice_pack_bytes`, a targeted
//! raw byte-slice extractor sharing the low-level npz/npy helpers) and write it
//! into FluidAudio's on-disk cache.
//!
//! FluidAudio's `ensureVoicePack` checks the local file FIRST and only downloads
//! when it's absent, so a materialized `<voice>.bin` is picked up transparently on
//! the next synth — no network, no 404 fallback to `af_heart`. The byte layout is
//! identical (the npz stores each voice as a `<f4` C-order `(510, 1, 256)` array =
//! 510*256 contiguous little-endian f32 = 522_240 bytes); verified byte-for-byte
//! against the shipped `af_heart.bin`.

use std::path::PathBuf;

/// Bytes in one ANE voice pack: `[510, 256]` little-endian f32.
const VOICE_PACK_BYTES: usize = 510 * 256 * 4;

/// The directory FluidAudio's English ANE Kokoro chain LOADS voice packs from —
/// `coreml_dir()/kokoro-82m-coreml/ANE/` (where `af_heart.bin` ships). DontSpeak inits the
/// shim with [`ds_config::coreml_dir`] (NOT FluidAudio's empty-default `~/.cache/fluidaudio`
/// cache), so `KokoroAneManager` resolves packs under OUR cache, not FluidAudio's scattered
/// default. We MUST materialize here or the shim won't find the pack — it then tries to
/// download, 404s, and silently degrades to `af_heart`. Resolved via the ONE shared
/// [`ds_model::coreml_repo::kokoro_ane_dir`] so the materialize target can't drift from where
/// the synth chain reads. `None` if the cache dir can't be resolved (`$HOME` unset).
pub fn ane_dir() -> Option<PathBuf> {
    ds_model::coreml_repo::kokoro_ane_dir()
}

/// Sanitize a voice id to the filename FluidAudio's `ensureVoicePack` looks up
/// (ASCII letters, digits, underscore), so a materialized file always matches.
/// FluidAudio's own filter is Unicode-aware (`isLetter`/`isNumber`); this is
/// ASCII-only, identical for the all-ASCII Kokoro ids. `None` if nothing survives.
fn sanitize(voice: &str) -> Option<String> {
    let s: String = voice
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!s.is_empty()).then_some(s)
}

/// Path `voice`'s ANE pack would live at (whether or not it exists yet).
pub fn voice_pack_path(voice: &str) -> Option<PathBuf> {
    Some(ane_dir()?.join(format!("{}.bin", sanitize(voice)?)))
}

/// Is `voice`'s ANE pack already on disk (playable without any fetch/extract)?
pub fn is_materialized(voice: &str) -> bool {
    voice_pack_path(voice).is_some_and(|p| p.is_file())
}

/// Extract `voice` from the local `voices-v1.0.bin` and write it into FluidAudio's
/// ANE cache so the ANE chain can play it. Idempotent — a no-op returning the
/// existing path when already present. Each error names the step that failed.
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
    // Atomic: write a writer-UNIQUE temp in the same dir, then rename, so a concurrent
    // synth never reads a half-written pack AND two writers (warm child + a cold-spawned
    // fallback) can't clobber each other's temp. Content is deterministic, so the
    // last rename winning is harmless.
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

/// Is the voices npz (`voices-v1.0.bin`) already on disk? Cheap presence probe used to
/// decide whether a voice can be extracted now or its download must be kicked first.
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
        if let Some(p) = voice_pack_path("am_adam") {
            assert!(p.ends_with("kokoro-82m-coreml/ANE/am_adam.bin"));
        }
    }
}
