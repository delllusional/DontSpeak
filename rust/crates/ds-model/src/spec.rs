//! Model specs + manifests: the `ModelSpec`/`DownloadFile`/`PrefetchItem` builders (Kokoro
//! TTS, Parakeet STT), the network-free presence probes the engine factory uses, and the
//! installer's prefetch list. Every URL/digest/size is read from the single download
//! registry in [`crate::urls`] — this module holds only the logic that shapes them.

use std::path::PathBuf;

use crate::download::url_basename;
use crate::hash::verify_sha256;
use crate::model_path;
use crate::ort::{onnxruntime_dist, onnxruntime_dylib_file, onnxruntime_dylib_path};

/// A single downloadable asset: its on-disk file name, source URL, and pinned
/// SHA-256 (lowercase hex). A human size label, when shown, is formatted from the
/// manifest's `size_bytes` at the display site — not carried here.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub file_name: String,
    pub url: String,
    /// Pinned lowercase-hex SHA-256. Empty == "do not verify" (used only by the
    /// localhost fixture test; real specs always pin a digest).
    pub sha256: String,
}

impl ModelSpec {
    /// Build a spec from a registry [`crate::urls::Download`] entry — the single source of
    /// every URL/digest (see `urls.rs`).
    fn of(d: crate::urls::Download) -> ModelSpec {
        ModelSpec {
            file_name: d.file_name.to_string(),
            url: d.url.to_string(),
            sha256: d.sha256.to_string(),
        }
    }
}

// On-disk file-name consts are part of the public API (`ds_model::KOKORO_ONNX_FILE`,
// …); re-export them from the registry so the historical paths keep resolving.
pub use crate::urls::{
    KOKORO_ONNX_FILE, KOKORO_VOICES_FILE, PARAKEET_DECODER_FILE, PARAKEET_ENCODER_FILE,
    PARAKEET_PREPROC_FILE, PARAKEET_VOCAB_FILE,
};

/// [`ModelSpec`] for `kokoro-v1.0.onnx` (~310 MB).
pub fn kokoro_onnx_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_ONNX)
}

/// [`ModelSpec`] for `voices-v1.0.bin` (~28 MB).
pub fn kokoro_voices_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_VOICES)
}

/// Is the FULL native-Kokoro asset set present AND checksum-valid (model +
/// voices + the onnxruntime dylib)? The TTS factory uses this as the cheap,
/// network-free availability probe so it can fail-quiet when assets are absent.
/// The model + voices are verified against their pinned SHA-256; the dylib is
/// version-gated (see `onnxruntime_dylib_version_ok`).
pub fn kokoro_present() -> bool {
    let onnx = kokoro_onnx_spec();
    let voices = kokoro_voices_spec();
    let model_ok = model_path(&onnx.file_name)
        .map(|p| verify_sha256(&p, &onnx.sha256))
        .unwrap_or(false);
    let voices_ok = model_path(&voices.file_name)
        .map(|p| verify_sha256(&p, &voices.sha256))
        .unwrap_or(false);
    // Dylib must be present AND the version `ort` needs — a wrong version would
    // deadlock `ort` at session build (see `onnxruntime_dylib_version_ok`), so a
    // mismatch reports "not present" here, surfacing as a red dot + re-download
    // prompt instead of a silent hang.
    let dylib_ok = crate::ort::onnxruntime_dylib_version_ok();
    model_ok && voices_ok && dylib_ok
}

// ─────────────────────────────────────────────────────────────────────────────
// Parakeet TDT 0.6b v2 (English) STT model — int8 ONNX (encoder + decoder_joint +
// nemo128 preprocessor + vocab). Run in-process via `transcribe-rs` over the SAME
// `ort` (load-dynamic) runtime as Kokoro, so the onnxruntime dylib is shared (no
// extra dylib to fetch).
// ─────────────────────────────────────────────────────────────────────────────

// The Parakeet URLs/digests/sizes live in the registry (`urls.rs`); `transcribe-rs`
// (`ParakeetModel::load`) expects all four files flat in ONE dir under exactly the names
// pinned there. The file-name consts are re-exported above with the Kokoro ones.

/// [`ModelSpec`] for the Parakeet encoder (`encoder-model.int8.onnx`, ~622 MB).
pub fn parakeet_encoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_ENCODER)
}

/// [`ModelSpec`] for the Parakeet decoder+joint (`decoder_joint-model.int8.onnx`,
/// ~9 MB).
pub fn parakeet_decoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_DECODER)
}

/// [`ModelSpec`] for the Parakeet mel preprocessor (`nemo128.onnx`, ~140 KB).
pub fn parakeet_preproc_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_PREPROC)
}

/// [`ModelSpec`] for the Parakeet vocab (`vocab.txt`, ~9 KB).
pub fn parakeet_vocab_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_VOCAB)
}

/// The directory `transcribe-rs` should load Parakeet from (the flat
/// `model_dir()` holding all four files). `None` only if the data dir won't
/// resolve.
pub fn parakeet_dir() -> Option<PathBuf> {
    ds_config::model_dir()
}

/// Is the FULL Parakeet asset set present AND checksum-valid (encoder + decoder,
/// preprocessor, vocab, and the shared onnxruntime dylib)? The STT factory uses
/// this as the cheap, network-free availability probe so it degrades to
/// ClaudeNative when the model is absent. The onnxruntime dylib is shared with
/// Kokoro (existence-only, as it's extracted from a separately sha-verified `.tgz`).
pub fn parakeet_present() -> bool {
    let specs = [
        parakeet_encoder_spec(),
        parakeet_decoder_spec(),
        parakeet_preproc_spec(),
        parakeet_vocab_spec(),
    ];
    let models_ok = specs.iter().all(|spec| {
        model_path(&spec.file_name)
            .map(|p| verify_sha256(&p, &spec.sha256))
            .unwrap_or(false)
    });
    let dylib_ok = onnxruntime_dylib_path()
        .map(|p| p.is_file())
        .unwrap_or(false);
    models_ok && dylib_ok
}

// ─────────────────────────────────────────────────────────────────────────────
// Download manifest — the URL + size of every file an asset needs, so a UI can
// show the total size up front and a real "X MB of Y MB" bar during the fetch.
// ─────────────────────────────────────────────────────────────────────────────

/// One file an asset download will fetch: where it comes from and how big it is.
/// `size_bytes` is the known/expected on-disk size (exact for the Kokoro release
/// blobs; approximate for other assets) — used to show total size BEFORE the
/// download starts. During the fetch the live `Content-Length` is what drives the
/// progress total, so an approximate value here never mis-scales the live bar.
#[derive(Debug, Clone)]
pub struct DownloadFile {
    pub file_name: String,
    pub url: String,
    pub size_bytes: u64,
}

impl DownloadFile {
    /// Build a manifest entry from a registry [`crate::urls::Download`] (file + URL + size).
    fn of(d: crate::urls::Download) -> DownloadFile {
        DownloadFile {
            file_name: d.file_name.to_string(),
            url: d.url.to_string(),
            size_bytes: d.size_bytes,
        }
    }
}

/// The onnxruntime dylib `.tgz` manifest entry on platforms that have a pinned dist.
fn onnxruntime_dylib_file_entry() -> Option<DownloadFile> {
    onnxruntime_dist().map(|dist| DownloadFile {
        file_name: onnxruntime_dylib_file().to_string(),
        url: dist.url.to_string(),
        size_bytes: crate::urls::ONNXRUNTIME_DIST_SIZE_BYTES,
    })
}

/// The files the FULL native-Kokoro download fetches, in fetch order
/// (onnx, voices, then the onnxruntime dylib `.tgz` on supported platforms). All
/// URLs/sizes come from the `urls.rs` registry.
pub fn kokoro_files() -> Vec<DownloadFile> {
    let mut v = vec![
        DownloadFile::of(crate::urls::KOKORO_ONNX),
        DownloadFile::of(crate::urls::KOKORO_VOICES),
    ];
    v.extend(onnxruntime_dylib_file_entry());
    v
}

/// The files the FULL Parakeet download fetches, in fetch order (encoder, decoder,
/// preprocessor, vocab, then the shared onnxruntime dylib `.tgz` on supported
/// platforms). Byte counts are the exact `istupakov/parakeet-tdt-0.6b-v2-onnx`
/// blob sizes.
pub fn parakeet_files() -> Vec<DownloadFile> {
    let mut v = vec![
        DownloadFile::of(crate::urls::PARAKEET_ENCODER),
        DownloadFile::of(crate::urls::PARAKEET_DECODER),
        DownloadFile::of(crate::urls::PARAKEET_PREPROC),
        DownloadFile::of(crate::urls::PARAKEET_VOCAB),
    ];
    v.extend(onnxruntime_dylib_file_entry());
    v
}

/// One asset the installer should download for a component, with its pinned digest.
/// `basename` is BOTH the name to save the download as AND the key
/// `crate::download::prefetch_local` matches — they must stay identical.
#[derive(Debug, Clone)]
pub struct PrefetchItem {
    pub url: String,
    pub basename: String,
    pub sha256: String,
}

/// The files a component still NEEDS downloaded — already-present, sha-valid assets
/// are omitted, so re-running the installer downloads nothing. `what` =
/// "onnx" | "kokoro" | "parakeet" | "cuda". This is the SINGLE source of the
/// installer's download list; the URLs/SHAs never leave ds-model.
pub fn prefetch_items(what: &str) -> Vec<PrefetchItem> {
    let item = |url: &str, sha: &str| PrefetchItem {
        url: url.to_string(),
        basename: url_basename(url).to_string(),
        sha256: sha.to_string(),
    };
    let spec_item = |spec: &ModelSpec| -> Option<PrefetchItem> {
        let present = model_path(&spec.file_name)
            .map(|p| verify_sha256(&p, &spec.sha256))
            .unwrap_or(false);
        (!present).then(|| item(&spec.url, &spec.sha256))
    };
    match what {
        // NOTE: the onnx dylib and the CUDA runtime below are gated on EXISTENCE, not a
        // pinned SHA/version (unlike the SHA-checked model specs). So if ONNXRUNTIME_VERSION
        // or CUDA_WHEELS is ever bumped, a reinstall will NOT re-fetch them while the old
        // files still exist — the user must clear model_dir() (or the app's runtime
        // onnxruntime_dylib_version_ok() check will flag onnx and offer a re-download).
        "onnx" => {
            if onnxruntime_dylib_path()
                .map(|p| p.is_file())
                .unwrap_or(false)
            {
                return vec![];
            }
            match onnxruntime_dist() {
                Some(d) => vec![item(d.url, d.archive_sha256)],
                None => vec![],
            }
        }
        "kokoro" => [kokoro_onnx_spec(), kokoro_voices_spec()]
            .iter()
            .filter_map(&spec_item)
            .collect(),
        "parakeet" => [
            parakeet_encoder_spec(),
            parakeet_decoder_spec(),
            parakeet_preproc_spec(),
            parakeet_vocab_spec(),
        ]
        .iter()
        .filter_map(&spec_item)
        .collect(),
        #[cfg(all(
            any(target_os = "windows", target_os = "linux"),
            target_arch = "x86_64"
        ))]
        "cuda" => {
            if crate::ort::cuda_runtime_present() {
                return vec![];
            }
            crate::ort::CUDA_WHEELS
                .iter()
                .map(|(u, s)| item(u, s))
                .collect()
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The installer keys each prefetched file by url_basename(url): the manifest
    // saves the download under that name and prefetch_local() looks it up by it.
    // That stays consistent automatically when a source URL changes — UNLESS two
    // URLs collide on the same basename, which would cross-wire two assets. Guard
    // that here so a future URL edit can't silently break the installer path.
    #[test]
    fn prefetch_basenames_are_unique_and_nonempty() {
        let mut urls: Vec<String> = vec![
            kokoro_onnx_spec().url,
            kokoro_voices_spec().url,
            parakeet_encoder_spec().url,
            parakeet_decoder_spec().url,
            parakeet_preproc_spec().url,
            parakeet_vocab_spec().url,
        ];
        if let Some(d) = onnxruntime_dist() {
            urls.push(d.url.to_string());
        }
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        for (u, _) in crate::ort::CUDA_WHEELS {
            urls.push(u.to_string());
        }
        let mut names: Vec<&str> = urls.iter().map(|u| url_basename(u)).collect();
        assert!(
            names.iter().all(|n| !n.is_empty()),
            "a source URL has no basename"
        );
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            total,
            names.len(),
            "two source URLs share a basename — the installer's prefetch keying \
             (url_basename) would cross-wire them; rename one or key by URL hash"
        );
    }

    #[test]
    fn kokoro_specs_have_right_urls_and_files() {
        let onnx = kokoro_onnx_spec();
        assert_eq!(onnx.file_name, "kokoro-v1.0.onnx");
        assert_eq!(
            onnx.url,
            "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx"
        );
        let voices = kokoro_voices_spec();
        assert_eq!(voices.file_name, "voices-v1.0.bin");
        assert_eq!(
            voices.url,
            "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin"
        );
    }

    #[test]
    fn kokoro_specs_pin_real_digests() {
        // The Kokoro pins are now the real release digests (64-hex lowercase),
        // not empty — so `ensure`/`kokoro_present` actually verify bytes.
        let onnx = kokoro_onnx_spec();
        let voices = kokoro_voices_spec();
        for spec in [&onnx, &voices] {
            assert_eq!(spec.sha256.len(), 64, "sha256 must be 64 hex chars");
            assert!(
                spec.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "sha256 must be lowercase hex: {}",
                spec.sha256
            );
        }
        // The two assets have distinct digests.
        assert_ne!(onnx.sha256, voices.sha256);
    }

    #[test]
    fn kokoro_present_returns_a_bool_without_panicking() {
        // Network-free presence probe: in the test env the assets almost
        // certainly aren't in model_dir, so this degrades to false — but we only
        // assert it does not panic and returns a bool (it must never wrong-accept
        // now that the pins are real and the files are absent/unverified).
        let _present: bool = kokoro_present();
    }

    #[test]
    fn parakeet_specs_have_right_urls_files_and_pins() {
        let enc = parakeet_encoder_spec();
        assert_eq!(enc.file_name, "encoder-model.int8.onnx");
        assert_eq!(
            enc.url,
            "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/encoder-model.int8.onnx"
        );
        let dec = parakeet_decoder_spec();
        assert_eq!(dec.file_name, "decoder_joint-model.int8.onnx");
        let pre = parakeet_preproc_spec();
        assert_eq!(pre.file_name, "nemo128.onnx");
        let voc = parakeet_vocab_spec();
        assert_eq!(voc.file_name, "vocab.txt");
        assert_eq!(
            voc.url,
            "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/vocab.txt"
        );
        // All four pin distinct, lowercase, 64-hex digests.
        for spec in [&enc, &dec, &pre, &voc] {
            assert_eq!(spec.sha256.len(), 64, "sha256 must be 64 hex chars");
            assert!(
                spec.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "sha256 must be lowercase hex: {}",
                spec.sha256
            );
        }
        assert_ne!(enc.sha256, dec.sha256);
        assert_ne!(dec.sha256, pre.sha256);
        assert_ne!(pre.sha256, voc.sha256);
    }

    #[test]
    fn parakeet_present_returns_a_bool_without_panicking() {
        // Network-free presence probe: assets almost certainly absent in the test
        // env, so this degrades to false — assert only that it never panics.
        let _present: bool = parakeet_present();
    }
}
