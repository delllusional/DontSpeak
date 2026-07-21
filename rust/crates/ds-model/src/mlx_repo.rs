//! Self-managed MLX model downloads.
//!
//! We fetch every MLX asset with the same HTTP/retry/SHA/atomic-rename/progress path as ONNX.
//! The native shim loads only these local directories. Each set is pinned to an immutable
//! HF commit; tree API + LFS `oid` (content sha256) or
//! plain-blob `oid` (`git hash-object`) + size verify bytes. `.ds-ready` holds the revision
//! and downloaded path manifest (status poll is network-free; partials never look ready).

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::download::{DEFAULT_RETRIES, DownloadState, download_to_with_state, is_permanent_error};
use crate::hash::verify_sha256;

const HF_HOST: &str = "https://huggingface.co";
/// Written into a model dir once every file is present + verified; holds the pinned revision
/// so bumping the pin invalidates a stale tree and forces a re-fetch.
const READY_MARKER: &str = ".ds-ready";

/// One MLX model set, pinned to an immutable HF revision. `include_prefixes`
/// keeps only tree paths beginning with one of them (empty = whole repo); `exclude_substrings`
/// drops junk/duplicate formats. Each kept tree path is written under `target()` preserving
/// its sub-path.
pub struct MlxRepo {
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

/// On-disk folder name for the MLX Kokoro model and per-voice safetensors.
pub const KOKORO_MLX_DIR_NAME: &str = "kokoro-82m";
pub const CHATTERBOX_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_chatterbox-8bit";
pub const CHATTERBOX_S3_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_S3TokenizerV2";
pub const QWEN_MLX_DIR_NAME: &str = "qwen3-tts-0.6b-customvoice";
pub const OMNIVOICE_MLX_DIR_NAME: &str = "mlx-audio/mlx-community_OmniVoice-bf16";

fn kokoro_main_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(KOKORO_MLX_DIR_NAME))
}

fn chatterbox_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(CHATTERBOX_MLX_DIR_NAME))
}

fn chatterbox_s3_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(CHATTERBOX_S3_MLX_DIR_NAME))
}

fn qwen_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(QWEN_MLX_DIR_NAME))
}

fn omnivoice_tts_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(OMNIVOICE_MLX_DIR_NAME))
}

/// Exact DontSpeak-managed directory passed to the selected MLX TTS loader.
pub fn tts_mlx_dir(model: ds_config::TtsModel) -> Option<PathBuf> {
    match model {
        ds_config::TtsModel::Kokoro => kokoro_main_target(),
        ds_config::TtsModel::Chatterbox => chatterbox_tts_target(),
        ds_config::TtsModel::Qwen => qwen_tts_target(),
        ds_config::TtsModel::OmniVoice => omnivoice_tts_target(),
    }
}

/// Exact local directory passed to `ParakeetModel.fromDirectory`.
fn parakeet_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join("parakeet-tdt-0.6b-v2"))
}

pub fn parakeet_mlx_dir() -> Option<PathBuf> {
    parakeet_target()
}

pub const DIARIZATION_MLX_DIR_NAME: &str = "sortformer";
pub const SPEAKER_EMBEDDING_DIR_NAME: &str = "wespeaker";

fn diarization_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(DIARIZATION_MLX_DIR_NAME))
}

fn speaker_embedding_target() -> Option<PathBuf> {
    Some(ds_config::mlx_dir()?.join(SPEAKER_EMBEDDING_DIR_NAME))
}

/// Exact local directory passed to `SortformerModel.fromModelDirectory`.
pub fn diarization_dir() -> Option<PathBuf> {
    diarization_target()
}

/// Exact local directory for the MLX WeSpeaker embedding checkpoint.
pub fn speaker_embedding_dir() -> Option<PathBuf> {
    speaker_embedding_target()
}

/// MLX Kokoro TTS weights and voice embeddings. Apache-2.0.
pub static KOKORO_MLX: MlxRepo = MlxRepo {
    name: "kokoro_mlx",
    repo: "mlx-community/Kokoro-82M-bf16",
    revision: "a71e4d38b236d968966a2002c4c895dbd12b1c3c",
    include_prefixes: &["config.json", "kokoro-v1_0.safetensors", "voices/"],
    exclude_substrings: &[".pt", ".DS_Store"],
    target: kokoro_main_target,
    display_name: "Kokoro (MLX)",
    usage: "Apple-Silicon text-to-speech model and voice embeddings for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Chatterbox Multilingual 8-bit weights and default voice conditioning. Apache-2.0.
pub static CHATTERBOX_MLX: MlxRepo = MlxRepo {
    name: "chatterbox_mlx",
    repo: "mlx-community/chatterbox-8bit",
    revision: "9617d61b596a03d1bed766a28c341680e993a1b9",
    include_prefixes: &[
        "Cangjie5_TC.json",
        "conds.safetensors",
        "config.json",
        "model.safetensors",
        "model.safetensors.index.json",
        "tokenizer.json",
    ],
    exclude_substrings: &[".DS_Store"],
    target: chatterbox_tts_target,
    display_name: "Chatterbox Multilingual (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model and default voice conditioning",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// Chatterbox's local S3 speech tokenizer. Apache-2.0; folded into its model set.
pub static CHATTERBOX_S3_MLX: MlxRepo = MlxRepo {
    name: "chatterbox_s3_mlx",
    repo: "mlx-community/S3TokenizerV2",
    revision: "e0c9886f0e1c35ae85b1f27277416fb19fc72bec",
    include_prefixes: &["config.json", "model.safetensors"],
    exclude_substrings: &[".DS_Store"],
    target: chatterbox_s3_target,
    display_name: "S3TokenizerV2 (MLX)",
    usage: "Apple-Silicon speech tokenizer required by Chatterbox MLX",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX Qwen3-TTS 0.6B CustomVoice weights. Apache-2.0.
pub static QWEN_MLX: MlxRepo = MlxRepo {
    name: "qwen_mlx",
    repo: "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit",
    revision: "049ef77fe8816b536193c0c25f9a214d17921282",
    include_prefixes: &[
        "config.json",
        "generation_config.json",
        "merges.txt",
        "model.safetensors",
        "speech_tokenizer/",
        "tokenizer_config.json",
        "vocab.json",
    ],
    exclude_substrings: &[".DS_Store", "README"],
    target: qwen_tts_target,
    display_name: "Qwen3-TTS (MLX)",
    usage: "Apple-Silicon multilingual text-to-speech model for MLX Audio",
    license: "Apache-2.0",
    license_url: "https://www.apache.org/licenses/LICENSE-2.0",
};

/// MLX OmniVoice bfloat16 model and Higgs audio tokenizer.
pub static OMNIVOICE_MLX: MlxRepo = MlxRepo {
    name: "omnivoice_mlx",
    repo: "mlx-community/OmniVoice-bf16",
    revision: "8fb0b754cad788aaefec690cd55c207e8a628f85",
    include_prefixes: &[
        "audio_tokenizer/config.json",
        "audio_tokenizer/model.safetensors",
        "audio_tokenizer/preprocessor_config.json",
        "chat_template.jinja",
        "config.json",
        "model.safetensors",
        "tokenizer.json",
        "tokenizer_config.json",
    ],
    exclude_substrings: &[".DS_Store"],
    target: omnivoice_tts_target,
    display_name: "OmniVoice (MLX)",
    usage: "Apple-Silicon omnilingual text-to-speech model and Higgs Audio 2 tokenizer",
    license: "CC-BY-NC / Boson Community License",
    license_url: "https://huggingface.co/k2-fsa/OmniVoice#license",
};

/// MLX Parakeet TDT 0.6b v2 STT. CC-BY-4.0.
pub static PARAKEET_MLX: MlxRepo = MlxRepo {
    name: "parakeet_mlx",
    repo: "mlx-community/parakeet-tdt-0.6b-v2",
    revision: "8ae155301e23d820d82aa60d24817c900e69e487",
    include_prefixes: &[
        "config.json",
        "model.safetensors",
        "tokenizer.model",
        "tokenizer.vocab",
        "vocab.txt",
    ],
    exclude_substrings: &[".DS_Store"],
    target: parakeet_target,
    display_name: "Parakeet (MLX)",
    usage: "Apple-Silicon speech-to-text model (NVIDIA NeMo; MLX conversion)",
    license: "CC-BY-4.0",
    license_url: "https://creativecommons.org/licenses/by/4.0/",
};

/// MLX Sortformer speaker diarization. The converted repository does not publish
/// SPDX metadata, so the original NVIDIA model terms are linked explicitly in NOTICE.
pub static DIARIZATION_MLX: MlxRepo = MlxRepo {
    name: "diarization_mlx",
    repo: "mlx-community/diar_streaming_sortformer_4spk-v2.1-fp16",
    revision: "e23e6404bd9859e93edbf94a740eb1c7fc58f12e",
    include_prefixes: &["config.json", "model.safetensors"],
    exclude_substrings: &[".DS_Store"],
    target: diarization_target,
    display_name: "Sortformer diarization (MLX)",
    usage: "Apple-Silicon speaker diarization (NVIDIA Sortformer; MLX conversion)",
    license: "NVIDIA Open Model License",
    license_url: "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/",
};

/// MLX WeSpeaker ResNet34 embedding model used for enrollment and speaker identity matching.
pub static SPEAKER_EMBEDDING_MLX: MlxRepo = MlxRepo {
    name: "speaker_embedding_mlx",
    repo: "mlx-community/wespeaker-voxceleb-resnet34-LM",
    revision: "038a61d379b8729c72d64d7c209e0cee80b11d0f",
    include_prefixes: &["config.json", "weights.npz"],
    exclude_substrings: &[],
    target: speaker_embedding_target,
    display_name: "WeSpeaker embedding (MLX)",
    usage: "Apple-Silicon speaker enrollment and identity matching",
    license: "MIT",
    license_url: "https://opensource.org/license/mit",
};

/// The repos one `DownloadTarget::KokoroMlx` fetch produces. ONE source of truth shared by the engine's
/// download manager (fetch + presence gate) and the status row, so they can never disagree
/// about what "the Kokoro MLX set" is.
pub static KOKORO_MLX_SET: [&MlxRepo; 1] = [&KOKORO_MLX];

/// The repos one `DownloadTarget::ChatterboxMlx` fetch produces.
pub static CHATTERBOX_MLX_SET: [&MlxRepo; 2] = [&CHATTERBOX_MLX, &CHATTERBOX_S3_MLX];

/// The repos one `DownloadTarget::QwenMlx` fetch produces.
pub static QWEN_MLX_SET: [&MlxRepo; 1] = [&QWEN_MLX];

/// The repos one `DownloadTarget::OmniVoiceMlx` fetch produces.
pub static OMNIVOICE_MLX_SET: [&MlxRepo; 1] = [&OMNIVOICE_MLX];

/// Complete pinned asset set for one built-in MLX TTS model.
pub fn tts_mlx_set(model: ds_config::TtsModel) -> &'static [&'static MlxRepo] {
    match model {
        ds_config::TtsModel::Kokoro => &KOKORO_MLX_SET,
        ds_config::TtsModel::Chatterbox => &CHATTERBOX_MLX_SET,
        ds_config::TtsModel::Qwen => &QWEN_MLX_SET,
        ds_config::TtsModel::OmniVoice => &OMNIVOICE_MLX_SET,
    }
}

/// The repos one `DownloadTarget::ParakeetMlx` fetch produces.
pub static PARAKEET_MLX_SET: [&MlxRepo; 1] = [&PARAKEET_MLX];

/// Sortformer segmentation plus WeSpeaker embeddings are one user-visible diarization download.
pub static DIARIZATION_MLX_SET: [&MlxRepo; 2] = [&DIARIZATION_MLX, &SPEAKER_EMBEDDING_MLX];

/// LOCAL presence of a whole set (no network): every repo's completion marker present at the
/// pinned revision — see [`is_mlx_repo_present`].
pub fn is_mlx_set_present(set: &[&MlxRepo]) -> bool {
    set.iter().all(|r| is_mlx_repo_present(r))
}

/// Every MLX repo we self-manage, in the order a clean install fetches them.
pub fn all_mlx_repos() -> [&'static MlxRepo; 8] {
    [
        &KOKORO_MLX,
        &CHATTERBOX_MLX,
        &CHATTERBOX_S3_MLX,
        &QWEN_MLX,
        &OMNIVOICE_MLX,
        &PARAKEET_MLX,
        &DIARIZATION_MLX,
        &SPEAKER_EMBEDDING_MLX,
    ]
}

/// One tree entry at the pinned revision. Content verify: LFS → `sha256`; else `git_blob_sha1` + size.
#[derive(Debug)]
struct TreeFile {
    path: String,
    size: u64,
    sha256: Option<String>,
    /// Non-LFS tree `oid` only — LFS top-level oid is the pointer file, not resolved content.
    git_blob_sha1: Option<String>,
}

/// Lowercase hex SHA-1 (git blob hash; no `sha1` crate — distinct from [`crate::hash`]).
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

/// `git hash-object` oid: `sha1("blob {len}\0" + content)`. Same-size MITM fails oid check.
fn git_blob_sha1_hex(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut data = format!("blob {}\0", bytes.len()).into_bytes();
    data.extend_from_slice(&bytes);
    Some(sha1_hex(&data))
}

/// Whether a kept tree path passes a repo's include/exclude filters.
fn keep(repo: &MlxRepo, path: &str) -> bool {
    let included = repo.include_prefixes.is_empty()
        || repo.include_prefixes.iter().any(|p| path.starts_with(p));
    let excluded = repo.exclude_substrings.iter().any(|s| path.contains(s));
    included && !excluded
}

/// GET + parse the HF tree API. `url` is full tree URL so tests can point at httpmock
/// instead of `huggingface.co` — same `http_get_builder`, JSON shape, and filters.
fn fetch_tree_at(url: &str, repo: &MlxRepo) -> std::io::Result<Vec<TreeFile>> {
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
        // LFS: content sha under `lfs`. Plain blob: top-level oid = git hash-object (not LFS pointer).
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
        let git_blob_sha1 = if lfs.is_none() {
            e.get("oid").and_then(|o| o.as_str()).map(|s| s.to_string())
        } else {
            None
        };
        // Every kept file needs SOME verifier (see `verify_downloaded`); an entry with
        // none would download unverifiable bytes. Permanent per the crate's retry policy.
        if sha256.is_none() && git_blob_sha1.is_none() && size == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("tree entry {path} has no verifier (no LFS sha256, git oid, or size)"),
            ));
        }
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

/// Skip re-fetch when [`verify_downloaded`] passes (stale same-size plain blobs fail).
fn already_have(dest: &Path, f: &TreeFile) -> bool {
    verify_downloaded(dest, f).is_ok()
}

/// Single integrity check for download + presence: LFS sha256, else size + optional git blob oid.
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

/// GET-and-verify one tree file (temp→rename, retry, [`verify_downloaded`]). Resolve URL is a
/// parameter so tests can point at httpmock instead of `huggingface.co`.
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
    let tmp = tempfile::Builder::new().tempfile_in(dir)?;
    let mut state = DownloadState::default();
    let mut attempt = 0;
    loop {
        let res = download_to_with_state(url, tmp.path(), progress, &mut state)
            .and_then(|()| verify_downloaded(tmp.path(), f));
        match res {
            Ok(()) => {
                tmp.persist(dest).map_err(|e| e.error)?;
                return Ok(());
            }
            Err(e) if is_permanent_error(&e) || attempt + 1 >= DEFAULT_RETRIES.max(1) => {
                return Err(e);
            }
            Err(_) => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(500 * attempt as u64));
            }
        }
    }
}

/// Download a single MLX repo, reporting one overall 0..1 byte-progress bar — a thin
/// single-repo wrapper over the universal [`ensure_mlx_repos`] (identical byte-weighted
/// aggregate), for callers that fetch exactly one repo (the engine-side download manager / the
/// diarization fetch).
pub fn ensure_mlx_repo(repo: &MlxRepo, progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    ensure_mlx_repos(&[repo], progress)
}

/// Download a SET of repos as one unit, reporting ONE overall byte-weighted bar:
/// `progress(done_bytes, total_bytes)` where BOTH are summed across every file of every repo —
/// so the UI shows a single monotonic "Downloading `<pct>`%" over the WHOLE set (a true global
/// percent, not a per-file percent that resets each file). Writes each repo's completion marker
/// once its files are all present. Missing files fetch concurrently (bounded pool).
pub fn ensure_mlx_repos(repos: &[&MlxRepo], progress: &dyn Fn(u64, u64)) -> std::io::Result<()> {
    ensure_mlx_repos_at(HF_HOST, repos, progress)
}

/// Same as [`ensure_mlx_repos`] but tree + resolve URLs are rooted at `host` (production:
/// [`HF_HOST`]; tests: httpmock base URL).
pub(crate) fn ensure_mlx_repos_at(
    host: &str,
    repos: &[&MlxRepo],
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<()> {
    // Resolve every not-yet-present repo's tree first so the total byte count is exact up front.
    let mut plan: Vec<(&MlxRepo, PathBuf, Vec<TreeFile>)> = Vec::new();
    for r in repos {
        if is_mlx_repo_present(r) {
            continue;
        }
        let target = (r.target)().ok_or_else(|| {
            std::io::Error::other(format!("cannot resolve target dir for {}", r.name))
        })?;
        let tree_url = format!(
            "{host}/api/models/{}/tree/{}?recursive=true",
            r.repo, r.revision
        );
        let files = fetch_tree_at(&tree_url, r)?;
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

    let mut pre_credit: u64 = 0;
    let mut jobs: Vec<crate::parallel::DownloadJob> = Vec::new();
    for (r, target, files) in &plan {
        for f in files {
            let relative = Path::new(&f.path);
            if relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsafe path in Apple model tree response: {}", f.path),
                ));
            }
            let dest = target.join(relative);
            let size = f.size.max(1);
            if already_have(&dest, f) {
                pre_credit = pre_credit.saturating_add(size);
                continue;
            }
            let host = host.to_string();
            let repo_id = r.repo;
            let revision = r.revision;
            let path = f.path.clone();
            let sha256 = f.sha256.clone();
            let git_blob_sha1 = f.git_blob_sha1.clone();
            let file_size = f.size;
            jobs.push(Box::new(move |p| {
                let tree = TreeFile {
                    path: path.clone(),
                    size: file_size,
                    sha256,
                    git_blob_sha1,
                };
                // Local progress is file bytes; pool sums high-waters + pre_credit.
                let emit = |done: u64, _t: u64| p(done.min(size), size);
                let url = format!("{host}/{repo_id}/resolve/{revision}/{path}");
                download_one_at(&url, &tree, &dest, &emit)
            }));
        }
    }

    crate::parallel::run_jobs_parallel(progress, total_bytes, pre_credit.min(total_bytes), jobs)?;

    // Markers only after the full set of missing files succeeded (no marker on partial fail).
    for (r, target, files) in &plan {
        for f in files {
            let dest = target.join(Path::new(&f.path));
            if !already_have(&dest, f) {
                return Err(std::io::Error::other(format!(
                    "missing verified file after download: {}",
                    f.path
                )));
            }
        }
        let mut marker = String::with_capacity(
            r.revision.len() + files.iter().map(|f| f.path.len() + 1).sum::<usize>() + 1,
        );
        marker.push_str(r.revision);
        marker.push('\n');
        for file in files {
            marker.push_str(&file.path);
            marker.push('\n');
        }
        std::fs::write(target.join(READY_MARKER), marker)?;
    }
    Ok(())
}

/// LOCAL presence (no network): marker revision matches and every manifested file still exists.
pub fn is_mlx_repo_present(repo: &MlxRepo) -> bool {
    let Some(target) = (repo.target)() else {
        return false;
    };
    let marker = target.join(READY_MARKER);
    let mut s = String::new();
    if std::fs::File::open(&marker)
        .and_then(|mut f| f.read_to_string(&mut s))
        .is_err()
    {
        return false;
    }
    let mut lines = s.lines();
    lines.next() == Some(repo.revision)
        && lines
            .try_fold(false, |_, path| {
                let relative = Path::new(path);
                let safe = relative
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_)));
                safe.then(|| target.join(relative).is_file())
                    .filter(|present| *present)
            })
            .unwrap_or(false)
}

// `target` is `fn() -> Option<PathBuf>` (no captures). Thread-local seam for presence tests.
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
    fn model_sets_have_the_expected_components() {
        assert_eq!(KOKORO_MLX_SET.len(), 1);
        assert_eq!(CHATTERBOX_MLX_SET.len(), 2);
        assert_eq!(OMNIVOICE_MLX_SET.len(), 1);
        assert_eq!(PARAKEET_MLX_SET.len(), 1);
        assert_eq!(DIARIZATION_MLX_SET.len(), 2);
        assert!(DIARIZATION_MLX.repo.contains("sortformer"));
        assert!(SPEAKER_EMBEDDING_MLX.repo.contains("wespeaker"));
    }

    #[test]
    fn native_shim_loads_only_rust_managed_local_directories() {
        let shim =
            include_str!("../../../../apps/macos/DontSpeakMLX/Sources/DontSpeakMLX/shim.swift");
        for forbidden in ["ModelHub", "snapshotDownload", "downloadModel"] {
            assert!(
                !shim.contains(forbidden),
                "native model download API: {forbidden}"
            );
        }
        assert_eq!(
            shim.matches("OmniVoiceModel.fromPretrained").count(),
            1,
            "OmniVoice's repository-only API must remain the sole native repository load"
        );
        assert!(shim.contains("configureManagedHubCache"));
        for required in ["fromModelDirectory", "fromDirectory"] {
            assert!(
                shim.contains(required),
                "missing local model load: {required}"
            );
        }
    }

    #[test]
    fn filters_keep_only_the_runtime_set() {
        assert!(keep(&KOKORO_MLX, "config.json"));
        assert!(keep(&KOKORO_MLX, "kokoro-v1_0.safetensors"));
        assert!(keep(&KOKORO_MLX, "voices/af_heart.safetensors"));
        assert!(!keep(&KOKORO_MLX, "voices/af_heart.pt"));
        assert!(!keep(&KOKORO_MLX, "README.md"));
        assert!(keep(&CHATTERBOX_MLX, "conds.safetensors"));
        assert!(keep(&CHATTERBOX_S3_MLX, "model.safetensors"));
        assert!(!keep(
            &CHATTERBOX_MLX,
            "models--ResembleAI--chatterbox/model.safetensors"
        ));
        assert!(keep(&OMNIVOICE_MLX, "audio_tokenizer/model.safetensors"));
        assert!(keep(&PARAKEET_MLX, "model.safetensors"));
        assert!(keep(&PARAKEET_MLX, "tokenizer.model"));
        assert!(!keep(&PARAKEET_MLX, "README.md"));
        assert!(keep(&DIARIZATION_MLX, "model.safetensors"));
        assert!(keep(&SPEAKER_EMBEDDING_MLX, "weights.npz"));
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

    /// Same-size wrong content must fail when a git blob oid is pinned.
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
        let repo = MlxRepo {
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
        assert!(!is_mlx_repo_present(&repo), "no marker yet");

        // Marker present but revision mismatches → still absent.
        std::fs::write(tmp.path().join(READY_MARKER), "some-other-revision").unwrap();
        assert!(
            !is_mlx_repo_present(&repo),
            "mismatched revision reads as absent"
        );

        // Revision alone is a pre-manifest marker and therefore absent.
        std::fs::write(tmp.path().join(READY_MARKER), repo.revision).unwrap();
        assert!(
            !is_mlx_repo_present(&repo),
            "old marker has no file manifest"
        );

        let model = tmp.path().join("model.safetensors");
        std::fs::write(&model, b"weights").unwrap();
        std::fs::write(
            tmp.path().join(READY_MARKER),
            format!("{}\nmodel.safetensors\n", repo.revision),
        )
        .unwrap();
        assert!(
            is_mlx_repo_present(&repo),
            "matching marker reads as present"
        );
        std::fs::remove_file(model).unwrap();
        assert!(!is_mlx_repo_present(&repo), "missing manifested file");
    }

    /// A local `httpmock` server stands in for `huggingface.co`, so the JSON-shape handling —
    /// LFS `oid` extraction, plain git-blob `oid` extraction, `directory`-type skipping, and
    /// the include/exclude filters — gets deterministic coverage on every `cargo test`.
    #[test]
    fn fetch_tree_at_parses_lfs_and_plain_blobs_and_applies_filters() {
        let server = httpmock::MockServer::start();
        let lfs_sha = "a".repeat(64);
        let body = serde_json::json!([
            // LFS-tracked runtime weight: kept; the
            // real content sha256 comes from `lfs.oid`, NOT the top-level (pointer-file) oid.
            {
                "type": "file",
                "path": "kokoro-v1_0.safetensors",
                "size": 12345,
                "oid": "pointerfileblobsha1notusedforlfs01",
                "lfs": { "oid": format!("sha256:{lfs_sha}"), "size": 12345 }
            },
            // Plain (non-LFS) blob: kept, verified via the top-level git-blob `oid` instead.
            {
                "type": "file",
                "path": "config.json",
                "size": 20,
                "oid": "deadbeefcafef00d"
            },
            // Excluded duplicate voice format — must be dropped.
            {
                "type": "file",
                "path": "voices/af_heart.pt",
                "size": 5,
                "oid": "ignored"
            },
            // Not included by prefix — dropped.
            {
                "type": "file",
                "path": "README.md",
                "size": 5,
                "oid": "ignored"
            },
            // A `directory` entry — never a file, must be skipped regardless of path/filters.
            {
                "type": "directory",
                "path": "voices",
                "size": 0,
                "oid": "ignored"
            }
        ]);
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/api/models/mlx-community/Kokoro-82M-bf16/tree/a71e4d38b236d968966a2002c4c895dbd12b1c3c")
                .query_param("recursive", "true");
            then.status(200).json_body(body);
        });

        let url = server.url(
            "/api/models/mlx-community/Kokoro-82M-bf16/tree/a71e4d38b236d968966a2002c4c895dbd12b1c3c?recursive=true",
        );
        let files = fetch_tree_at(&url, &KOKORO_MLX).expect("tree fetch parses");
        mock.assert();

        assert_eq!(
            files.len(),
            2,
            "only the selected non-excluded files survive: {files:?}",
        );
        let weights = files
            .iter()
            .find(|f| f.path == "kokoro-v1_0.safetensors")
            .expect("LFS weights file kept");
        assert_eq!(weights.size, 12345);
        assert_eq!(weights.sha256.as_deref(), Some(lfs_sha.as_str()));
        assert!(
            weights.git_blob_sha1.is_none(),
            "LFS entries must not capture the pointer-file's top-level oid"
        );
        let heart = files
            .iter()
            .find(|f| f.path == "config.json")
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
                { "type": "file", "path": "README.md", "size": 5, "oid": "x" }
            ]));
        });
        let err = fetch_tree_at(&server.url("/empty-tree"), &KOKORO_MLX).unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("matched no files"));
    }

    /// A kept tree entry that carries NO verifier at all (no `lfs.oid`, no top-level `oid`,
    /// no size) must reject the whole tree as permanent `InvalidData` — otherwise its bytes
    /// would land with zero content verification.
    #[test]
    fn fetch_tree_at_rejects_an_entry_with_no_verifier() {
        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/no-verifier");
            then.status(200).json_body(serde_json::json!([
                { "type": "file", "path": "config.json" }
            ]));
        });
        let err = fetch_tree_at(&server.url("/no-verifier"), &KOKORO_MLX).unwrap_err();
        mock.assert();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(is_permanent_error(&err), "no-verifier entries fail fast");
        assert!(err.to_string().contains("no verifier"), "got: {err}");
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
        let err = fetch_tree_at(&server.url("/missing-repo"), &KOKORO_MLX).unwrap_err();
        mock.assert();
        assert!(err.to_string().contains("HF tree fetch failed"));
    }

    /// A local `httpmock` server stands in for the real `resolve/.../<path>` blob GET,
    /// exercising the same temp→rename→[`verify_downloaded`] path `download_one` uses in
    /// production.
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

    /// Parallel multi-file orchestrator: tree + blobs via httpmock; marker only after all files.
    #[test]
    fn ensure_mlx_repos_at_downloads_files_and_writes_marker() {
        let server = httpmock::MockServer::start();
        let a = b"alpha-bytes";
        let b = b"beta-bytes!!";
        let a_sha = crate::hash::sha256_hex(a);
        let b_sha = crate::hash::sha256_hex(b);
        let rev = "abc123rev00000000000000000000000000001";
        let repo_id = "test-org/test-model";

        let tree = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/models/{repo_id}/tree/{rev}"))
                .query_param("recursive", "true");
            then.status(200).json_body(serde_json::json!([
                {
                    "type": "file",
                    "path": "a.bin",
                    "size": a.len(),
                    "lfs": { "oid": format!("sha256:{a_sha}"), "size": a.len() }
                },
                {
                    "type": "file",
                    "path": "b.bin",
                    "size": b.len(),
                    "lfs": { "oid": format!("sha256:{b_sha}"), "size": b.len() }
                }
            ]));
        });
        let blob_a = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/a.bin"));
            then.status(200).body(a);
        });
        let blob_b = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/b.bin"));
            then.status(200).body(b);
        });

        let tmp = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = Some(tmp.path().to_path_buf()));
        let repo = MlxRepo {
            name: "orch_ok",
            repo: repo_id,
            revision: rev,
            include_prefixes: &[],
            exclude_substrings: &[],
            target: test_target,
            display_name: "",
            usage: "",
            license: "",
            license_url: "",
        };

        let seen = std::sync::Mutex::new(Vec::<u64>::new());
        ensure_mlx_repos_at(server.base_url().as_str(), &[&repo], &|d, t| {
            seen.lock().unwrap().push(d);
            assert_eq!(t, (a.len() + b.len()) as u64);
        })
        .expect("orchestrator succeeds");
        tree.assert();
        blob_a.assert();
        blob_b.assert();
        assert_eq!(std::fs::read(tmp.path().join("a.bin")).unwrap(), a);
        assert_eq!(std::fs::read(tmp.path().join("b.bin")).unwrap(), b);
        let marker = std::fs::read_to_string(tmp.path().join(READY_MARKER)).unwrap();
        assert!(marker.starts_with(rev));
        assert!(marker.contains("a.bin"));
        assert!(marker.contains("b.bin"));
        let seen = seen.lock().unwrap();
        assert!(seen.windows(2).all(|w| w[1] >= w[0]), "monotonic: {seen:?}");
        assert_eq!(*seen.last().unwrap(), (a.len() + b.len()) as u64);
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = None);
    }

    /// Already-present files pre-credit the bar; only missing blobs are fetched.
    #[test]
    fn ensure_mlx_repos_at_precredits_present_files() {
        let server = httpmock::MockServer::start();
        let a = b"present-aaa";
        let b = b"missing-bbb";
        let a_sha = crate::hash::sha256_hex(a);
        let b_sha = crate::hash::sha256_hex(b);
        let rev = "abc123rev00000000000000000000000000002";
        let repo_id = "test-org/precredit";

        let tree = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/models/{repo_id}/tree/{rev}"))
                .query_param("recursive", "true");
            then.status(200).json_body(serde_json::json!([
                {
                    "type": "file",
                    "path": "a.bin",
                    "size": a.len(),
                    "lfs": { "oid": format!("sha256:{a_sha}"), "size": a.len() }
                },
                {
                    "type": "file",
                    "path": "b.bin",
                    "size": b.len(),
                    "lfs": { "oid": format!("sha256:{b_sha}"), "size": b.len() }
                }
            ]));
        });
        let blob_a = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/a.bin"));
            then.status(200).body(a);
        });
        let blob_b = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/b.bin"));
            then.status(200).body(b);
        });

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.bin"), a).unwrap();
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = Some(tmp.path().to_path_buf()));
        let repo = MlxRepo {
            name: "orch_pre",
            repo: repo_id,
            revision: rev,
            include_prefixes: &[],
            exclude_substrings: &[],
            target: test_target,
            display_name: "",
            usage: "",
            license: "",
            license_url: "",
        };

        ensure_mlx_repos_at(server.base_url().as_str(), &[&repo], &|_, _| {})
            .expect("precredit path succeeds");
        tree.assert();
        assert_eq!(blob_a.calls(), 0, "present file must not re-fetch");
        blob_b.assert();
        assert!(tmp.path().join(READY_MARKER).is_file());
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = None);
    }

    /// Permanent blob failure must not write `.ds-ready`.
    #[test]
    fn ensure_mlx_repos_at_fails_without_marker() {
        let server = httpmock::MockServer::start();
        let a = b"ok-file";
        let a_sha = crate::hash::sha256_hex(a);
        let rev = "abc123rev00000000000000000000000000003";
        let repo_id = "test-org/fail-set";

        let tree = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/api/models/{repo_id}/tree/{rev}"))
                .query_param("recursive", "true");
            then.status(200).json_body(serde_json::json!([
                {
                    "type": "file",
                    "path": "a.bin",
                    "size": a.len(),
                    "lfs": { "oid": format!("sha256:{a_sha}"), "size": a.len() }
                },
                {
                    "type": "file",
                    "path": "bad.bin",
                    "size": 10,
                    "lfs": {
                        "oid": format!("sha256:{}", "0".repeat(64)),
                        "size": 10
                    }
                }
            ]));
        });
        let _blob_a = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/a.bin"));
            then.status(200).body(a);
        });
        let _blob_bad = server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path(format!("/{repo_id}/resolve/{rev}/bad.bin"));
            then.status(200).body(b"wrong-size");
        });

        let tmp = tempfile::tempdir().unwrap();
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = Some(tmp.path().to_path_buf()));
        let repo = MlxRepo {
            name: "orch_fail",
            repo: repo_id,
            revision: rev,
            include_prefixes: &[],
            exclude_substrings: &[],
            target: test_target,
            display_name: "",
            usage: "",
            license: "",
            license_url: "",
        };

        let err = ensure_mlx_repos_at(server.base_url().as_str(), &[&repo], &|_, _| {}).unwrap_err();
        tree.assert();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            !tmp.path().join(READY_MARKER).exists(),
            "partial fail must not write .ds-ready"
        );
        TEST_TARGET_DIR.with(|t| *t.borrow_mut() = None);
    }

    #[test]
    fn revisions_are_full_40_char_commit_shas() {
        for r in all_mlx_repos() {
            assert_eq!(
                r.revision.len(),
                40,
                "{} revision must be a full SHA",
                r.name
            );
            assert!(r.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(r.repo.starts_with("mlx-community/"));
        }
    }
}
