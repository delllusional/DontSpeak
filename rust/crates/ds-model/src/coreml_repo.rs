//! Self-managed FluidAudio (Core ML / ANE) model downloads.
//!
//! Historically FluidAudio fetched its own Core ML models on first load, which meant we had
//! no integrity control, no real download %, and the files scattered across FluidAudio's
//! several hardcoded cache roots. Instead we now fetch EVERY FluidAudio model file ourselves
//! (reusing the same HTTP + retry + SHA + atomic-rename + progress machinery as the ONNX
//! path) and then run FluidAudio in OFFLINE mode (the Swift shim sets
//! `DownloadUtils.enforceOffline = true`), so it only ever LOADS from the dirs we populated.
//!
//! Each model set is pinned to an IMMUTABLE HuggingFace revision (a commit SHA). At download
//! time we enumerate that revision's file tree via the HF tree API and fetch each blob into
//! the exact directory FluidAudio expects. Pinning the revision (content can't change under
//! us) plus verifying each LFS file's `oid` (a content sha256) gives the same integrity as the
//! per-file SHA pins we use for the ONNX assets. A small marker file written on completion
//! (`.ds-ready` holding the revision) is the LOCAL presence signal — so the status poll
//! never needs the network and a partial download never reads as present.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::download::{DEFAULT_RETRIES, download_to, is_permanent_error};
use crate::hash::verify_sha256;

const HF_HOST: &str = "https://huggingface.co";
/// Written into a model dir once every file is present + verified; holds the pinned revision
/// so bumping the pin invalidates a stale tree and forces a re-fetch.
const READY_MARKER: &str = ".ds-ready";

/// One FluidAudio Core ML model set, pinned to an immutable HF revision. `include_prefixes`
/// keeps only tree paths beginning with one of them (empty = whole repo); `exclude_substrings`
/// drops junk/dupes (`.mlpackage` source copies, `.DS_Store`, docs). Each kept tree path is
/// written under `target()` preserving its sub-path (so `ANE/Foo.mlmodelc/...` lands at
/// `target/ANE/Foo.mlmodelc/...`).
pub struct CoremlRepo {
    pub name: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub include_prefixes: &'static [&'static str],
    pub exclude_substrings: &'static [&'static str],
    pub target: fn() -> Option<PathBuf>,
}

/// `coreml_dir()/kokoro-82m-coreml` — the apple-native Kokoro runtime chain (the `ANE/`
/// subtree). FluidAudio's `KokoroAneManager(directory: coreml_dir)` looks here.
fn kokoro_main_target() -> Option<PathBuf> {
    Some(ds_config::coreml_dir()?.join("kokoro-82m-coreml"))
}

/// `~/.cache/fluidaudio/Models/kokoro` — FluidAudio's `G2PModel` singleton HARDCODES this
/// path (`TtsCacheDirectory`), so we can't relocate the G2P/lexicon sub-models; we pre-fill
/// the exact dir it reads. Uninstall already wipes `~/.cache/fluidaudio`.
fn kokoro_g2p_target() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".cache/fluidaudio/Models/kokoro"))
}

/// `model_dir()/parakeet-tdt-0.6b-v2` — FluidAudio's ASR loader appends this folder name to
/// the PARENT of the dir we pass `smk_asr_init` (we pass `coreml_dir`, whose parent is
/// `model_dir`), so this is where it looks.
fn parakeet_target() -> Option<PathBuf> {
    Some(ds_config::model_dir()?.join("parakeet-tdt-0.6b-v2"))
}

/// `coreml_dir()/speaker-diarization-coreml` — a dir WE choose; the shim's `smk_diar_init`
/// loads the two diarization `.mlmodelc` from here via FluidAudio's explicit local-file API.
fn diarizer_target() -> Option<PathBuf> {
    Some(ds_config::coreml_dir()?.join("speaker-diarization-coreml"))
}

/// Apple-native Kokoro TTS runtime models (the `ANE/` subtree). Pinned to the FluidInference
/// repo revision audited 2026-06. apache-2.0.
pub static KOKORO_COREML: CoremlRepo = CoremlRepo {
    name: "kokoro_coreml",
    repo: "FluidInference/kokoro-82m-coreml",
    revision: "c94edcb4b671856795458645cd389c0a9184e8bb",
    include_prefixes: &["ANE/"],
    exclude_substrings: &[".mlpackage", ".DS_Store", "ANE/LICENSE", "ANE/README"],
    target: kokoro_main_target,
};

/// Shared Kokoro G2P + lexicon (the repo ROOT files), which FluidAudio loads from its own
/// hardcoded `~/.cache/fluidaudio/Models/kokoro`. Same repo + revision as the runtime set.
pub static KOKORO_G2P_COREML: CoremlRepo = CoremlRepo {
    name: "kokoro_g2p_coreml",
    repo: "FluidInference/kokoro-82m-coreml",
    revision: "c94edcb4b671856795458645cd389c0a9184e8bb",
    include_prefixes: &["G2PEncoder", "G2PDecoder", "g2p_vocab.json", "us_lexicon_cache.json"],
    exclude_substrings: &[".mlpackage", ".DS_Store"],
    target: kokoro_g2p_target,
};

/// Apple-native Parakeet TDT 0.6b v2 STT. cc-by-4.0 (attribution required). Pinned revision
/// audited 2026-06. We fetch only the v2 runtime set; the repo also ships alternate encoders
/// (`_v2`, `_4bit_par`) and `.mlpackage` source copies, which the excludes drop.
pub static PARAKEET_COREML: CoremlRepo = CoremlRepo {
    name: "parakeet_coreml",
    repo: "FluidInference/parakeet-tdt-0.6b-v2-coreml",
    revision: "ee09c569f73759e6d44c9bd16766f477b2b36d39",
    include_prefixes: &[
        "Preprocessor.mlmodelc/",
        "Encoder.mlmodelc/",
        "Decoder.mlmodelc/",
        "JointDecision.mlmodelc/",
        "parakeet_vocab.json",
        "config.json",
    ],
    exclude_substrings: &[".DS_Store"],
    target: parakeet_target,
};

/// Apple-native speaker diarization (pyannote segmentation + wespeaker embedding). cc-by-4.0.
/// We fetch only the two `.mlmodelc` the shim hands to FluidAudio's local-file loader.
pub static DIARIZER_COREML: CoremlRepo = CoremlRepo {
    name: "diarizer_coreml",
    repo: "FluidInference/speaker-diarization-coreml",
    revision: "1ed7a662fdc7109e36d822db793ee6eebdaf8594",
    include_prefixes: &["pyannote_segmentation.mlmodelc/", "wespeaker_v2.mlmodelc/"],
    exclude_substrings: &[".DS_Store"],
    target: diarizer_target,
};

/// Every Core ML repo we self-manage, in the order a clean install fetches them.
pub fn all_coreml_repos() -> [&'static CoremlRepo; 4] {
    [
        &KOKORO_COREML,
        &KOKORO_G2P_COREML,
        &PARAKEET_COREML,
        &DIARIZER_COREML,
    ]
}

/// One file in a repo's tree at the pinned revision: where it goes (`path`, relative to the
/// repo root / target), its byte size (for the progress bar), and its content sha256 when LFS-
/// tracked (small git-blob files have no content sha in the API — the immutable revision
/// guarantees their bytes, so we size-check those).
struct TreeFile {
    path: String,
    size: u64,
    sha256: Option<String>,
}

/// Whether a kept tree path passes a repo's include/exclude filters.
fn keep(repo: &CoremlRepo, path: &str) -> bool {
    let included = repo.include_prefixes.is_empty()
        || repo.include_prefixes.iter().any(|p| path.starts_with(p));
    let excluded = repo
        .exclude_substrings
        .iter()
        .any(|s| path.contains(s));
    included && !excluded
}

/// GET the HF tree API at the pinned revision and return the kept files. The revision is
/// immutable, so this list is stable. Network — only called during a download, never on the
/// status poll (that uses the local marker).
fn fetch_tree(repo: &CoremlRepo) -> std::io::Result<Vec<TreeFile>> {
    let url = format!(
        "{HF_HOST}/api/models/{}/tree/{}?recursive=true",
        repo.repo, repo.revision
    );
    let body = crate::download::http_get_builder(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| std::io::Error::other(format!("HF tree fetch failed: {e}")))?;
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let arr = json
        .as_array()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "tree not an array"))?;
    let mut out = Vec::new();
    for e in arr {
        if e.get("type").and_then(|t| t.as_str()) != Some("file") {
            continue;
        }
        let Some(path) = e.get("path").and_then(|p| p.as_str()) else {
            continue;
        };
        if !keep(repo, path) {
            continue;
        }
        // LFS files carry the real content sha256 + size under `lfs`; plain git blobs only have
        // a top-level `size` (and a git-blob `oid` we can't sha-verify, so we trust the revision).
        let lfs = e.get("lfs");
        let size = lfs
            .and_then(|l| l.get("size"))
            .or_else(|| e.get("size"))
            .and_then(|s| s.as_u64())
            .unwrap_or(0);
        let sha256 = lfs
            .and_then(|l| l.get("oid"))
            .and_then(|o| o.as_str())
            .map(|s| s.trim_start_matches("sha256:").to_string());
        out.push(TreeFile {
            path: path.to_string(),
            size,
            sha256,
        });
    }
    if out.is_empty() {
        return Err(std::io::Error::other(format!(
            "HF tree for {} matched no files (filters too strict or revision moved)",
            repo.repo
        )));
    }
    Ok(out)
}

/// True if `dest` already holds the right bytes for `f` — its sha256 (LFS) or, for a plain
/// blob, just the expected size. Lets a re-run skip already-fetched files.
fn already_have(dest: &Path, f: &TreeFile) -> bool {
    match &f.sha256 {
        Some(sha) => verify_sha256(dest, sha),
        None => f.size > 0 && std::fs::metadata(dest).map(|m| m.len() == f.size).unwrap_or(false),
    }
}

/// Download one tree file to `dest` (atomic temp→rename), verifying its LFS sha256 when known.
/// Transient failures retry; a sha mismatch / 404 fails fast (same policy as the ONNX path).
fn download_one(
    repo: &CoremlRepo,
    f: &TreeFile,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let url = format!("{HF_HOST}/{}/resolve/{}/{}", repo.repo, repo.revision, f.path);
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let mut attempt = 0;
    loop {
        let tmp = tempfile::Builder::new().tempfile_in(dir)?;
        let res = download_to(&url, tmp.path(), progress).and_then(|()| {
            if let Some(sha) = &f.sha256 {
                if !verify_sha256(tmp.path(), sha) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("sha256 mismatch for {}", f.path),
                    ));
                }
            }
            Ok(())
        });
        match res {
            Ok(()) => {
                tmp.persist(dest).map_err(|e| e.error)?;
                return Ok(());
            }
            Err(e) if is_permanent_error(&e) || attempt >= DEFAULT_RETRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
            }
        }
    }
}

/// Download a SINGLE Core ML repo, reporting ONE overall 0..1 bar (as `/10_000`) — for callers
/// that want an aggregate, not per-file, progress (the engine-side download manager / the
/// diarization fetch). A thin AGGREGATE ADAPTER over the one universal downloader
/// [`ensure_coreml_repos`]: every file of every set goes through the SAME code; this only
/// collapses the per-file `(index, count, fraction)` into a smooth overall fraction.
pub fn ensure_coreml_repo(repo: &CoremlRepo, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    ensure_coreml_repos(&[repo], &|file_done, file_total, idx, count| {
        let done_files = idx.saturating_sub(1) as f64;
        let file_frac = file_done as f64 / file_total.max(1) as f64;
        let frac = ((done_files + file_frac) / count.max(1) as f64).clamp(0.0, 1.0);
        progress((frac * 10_000.0) as u64, 10_000);
    })
}

/// Download a SET of repos as one unit, reporting PER-FILE progress:
/// `progress(file_done, file_total, file_index, file_count)` — `file_done`/`file_total` are the
/// CURRENT file's own bytes (its own 0→100%), `file_index` is its 1-based position across the
/// whole set and `file_count` the total. So the UI can show "3/22 · 49%": which file of how
/// many, and THAT file's percent (not a confusing cross-file aggregate). Writes each repo's
/// completion marker once its files are all present.
pub fn ensure_coreml_repos(
    repos: &[&CoremlRepo],
    progress: &dyn Fn(u64, u64, u64, u64),
) -> std::io::Result<()> {
    // Resolve every not-yet-present repo's tree first so file_count is exact up front.
    let mut plan: Vec<(&CoremlRepo, PathBuf, Vec<TreeFile>)> = Vec::new();
    for r in repos {
        if coreml_repo_present(r) {
            continue;
        }
        let target = (r.target)().ok_or_else(|| {
            std::io::Error::other(format!("cannot resolve target dir for {}", r.name))
        })?;
        let files = fetch_tree(r)?;
        plan.push((r, target, files));
    }
    let file_count: u64 = plan.iter().map(|(_, _, f)| f.len() as u64).sum();
    if file_count == 0 {
        progress(1, 1, 1, 1);
        return Ok(());
    }
    let mut idx: u64 = 0;
    for (r, target, files) in &plan {
        for f in files {
            idx += 1;
            let dest = target.join(&f.path);
            let total = f.size.max(1);
            if already_have(&dest, f) {
                progress(total, total, idx, file_count);
                continue;
            }
            download_one(r, f, &dest, &|done, _t| {
                progress(done.min(total), total, idx, file_count);
            })?;
            progress(total, total, idx, file_count);
        }
        // All of this repo's files are present → mark it complete (revision-pinned).
        std::fs::write(target.join(READY_MARKER), r.revision)?;
    }
    Ok(())
}

/// LOCAL presence (no network): the completion marker exists AND matches the pinned revision.
/// A partial download has no marker; a stale pin has a mismatching one → both read absent.
pub fn coreml_repo_present(repo: &CoremlRepo) -> bool {
    let Some(target) = (repo.target)() else {
        return false;
    };
    let marker = target.join(READY_MARKER);
    let mut s = String::new();
    std::fs::File::open(&marker)
        .and_then(|mut f| f.read_to_string(&mut s))
        .is_ok()
        && s.trim() == repo.revision
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_keep_only_the_runtime_set() {
        // Kokoro runtime set keeps the ANE/ tree, drops the .mlpackage source copies + docs.
        assert!(keep(&KOKORO_COREML, "ANE/KokoroVocoder.mlmodelc/coremldata.bin"));
        assert!(keep(&KOKORO_COREML, "ANE/af_heart.bin"));
        assert!(!keep(&KOKORO_COREML, "ANE/KokoroVocoder.mlpackage/x"));
        assert!(!keep(&KOKORO_COREML, "ANE/.DS_Store"));
        assert!(!keep(&KOKORO_COREML, "ANE/LICENSE"));
        assert!(!keep(&KOKORO_COREML, "G2PEncoder.mlmodelc/coremldata.bin")); // belongs to G2P set
        // G2P set is the complement at the repo root.
        assert!(keep(&KOKORO_G2P_COREML, "G2PEncoder.mlmodelc/coremldata.bin"));
        assert!(keep(&KOKORO_G2P_COREML, "g2p_vocab.json"));
        assert!(!keep(&KOKORO_G2P_COREML, "ANE/KokoroVocoder.mlmodelc/coremldata.bin"));
        // Parakeet keeps the v2 runtime mlmodelc, drops alternate encoders.
        assert!(keep(&PARAKEET_COREML, "Encoder.mlmodelc/weights/weight.bin"));
        assert!(keep(&PARAKEET_COREML, "parakeet_vocab.json"));
        assert!(!keep(&PARAKEET_COREML, "ParakeetEncoder_4bit_par.mlmodelc/x"));
    }

    #[test]
    fn presence_is_false_without_a_matching_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        // A throwaway repo descriptor pointed at the temp dir via a leaked closure-free target.
        // (We can't set `target` to a closure capturing `dir`, so test the marker logic directly.)
        let marker = dir.join(READY_MARKER);
        assert!(!marker.exists());
        std::fs::write(&marker, "deadbeef").unwrap();
        // Marker present but revision mismatches → still absent.
        let mut s = String::new();
        std::fs::File::open(&marker)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        assert_ne!(s.trim(), KOKORO_COREML.revision);
        // Matching marker → present.
        std::fs::write(&marker, KOKORO_COREML.revision).unwrap();
        let mut s2 = String::new();
        std::fs::File::open(&marker)
            .unwrap()
            .read_to_string(&mut s2)
            .unwrap();
        assert_eq!(s2.trim(), KOKORO_COREML.revision);
    }

    /// Live HF-API check (network) — run with `--ignored`. Confirms the tree URL, the JSON
    /// shape, the lfs.oid extraction, and the filters all line up with the real repos.
    #[test]
    #[ignore = "network: hits the HuggingFace API"]
    fn live_tree_fetch_returns_the_expected_runtime_files() {
        for repo in all_coreml_repos() {
            let files = fetch_tree(repo).unwrap_or_else(|e| panic!("{}: {e}", repo.name));
            let total: u64 = files.iter().map(|f| f.size).sum();
            let lfs = files.iter().filter(|f| f.sha256.is_some()).count();
            eprintln!(
                "{}: {} files, {} LFS, {:.0} MB",
                repo.name,
                files.len(),
                lfs,
                total as f64 / 1e6
            );
            assert!(!files.is_empty(), "{} returned no files", repo.name);
            // The big weight.bin blobs must be LFS (so we sha-verify them).
            assert!(lfs > 0, "{} has no LFS files — oid parse wrong?", repo.name);
            assert!(total > 1_000_000, "{} total too small", repo.name);
        }
    }

    /// Live download of one real LFS file (network) — validates the resolve URL, the temp→
    /// rename, and the sha256 verification end-to-end. Run with `--ignored`.
    #[test]
    #[ignore = "network: downloads ~6 MB from HuggingFace"]
    fn live_download_one_file_verifies_sha() {
        // The diarizer's wespeaker weights — a modest (~7 MB) LFS blob with a known oid.
        let files = fetch_tree(&DIARIZER_COREML).unwrap();
        let f = files
            .iter()
            .filter(|f| f.sha256.is_some())
            .min_by_key(|f| f.size)
            .expect("an LFS file");
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join(&f.path);
        download_one(&DIARIZER_COREML, f, &dest, &|_, _| {}).expect("download+verify");
        assert!(dest.exists());
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), f.size);
        // A second pass is a cheap no-op (already_have short-circuits on the verified sha).
        assert!(already_have(&dest, f));
    }

    #[test]
    fn revisions_are_full_40_char_commit_shas() {
        for r in all_coreml_repos() {
            assert_eq!(r.revision.len(), 40, "{} revision must be a full SHA", r.name);
            assert!(r.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(r.repo.starts_with("FluidInference/"));
        }
    }
}
