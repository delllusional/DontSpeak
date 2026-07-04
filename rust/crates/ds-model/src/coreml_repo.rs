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
//! per-file SHA pins we use for the ONNX assets. Plain (non-LFS) git-blob files — `config.json`,
//! `g2p_vocab.json`, `parakeet_vocab.json`, … — carry no `lfs.oid`, but the tree API's
//! top-level `oid` on those entries IS the git blob hash of their real bytes
//! (`git hash-object`'s `sha1("blob {len}\0" + content)`); we verify THAT (plus the exact
//! size) so these files get the same "downloaded bytes match the pinned revision" guarantee
//! as the LFS ones, instead of the zero verification they used to get (see
//! `git_blob_sha1_hex`). A small marker file written on completion (`.ds-ready` holding the
//! revision) is the LOCAL presence signal — so the status poll never needs the network and a
//! partial download never reads as present.

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
    /// Library-catalog metadata (the macOS-only Apple-Silicon model sets shown in the
    /// Libraries tab). The license lives WITH the files here — same can't-drift principle
    /// as [`crate::urls::Project`] — so `crate::libraries` can render these alongside the
    /// downloaded ONNX assets without a second, drift-prone source. `display_name`/`usage`
    /// empty ⇒ the set is an internal sub-component of another entry (e.g. the Kokoro G2P
    /// sub-models share the Kokoro repo) and is folded into it, not listed on its own.
    pub display_name: &'static str,
    pub usage: &'static str,
    pub license: &'static str,
    pub license_url: &'static str,
}

/// On-disk folder name (== the FluidAudio repo's last path segment) of the apple-native
/// Kokoro Core ML chain. The ONE source of truth for this folder name — both the download
/// target and the voice-pack materialize dir derive from it, so they can't drift apart.
pub const KOKORO_COREML_DIR_NAME: &str = "kokoro-82m-coreml";

/// The HuggingFace repo + pinned revision shared by BOTH Kokoro Core ML sets — the runtime
/// ANE chain ([`KOKORO_COREML`]) and the G2P/lexicon sub-models ([`KOKORO_G2P_COREML`]) live
/// in the SAME repo at the SAME commit (they're just different sub-paths of one tree). ONE
/// source of truth so a revision bump can't update one set and silently leave the other
/// pinned to a stale tree (which would mix model files across two commits). The
/// `kokoro_sets_share_one_repo_and_revision` test pins both statics to these.
const KOKORO_HF_REPO: &str = "FluidInference/kokoro-82m-coreml";
const KOKORO_HF_REVISION: &str = "c94edcb4b671856795458645cd389c0a9184e8bb";

/// `coreml_dir()/kokoro-82m-coreml` — the apple-native Kokoro runtime chain (the `ANE/`
/// subtree). FluidAudio's `KokoroAneManager(directory: coreml_dir)` looks here.
fn kokoro_main_target() -> Option<PathBuf> {
    Some(ds_config::coreml_dir()?.join(KOKORO_COREML_DIR_NAME))
}

/// `coreml_dir()/kokoro-82m-coreml/ANE` — the exact directory FluidAudio's ANE Kokoro chain
/// LOADS voice packs from (the `<voice>.bin` files; `af_heart.bin` ships here). Because we
/// init the shim with [`ds_config::coreml_dir`] (NOT FluidAudio's empty-default cache),
/// `KokoroAneManager` resolves voice packs UNDER this tree — so any voice pack we materialize
/// for the ANE path MUST land here, or the shim 404s and silently falls back to `af_heart`.
pub fn kokoro_ane_dir() -> Option<PathBuf> {
    Some(kokoro_main_target()?.join("ANE"))
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

/// On-disk folder name of the apple-native STREAMING STT set (Parakeet EOU 120M).
pub const PARAKEET_EOU_DIR_NAME: &str = "parakeet-eou-streaming";
/// The EOU variant subfolder we fetch — MIRRORS the chunk size the shim requests
/// (`StreamingEouAsrManager(chunkSize: .ms160)` in `shim.swift`), so they can't drift. 160ms is
/// FluidAudio's lowest-latency EOU variant (~6 partials/sec) — picked for a snappier live overlay.
pub const PARAKEET_EOU_VARIANT: &str = "160ms";

/// `coreml_dir()/parakeet-eou-streaming` — download target for the streaming EOU set. The repo's
/// `320ms/` subtree lands under here, so the loadable model dir is the `320ms` subfolder (see
/// [`parakeet_eou_dir`]).
fn parakeet_eou_target() -> Option<PathBuf> {
    Some(ds_config::coreml_dir()?.join(PARAKEET_EOU_DIR_NAME))
}

/// The exact dir FluidAudio's `StreamingEouAsrManager.loadModels(from:)` reads the streaming
/// `.mlmodelc` set + `vocab.json` from (handed to the shim's `smk_asr_stream_start`). The download
/// writes the repo's `320ms/` subtree under `parakeet_eou_target`, so the loadable dir is that
/// subfolder. ONE source of truth so the download target and the shim load path (via
/// `ds_stt::coreml::CoremlStreamer`) can't drift.
pub fn parakeet_eou_dir() -> Option<PathBuf> {
    Some(parakeet_eou_target()?.join(PARAKEET_EOU_VARIANT))
}

/// On-disk folder name of the speaker-diarization Core ML set (== the FluidAudio repo's last
/// path segment). ONE source of truth: the download target, the presence check, and the Swift
/// shim's load path (`shim.swift`, kept in sync by comment + the `diarization_model_names_match_prefixes`
/// test) all derive from this so they can't drift.
pub const DIARIZATION_COREML_DIR_NAME: &str = "speaker-diarization-coreml";
/// The two `.mlmodelc` bundles the diarizer shim loads from [`diarization_dir`]. Mirrored as
/// literals in the Swift shim (`smk_diar_init`) — a Rust-side rename trips
/// `diarization_model_names_match_prefixes` so the cross-language pair can't silently drift.
pub const DIARIZATION_SEGMENTATION_MODEL: &str = "pyannote_segmentation.mlmodelc";
pub const DIARIZATION_EMBEDDING_MODEL: &str = "wespeaker_v2.mlmodelc";

/// `coreml_dir()/speaker-diarization-coreml` — a dir WE choose; the shim's `smk_diar_init`
/// loads the two diarization `.mlmodelc` ([`DIARIZATION_SEGMENTATION_MODEL`] /
/// [`DIARIZATION_EMBEDDING_MODEL`]) from here via FluidAudio's explicit local-file API.
fn diarization_target() -> Option<PathBuf> {
    Some(ds_config::coreml_dir()?.join(DIARIZATION_COREML_DIR_NAME))
}

/// Public accessor for the diarization model dir (== `diarization_target`) — the ONE place
/// Rust consumers resolve where the two diarization `.mlmodelc` live.
pub fn diarization_dir() -> Option<PathBuf> {
    diarization_target()
}

/// Apple-native Kokoro TTS runtime models (the `ANE/` subtree). Pinned to the FluidInference
/// repo revision audited 2026-06. apache-2.0.
pub static KOKORO_COREML: CoremlRepo = CoremlRepo {
    name: "kokoro_coreml",
    repo: KOKORO_HF_REPO,
    revision: KOKORO_HF_REVISION,
    include_prefixes: &["ANE/"],
    exclude_substrings: &[".mlpackage", ".DS_Store", "ANE/LICENSE", "ANE/README"],
    target: kokoro_main_target,
    display_name: "Kokoro (Core ML / ANE)",
    usage: "Apple-Silicon text-to-speech voice model (FluidAudio Core ML / ANE)",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Shared Kokoro G2P + lexicon (the repo ROOT files), which FluidAudio loads from its own
/// hardcoded `~/.cache/fluidaudio/Models/kokoro`. Same repo + revision as the runtime set.
pub static KOKORO_G2P_COREML: CoremlRepo = CoremlRepo {
    name: "kokoro_g2p_coreml",
    repo: KOKORO_HF_REPO,
    revision: KOKORO_HF_REVISION,
    include_prefixes: &[
        "G2PEncoder",
        "G2PDecoder",
        "g2p_vocab.json",
        "us_lexicon_cache.json",
    ],
    exclude_substrings: &[".mlpackage", ".DS_Store"],
    target: kokoro_g2p_target,
    // Same repo + license as the Kokoro runtime set; folded into the "Kokoro (Core ML / ANE)"
    // catalog entry (empty display_name) rather than listed as a separate library.
    display_name: "",
    usage: "",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
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
    display_name: "Parakeet (Core ML)",
    usage: "Apple-Silicon speech-to-text model (NVIDIA NeMo; Core ML export by FluidInference)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// Apple-native STREAMING speech-to-text (Parakeet EOU 120M, cache-aware encoder) — the smooth
/// real-time path the dictation overlay is built for. Without it the macOS Core ML STT silently
/// falls back to the slower offline sliding-window engine (whole-tail re-passes). We fetch only the
/// 160 ms variant the shim requests (lowest latency → ~6 partials/sec): the three runtime
/// `.mlmodelc` bundles + the vocab (NOT the `.mlpackage` sources or the conversion scripts).
/// cc-by-4.0 (NVIDIA NeMo; Core ML by FluidInference).
pub static PARAKEET_EOU_COREML: CoremlRepo = CoremlRepo {
    name: "parakeet_eou_coreml",
    repo: "FluidInference/parakeet-realtime-eou-120m-coreml",
    revision: "40a23f4c0b333aa17ad8c0f2ea47ec2347f2f355",
    include_prefixes: &[
        "160ms/streaming_encoder.mlmodelc/",
        "160ms/decoder.mlmodelc/",
        "160ms/joint_decision.mlmodelc/",
        "160ms/vocab.json",
    ],
    exclude_substrings: &[".mlpackage", ".DS_Store"],
    target: parakeet_eou_target,
    display_name: "Parakeet EOU streaming (Core ML)",
    usage: "Apple-Silicon real-time streaming speech-to-text (NVIDIA NeMo; Core ML export by FluidInference)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// Apple-native speaker diarization (pyannote segmentation + wespeaker embedding). cc-by-4.0.
/// We fetch only the two `.mlmodelc` the shim hands to FluidAudio's local-file loader.
pub static DIARIZATION_COREML: CoremlRepo = CoremlRepo {
    name: "diarization_coreml",
    repo: "FluidInference/speaker-diarization-coreml",
    revision: "1ed7a662fdc7109e36d822db793ee6eebdaf8594",
    include_prefixes: &["pyannote_segmentation.mlmodelc/", "wespeaker_v2.mlmodelc/"],
    exclude_substrings: &[".DS_Store"],
    target: diarization_target,
    display_name: "Diarization (Core ML)",
    usage: "Apple-Silicon speaker diarization (pyannote segmentation + wespeaker embedding)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// The repos one `DownloadTarget::KokoroCoreml` fetch produces — the apple-native Kokoro
/// runtime chain plus its G2P/lexicon sub-models. ONE source of truth shared by the engine's
/// download manager (fetch + presence gate) and the status row, so they can never disagree
/// about what "the Kokoro Core ML set" is.
pub static KOKORO_COREML_SET: [&CoremlRepo; 2] = [&KOKORO_COREML, &KOKORO_G2P_COREML];

/// The repos one `DownloadTarget::ParakeetCoreml` fetch produces — the streaming EOU set (the
/// smooth real-time path) plus the offline sliding-window fallback. Same one-source-of-truth
/// role as [`KOKORO_COREML_SET`].
pub static PARAKEET_COREML_SET: [&CoremlRepo; 2] = [&PARAKEET_EOU_COREML, &PARAKEET_COREML];

/// LOCAL presence of a whole set (no network): every repo's completion marker present at the
/// pinned revision — see [`is_coreml_repo_present`].
pub fn is_coreml_set_present(set: &[&CoremlRepo]) -> bool {
    set.iter().all(|r| is_coreml_repo_present(r))
}

/// Every Core ML repo we self-manage, in the order a clean install fetches them.
pub fn all_coreml_repos() -> [&'static CoremlRepo; 5] {
    [
        &KOKORO_COREML,
        &KOKORO_G2P_COREML,
        &PARAKEET_COREML,
        &PARAKEET_EOU_COREML,
        &DIARIZATION_COREML,
    ]
}

/// One file in a repo's tree at the pinned revision: where it goes (`path`, relative to the
/// repo root / target), its byte size (for the progress bar), its content sha256 when LFS-
/// tracked, and — for a plain (non-LFS) git blob — the tree API's git blob `oid` for it (see
/// `git_blob_sha1_hex`). Every file gets SOME form of content verification: `sha256` for LFS
/// blobs, `git_blob_sha1` (plus `size`) for everything else.
#[derive(Debug)]
struct TreeFile {
    path: String,
    size: u64,
    sha256: Option<String>,
    /// Plain (non-LFS) git blob hash (`git hash-object`'s id) of this file's real bytes, from
    /// the tree API's top-level `oid`. `None` for LFS-tracked entries — their top-level `oid`
    /// is the LFS *pointer* file's hash, not the real (resolved) content we download, so it
    /// would never match and must not be captured here.
    git_blob_sha1: Option<String>,
}

/// SHA-1 of `message`, lowercase hex. Hand-rolled (no `sha1` crate dependency) purely to
/// compute `git_blob_sha1_hex` below — git's object hash is SHA-1, distinct from the SHA-256
/// this crate uses everywhere else ([`crate::hash`]), so the existing hasher can't be reused
/// for it. Verified against the standard NIST test vectors and against real `git hash-object`
/// output for both single- and multi-block inputs (see the tests below) — a textbook
/// implementation, not exposed outside this module.
fn sha1_hex(message: &[u8]) -> String {
    let (mut h0, mut h1, mut h2, mut h3, mut h4): (u32, u32, u32, u32, u32) =
        (0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0);

    let bit_len: u64 = (message.len() as u64) * 8;
    let mut msg = message.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in w.iter_mut().enumerate().take(16) {
            *word = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
                _ => (b ^ c ^ d, 0xCA62C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    let mut out = String::with_capacity(40);
    for word in [h0, h1, h2, h3, h4] {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

/// Git's blob object hash for the file at `path`: `sha1("blob {len}\0" + content)` — exactly
/// what `git hash-object <path>` reports, and what the HF tree API's top-level `oid` is for a
/// plain (non-LFS) tree entry. Comparing this against that pinned `oid` catches a same-size
/// MITM substitution that a bare size check (the old behavior) would silently accept.
/// `None` only if `path` can't be read.
fn git_blob_sha1_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut data = format!("blob {}\0", bytes.len()).into_bytes();
    data.extend_from_slice(&bytes);
    Some(sha1_hex(&data))
}

/// Whether a kept tree path passes a repo's include/exclude filters.
fn keep(repo: &CoremlRepo, path: &str) -> bool {
    let included = repo.include_prefixes.is_empty()
        || repo.include_prefixes.iter().any(|p| path.starts_with(p));
    let excluded = repo.exclude_substrings.iter().any(|s| path.contains(s));
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
    fetch_tree_at(&url, repo)
}

/// The GET + parse half of [`fetch_tree`], taking the tree-API URL as a parameter so tests can
/// point it at a local mock server instead of the real `huggingface.co` — everything below this
/// is the exact production code path (same `http_get_builder`, same JSON shape, same filters).
fn fetch_tree_at(url: &str, repo: &CoremlRepo) -> std::io::Result<Vec<TreeFile>> {
    let body = crate::download::http_get_builder(url)
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
        // a top-level `size` and a top-level git-blob `oid` — which we DO verify (see
        // `git_blob_sha1_hex`) rather than trusting the revision blindly.
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
        // Only capture the top-level `oid` for a NON-LFS entry: for an LFS entry it's the
        // pointer file's hash (the small text blob actually committed to git), not the real
        // resolved content we download, so it would never match and must be left `None`.
        let git_blob_sha1 = if lfs.is_none() {
            e.get("oid").and_then(|o| o.as_str()).map(|s| s.to_string())
        } else {
            None
        };
        out.push(TreeFile {
            path: path.to_string(),
            size,
            sha256,
            git_blob_sha1,
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

/// True if `dest` already holds the right bytes for `f` — verified the same way a fresh
/// download is (see [`verify_downloaded`]): its sha256 when LFS-tracked, or, for a plain blob,
/// the exact size AND (when the tree API exposed it) the git blob oid. A same-size-but-
/// different-content plain file (e.g. one left over from an older pinned revision) therefore
/// reads as ABSENT rather than being silently kept. Lets a re-run skip already-fetched files.
fn already_have(dest: &Path, f: &TreeFile) -> bool {
    verify_downloaded(dest, f).is_ok()
}

/// Verify a downloaded (or previously-downloaded) tree file's bytes at `path` against
/// everything the pinned revision's tree API told us about it: an LFS file's real content
/// sha256, or — for a plain git blob (no LFS sha256) — its exact size AND, when the tree API
/// exposed it, the git blob oid (`git_blob_sha1_hex`) of the bytes on disk. This is the
/// SINGLE place both a fresh download ([`download_one`]) and the presence check
/// ([`already_have`]) apply integrity verification, so they can't drift apart. Plain files
/// used to get NO verification at all here (not even a size check); they now get the same
/// "matches the pinned revision" guarantee as LFS files.
fn verify_downloaded(path: &Path, f: &TreeFile) -> std::io::Result<()> {
    if let Some(sha) = &f.sha256 {
        return if verify_sha256(path, sha) {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("sha256 mismatch for {}", f.path),
            ))
        };
    }
    let len = std::fs::metadata(path)?.len();
    if f.size > 0 && len != f.size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "size mismatch for {}: got {len} bytes, expected {}",
                f.path, f.size
            ),
        ));
    }
    if let Some(oid) = &f.git_blob_sha1 {
        let matches = git_blob_sha1_hex(path).is_some_and(|got| got.eq_ignore_ascii_case(oid));
        if !matches {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("git blob oid mismatch for {}", f.path),
            ));
        }
    }
    Ok(())
}

/// Download one tree file to `dest` (atomic temp→rename), verifying it against the pinned
/// revision's tree metadata (see [`verify_downloaded`]) before persisting. Transient failures
/// retry; a verification failure / 404 fails fast (same policy as the ONNX path).
fn download_one(
    repo: &CoremlRepo,
    f: &TreeFile,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    let url = format!(
        "{HF_HOST}/{}/resolve/{}/{}",
        repo.repo, repo.revision, f.path
    );
    download_one_at(&url, f, dest, progress)
}

/// The GET-and-verify half of [`download_one`], taking the resolve URL as a parameter so tests
/// can point it at a local mock server instead of the real `huggingface.co` — everything below
/// this is the exact production code path (temp→rename, retry, [`verify_downloaded`]).
fn download_one_at(
    url: &str,
    f: &TreeFile,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir = dest.parent().unwrap_or_else(|| Path::new("."));
    let mut attempt = 0;
    loop {
        let tmp = tempfile::Builder::new().tempfile_in(dir)?;
        let res =
            download_to(url, tmp.path(), progress).and_then(|()| verify_downloaded(tmp.path(), f));
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

/// Download a SINGLE Core ML repo, reporting ONE overall 0..1 byte-progress bar — a thin
/// single-repo wrapper over the universal [`ensure_coreml_repos`] (identical byte-weighted
/// aggregate), for callers that fetch exactly one repo (the engine-side download manager / the
/// diarization fetch).
pub fn ensure_coreml_repo(repo: &CoremlRepo, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    ensure_coreml_repos(&[repo], progress)
}

/// Download a SET of repos as one unit, reporting ONE overall byte-weighted bar:
/// `progress(done_bytes, total_bytes)` where BOTH are summed across every file of every repo —
/// so the UI shows a single monotonic "Downloading `<pct>`%" over the WHOLE set (a true global
/// percent, not a per-file percent that resets each file). Writes each repo's completion marker
/// once its files are all present.
pub fn ensure_coreml_repos(
    repos: &[&CoremlRepo],
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    // Resolve every not-yet-present repo's tree first so the total byte count is exact up front.
    let mut plan: Vec<(&CoremlRepo, PathBuf, Vec<TreeFile>)> = Vec::new();
    for r in repos {
        if is_coreml_repo_present(r) {
            continue;
        }
        let target = (r.target)().ok_or_else(|| {
            std::io::Error::other(format!("cannot resolve target dir for {}", r.name))
        })?;
        let files = fetch_tree(r)?;
        plan.push((r, target, files));
    }
    // Denominator = sum of every file's size across the whole set (each floored at 1 so a
    // zero-length/unknown entry still advances the bar as it completes).
    let total_bytes: u64 = plan
        .iter()
        .flat_map(|(_, _, f)| f.iter())
        .map(|f| f.size.max(1))
        .sum();
    if total_bytes == 0 {
        progress(1, 1);
        return Ok(());
    }
    // Numerator = bytes finished so far: every completed file's full size plus the current
    // file's in-flight bytes, so the overall percent only ever moves forward.
    let mut done_bytes: u64 = 0;
    for (r, target, files) in &plan {
        for f in files {
            let dest = target.join(&f.path);
            let size = f.size.max(1);
            let base = done_bytes;
            if already_have(&dest, f) {
                done_bytes = base + size;
                progress(done_bytes, total_bytes);
                continue;
            }
            download_one(r, f, &dest, &|done, _t| {
                progress((base + done.min(size)).min(total_bytes), total_bytes);
            })?;
            done_bytes = base + size;
            progress(done_bytes, total_bytes);
        }
        // All of this repo's files are present → mark it complete (revision-pinned).
        std::fs::write(target.join(READY_MARKER), r.revision)?;
    }
    Ok(())
}

/// LOCAL presence (no network): the completion marker exists AND matches the pinned revision.
/// A partial download has no marker; a stale pin has a mismatching one → both read absent.
pub fn is_coreml_repo_present(repo: &CoremlRepo) -> bool {
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

// `CoremlRepo::target` is a plain `fn() -> Option<PathBuf>` (a real static's target function
// captures nothing), so a test can't just close over a tempdir the way a closure would — this
// thread-local + zero-capture accessor is the seam that lets `is_coreml_repo_present_*` tests point
// a throwaway `CoremlRepo` at a tempdir and call the REAL function instead of re-implementing its
// marker-comparison logic by hand. Thread-local (not a shared `Mutex`, unlike `download.rs`'s
// `PREFETCH_DIR`) because each `#[test]` fn runs on its own thread by default, so there's no
// cross-test interference to guard against.
#[cfg(test)]
thread_local! {
    static TEST_TARGET_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_target() -> Option<PathBuf> {
    TEST_TARGET_DIR.with(|t| t.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diarization_model_names_match_prefixes() {
        // The two `.mlmodelc` consts (mirrored as literals in the Swift `smk_diar_init`
        // loader) MUST equal the download include-prefixes (with the dir-style trailing
        // slash). A rename of either const without updating the prefixes — or vice versa —
        // would make Rust download to one name and the Swift shim load another (the
        // `ane_voices`-class bug, but cross-language so the compiler can't catch it). This
        // test catches the Rust half; the Swift comment pins the other.
        assert_eq!(
            DIARIZATION_COREML.include_prefixes,
            [
                format!("{DIARIZATION_SEGMENTATION_MODEL}/"),
                format!("{DIARIZATION_EMBEDDING_MODEL}/"),
            ]
        );
        // The repo path's last segment is the on-disk folder name we materialize into.
        assert!(
            DIARIZATION_COREML
                .repo
                .ends_with(DIARIZATION_COREML_DIR_NAME)
        );
    }

    #[test]
    fn kokoro_sets_share_one_repo_and_revision() {
        // The runtime ANE chain and the G2P/lexicon set are two sub-paths of ONE HF tree;
        // they MUST stay pinned to the same repo + commit, or a half-bumped revision mixes
        // model files across commits. Both derive from the shared consts, so this can't
        // drift — and the repo's last segment is the on-disk folder name we materialize into.
        assert_eq!(KOKORO_COREML.repo, KOKORO_G2P_COREML.repo);
        assert_eq!(KOKORO_COREML.revision, KOKORO_G2P_COREML.revision);
        assert_eq!(KOKORO_COREML.repo, KOKORO_HF_REPO);
        assert!(KOKORO_HF_REPO.ends_with(KOKORO_COREML_DIR_NAME));
    }

    #[test]
    fn filters_keep_only_the_runtime_set() {
        // Kokoro runtime set keeps the ANE/ tree, drops the .mlpackage source copies + docs.
        assert!(keep(
            &KOKORO_COREML,
            "ANE/KokoroVocoder.mlmodelc/coremldata.bin"
        ));
        assert!(keep(&KOKORO_COREML, "ANE/af_heart.bin"));
        assert!(!keep(&KOKORO_COREML, "ANE/KokoroVocoder.mlpackage/x"));
        assert!(!keep(&KOKORO_COREML, "ANE/.DS_Store"));
        assert!(!keep(&KOKORO_COREML, "ANE/LICENSE"));
        assert!(!keep(&KOKORO_COREML, "G2PEncoder.mlmodelc/coremldata.bin")); // belongs to G2P set
        // G2P set is the complement at the repo root.
        assert!(keep(
            &KOKORO_G2P_COREML,
            "G2PEncoder.mlmodelc/coremldata.bin"
        ));
        assert!(keep(&KOKORO_G2P_COREML, "g2p_vocab.json"));
        assert!(!keep(
            &KOKORO_G2P_COREML,
            "ANE/KokoroVocoder.mlmodelc/coremldata.bin"
        ));
        // Parakeet keeps the v2 runtime mlmodelc, drops alternate encoders.
        assert!(keep(
            &PARAKEET_COREML,
            "Encoder.mlmodelc/weights/weight.bin"
        ));
        assert!(keep(&PARAKEET_COREML, "parakeet_vocab.json"));
        assert!(!keep(
            &PARAKEET_COREML,
            "ParakeetEncoder_4bit_par.mlmodelc/x"
        ));
    }

    #[test]
    fn sha1_hex_matches_known_vectors() {
        // Standard NIST SHA-1 test vectors, plus a 56-byte input (crosses the single-block
        // padding boundary, exercising the multi-block chunking path).
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn git_blob_sha1_hex_matches_real_git_hash_object() {
        let tmp = tempfile::tempdir().unwrap();
        // Empty file: `git hash-object` of an empty file is this exact, famous id — every git
        // repo has an object with it.
        let empty = tmp.path().join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            git_blob_sha1_hex(&empty).unwrap(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        // A short file — cross-checked against real `git hash-object`'s output for the literal
        // bytes "abc\n".
        let short = tmp.path().join("short");
        std::fs::write(&short, b"abc\n").unwrap();
        assert_eq!(
            git_blob_sha1_hex(&short).unwrap(),
            "8baef1b4abc478178b004d62031cf7fe6db6f903"
        );
    }

    #[test]
    fn verify_downloaded_checks_lfs_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("weights.bin");
        std::fs::write(&p, b"weights").unwrap();
        let f = TreeFile {
            path: "weights.bin".to_string(),
            size: 7,
            sha256: Some(crate::hash::sha256_hex(b"weights")),
            git_blob_sha1: None,
        };
        assert!(verify_downloaded(&p, &f).is_ok());

        let f_bad = TreeFile {
            path: "weights.bin".to_string(),
            size: 7,
            sha256: Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into()),
            git_blob_sha1: None,
        };
        let err = verify_downloaded(&p, &f_bad).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// A plain (non-LFS) blob with NO metadata beyond size used to get ZERO verification after
    /// download — this pins the fix: a size mismatch is now caught.
    #[test]
    fn verify_downloaded_rejects_size_mismatch_for_plain_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.json");
        std::fs::write(&p, b"{}").unwrap();
        let f = TreeFile {
            path: "config.json".to_string(),
            size: 999,
            sha256: None,
            git_blob_sha1: None,
        };
        let err = verify_downloaded(&p, &f).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// The high-severity gap this closes: a MITM (or a stale same-size leftover from an older
    /// pinned revision) that substitutes bytes of the SAME length would pass a size-only check
    /// — the pre-fix behavior — but must be rejected once the tree API exposed a git blob oid.
    #[test]
    fn verify_downloaded_rejects_git_blob_oid_mismatch_even_at_the_right_size() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("parakeet_vocab.json");
        std::fs::write(&p, b"AAAAAAAAAA").unwrap(); // 10 tampered bytes, right length
        let f = TreeFile {
            path: "parakeet_vocab.json".to_string(),
            size: 10,
            sha256: None,
            git_blob_sha1: Some("f".repeat(40)), // not this content's real oid
        };
        let err = verify_downloaded(&p, &f).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn verify_downloaded_accepts_the_real_git_blob_oid() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("parakeet_vocab.json");
        let content: &[u8] = b"real, untampered bytes";
        std::fs::write(&p, content).unwrap();
        let oid = git_blob_sha1_hex(&p).unwrap();
        let f = TreeFile {
            path: "parakeet_vocab.json".to_string(),
            size: content.len() as u64,
            sha256: None,
            git_blob_sha1: Some(oid),
        };
        assert!(verify_downloaded(&p, &f).is_ok());
    }

    /// `already_have` (the re-run skip-check) must apply the SAME content verification as a
    /// fresh download, not just a size check — a same-size-but-different-content leftover from
    /// an older pinned revision must read as ABSENT (so it gets re-fetched and the completion
    /// marker doesn't get written over a stale file), matching `verify_downloaded` exactly
    /// since `already_have` now delegates to it.
    #[test]
    fn already_have_checks_content_not_just_size_for_plain_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("g2p_vocab.json");
        let content: &[u8] = b"{\"hello\":\"world\"}";
        std::fs::write(&dest, content).unwrap();
        let real_oid = git_blob_sha1_hex(&dest).unwrap();

        let matching = TreeFile {
            path: "g2p_vocab.json".to_string(),
            size: content.len() as u64,
            sha256: None,
            git_blob_sha1: Some(real_oid),
        };
        assert!(already_have(&dest, &matching));

        let tampered = TreeFile {
            path: "g2p_vocab.json".to_string(),
            size: content.len() as u64,
            sha256: None,
            git_blob_sha1: Some("0".repeat(40)),
        };
        assert!(!already_have(&dest, &tampered));

        let wrong_size = TreeFile {
            path: "g2p_vocab.json".to_string(),
            size: content.len() as u64 + 1,
            sha256: None,
            git_blob_sha1: None,
        };
        assert!(!already_have(&dest, &wrong_size));
    }

    #[test]
    fn presence_is_false_without_a_matching_marker() {
        let tmp = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = Some(tmp.path().to_path_buf()));
        let repo = CoremlRepo {
            name: "test_repo",
            repo: "Test/test-repo",
            revision: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            include_prefixes: &[],
            exclude_substrings: &[],
            target: test_target,
            display_name: "",
            usage: "",
            license: "",
            license_url: "",
        };

        // No marker at all → absent.
        assert!(!is_coreml_repo_present(&repo), "no marker yet");

        // Marker present but revision mismatches → still absent.
        std::fs::write(tmp.path().join(READY_MARKER), "some-other-revision").unwrap();
        assert!(
            !is_coreml_repo_present(&repo),
            "mismatched revision reads as absent"
        );

        // Matching marker → present.
        std::fs::write(tmp.path().join(READY_MARKER), repo.revision).unwrap();
        assert!(
            is_coreml_repo_present(&repo),
            "matching marker reads as present"
        );
    }

    /// Hermetic counterpart to `live_tree_fetch_returns_the_expected_runtime_files`: a local
    /// `httpmock` server stands in for `huggingface.co`, so the JSON-shape handling — LFS
    /// `oid` extraction, plain git-blob `oid` extraction, `directory`-type skipping, and the
    /// include/exclude filters — gets real coverage on every `cargo test`, not just an
    /// occasional `--ignored` run against the real API.
    #[test]
    fn fetch_tree_at_parses_lfs_and_plain_blobs_and_applies_filters() {
        let server = httpmock::MockServer::start();
        let lfs_sha = "a".repeat(64);
        let body = serde_json::json!([
            // LFS-tracked runtime weight: kept (matches KOKORO_COREML's "ANE/" prefix), the
            // real content sha256 comes from `lfs.oid`, NOT the top-level (pointer-file) oid.
            {
                "type": "file",
                "path": "ANE/KokoroVocoder.mlmodelc/weights.bin",
                "size": 12345,
                "oid": "pointerfileblobsha1notusedforlfs01",
                "lfs": { "oid": format!("sha256:{lfs_sha}"), "size": 12345 }
            },
            // Plain (non-LFS) blob: kept, verified via the top-level git-blob `oid` instead.
            {
                "type": "file",
                "path": "ANE/af_heart.bin",
                "size": 20,
                "oid": "deadbeefcafef00d"
            },
            // Excluded by substring filter (.mlpackage source copy) — must be dropped.
            {
                "type": "file",
                "path": "ANE/KokoroVocoder.mlpackage/x",
                "size": 5,
                "oid": "ignored"
            },
            // Not included by prefix (belongs to the G2P sub-tree, not "ANE/") — dropped.
            {
                "type": "file",
                "path": "G2PEncoder.mlmodelc/coremldata.bin",
                "size": 5,
                "oid": "ignored"
            },
            // A `directory` entry — never a file, must be skipped regardless of path/filters.
            {
                "type": "directory",
                "path": "ANE/KokoroVocoder.mlmodelc",
                "size": 0,
                "oid": "ignored"
            }
        ]);
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/api/models/FluidInference/kokoro-82m-coreml/tree/c94edcb4b671856795458645cd389c0a9184e8bb")
                .query_param("recursive", "true");
            then.status(200).json_body(body);
        });

        let url = server.url(
            "/api/models/FluidInference/kokoro-82m-coreml/tree/c94edcb4b671856795458645cd389c0a9184e8bb?recursive=true",
        );
        let files = fetch_tree_at(&url, &KOKORO_COREML).expect("tree fetch parses");
        mock.assert();

        assert_eq!(
            files.len(),
            2,
            "only the two ANE/-prefixed, non-excluded files survive: {files:?}",
        );
        let weights = files
            .iter()
            .find(|f| f.path == "ANE/KokoroVocoder.mlmodelc/weights.bin")
            .expect("LFS weights file kept");
        assert_eq!(weights.size, 12345);
        assert_eq!(weights.sha256.as_deref(), Some(lfs_sha.as_str()));
        assert!(
            weights.git_blob_sha1.is_none(),
            "LFS entries must not capture the pointer-file's top-level oid"
        );
        let heart = files
            .iter()
            .find(|f| f.path == "ANE/af_heart.bin")
            .expect("plain blob kept");
        assert_eq!(heart.size, 20);
        assert!(heart.sha256.is_none(), "plain blobs have no lfs.oid");
        assert_eq!(heart.git_blob_sha1.as_deref(), Some("deadbeefcafef00d"));
    }

    /// A tree response where every entry is filtered out (or the repo/revision moved) must
    /// error rather than silently report zero files to download.
    #[test]
    fn fetch_tree_at_errors_when_no_files_match() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/empty-tree");
            then.status(200).json_body(serde_json::json!([
                { "type": "file", "path": "G2PEncoder.mlmodelc/coremldata.bin", "size": 5, "oid": "x" }
            ]));
        });
        let err = fetch_tree_at(&server.url("/empty-tree"), &KOKORO_COREML).unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("matched no files"));
    }

    /// A non-2xx tree-API response (moved/deleted repo, bad revision) must surface as an
    /// error, not a panic or a silently-empty file list.
    #[test]
    fn fetch_tree_at_errors_on_http_error_status() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/missing-repo");
            then.status(404).body("Not Found");
        });
        let err = fetch_tree_at(&server.url("/missing-repo"), &KOKORO_COREML).unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("HF tree fetch failed"));
    }

    /// Hermetic counterpart to `live_download_one_file_verifies_sha`: a local `httpmock`
    /// server stands in for the real `resolve/.../<path>` blob GET, exercising the same
    /// temp→rename→[`verify_downloaded`] path `download_one` uses in production.
    #[test]
    fn download_one_at_persists_and_verifies_a_mocked_blob() {
        let server = httpmock::MockServer::start();
        let content = b"fake sepformer weights";
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/blob/weights.bin");
            then.status(200).body(content);
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested").join("weights.bin");
        let f = TreeFile {
            path: "weights.bin".to_string(),
            size: content.len() as u64,
            sha256: Some(crate::hash::sha256_hex(content)),
            git_blob_sha1: None,
        };
        download_one_at(&server.url("/blob/weights.bin"), &f, &dest, &|_, _| {})
            .expect("download + verify succeeds");
        mock.assert();
        assert_eq!(std::fs::read(&dest).unwrap(), content);
    }

    /// A sha256 mismatch (corrupted/MITM'd blob) must fail rather than persist bad bytes —
    /// `download_one_at` retries transient errors but a verify failure isn't one, so this
    /// exercises the fast-fail path with no retry delay.
    #[test]
    fn download_one_at_rejects_a_sha256_mismatch() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/blob/tampered.bin");
            then.status(200).body(b"tampered bytes");
        });

        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("tampered.bin");
        let f = TreeFile {
            path: "tampered.bin".to_string(),
            size: 14,
            sha256: Some("0".repeat(64)),
            git_blob_sha1: None,
        };
        let err =
            download_one_at(&server.url("/blob/tampered.bin"), &f, &dest, &|_, _| {}).unwrap_err();
        mock.assert();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!dest.exists(), "a failed verify must not persist the file");
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
        let files = fetch_tree(&DIARIZATION_COREML).unwrap();
        let f = files
            .iter()
            .filter(|f| f.sha256.is_some())
            .min_by_key(|f| f.size)
            .expect("an LFS file");
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join(&f.path);
        download_one(&DIARIZATION_COREML, f, &dest, &|_, _| {}).expect("download+verify");
        assert!(dest.exists());
        assert_eq!(std::fs::metadata(&dest).unwrap().len(), f.size);
        // A second pass is a cheap no-op (already_have short-circuits on the verified sha).
        assert!(already_have(&dest, f));
    }

    #[test]
    fn revisions_are_full_40_char_commit_shas() {
        for r in all_coreml_repos() {
            assert_eq!(
                r.revision.len(),
                40,
                "{} revision must be a full SHA",
                r.name
            );
            assert!(r.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(r.repo.starts_with("FluidInference/"));
        }
    }
}
