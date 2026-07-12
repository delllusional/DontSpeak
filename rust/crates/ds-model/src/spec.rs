//! Model specs + manifests: the `ModelSpec`/`DownloadFile`/`PrefetchItem` builders (Kokoro
//! TTS, Parakeet STT), the network-free presence probes the engine factory uses, and the
//! installer's prefetch list. Every URL/digest/size is read from the single download
//! registry in [`crate::urls`] — this module holds only the logic that shapes them.

use std::path::PathBuf;

use crate::download::url_basename;
use crate::hash::verify_sha256_cached;
use crate::model_path;
use crate::ort::{onnxruntime_dist, onnxruntime_dylib_file, onnxruntime_dylib_path};
use crate::target::DownloadTarget;

/// A single downloadable asset: its on-disk file name, source URL, and pinned
/// SHA-256 (lowercase hex). A human size label, when shown, is formatted from the
/// manifest's `size_bytes` at the display site — not carried here.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub file_name: String,
    pub url: String,
    /// Pinned lowercase-hex SHA-256. Real model specs always pin a digest.
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
    PARAKEET_JOINER_FILE, PARAKEET_TOKENS_FILE, SEPFORMER_FILE,
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
/// version-gated (see `is_onnxruntime_dylib_version_ok`).
pub fn is_kokoro_present() -> bool {
    let onnx = kokoro_onnx_spec();
    let voices = kokoro_voices_spec();
    let model_ok = model_path(&onnx.file_name)
        .map(|p| verify_sha256_cached(&p, &onnx.sha256))
        .unwrap_or(false);
    let voices_ok = model_path(&voices.file_name)
        .map(|p| verify_sha256_cached(&p, &voices.sha256))
        .unwrap_or(false);
    // Dylib must be present AND the version `ort` needs — a wrong version would
    // deadlock `ort` at session build (see `is_onnxruntime_dylib_version_ok`), so a
    // mismatch reports "not present" here, surfacing as a red dot + re-download
    // prompt instead of a silent hang.
    let dylib_ok = crate::ort::is_onnxruntime_dylib_version_ok();
    model_ok && voices_ok && dylib_ok
}

// ─────────────────────────────────────────────────────────────────────────────
// Parakeet STT model — the cache-aware STREAMING FastConformer transducer (int8 ONNX:
// encoder + decoder LSTM + joiner + tokens), run in-process by `ds-stt::streaming` over the
// SAME shared `ort` (load-dynamic) runtime as Kokoro, so the onnxruntime dylib is shared.
// This REPLACED the old whole-buffer transcribe-rs Parakeet TDT model; the `built_in` STT
// engine keeps the "parakeet" name. All four files load flat from `model_dir()`.
// ─────────────────────────────────────────────────────────────────────────────

/// [`ModelSpec`] for the encoder (`encoder.int8.onnx`, ~132 MB).
pub fn parakeet_encoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_ENCODER)
}

/// [`ModelSpec`] for the decoder LSTM (`decoder.int8.onnx`, ~4 MB).
pub fn parakeet_decoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_DECODER)
}

/// [`ModelSpec`] for the joiner (`joiner.int8.onnx`, ~1.4 MB).
pub fn parakeet_joiner_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_JOINER)
}

/// [`ModelSpec`] for the tokens (`tokens.txt`, ~12 KB).
pub fn parakeet_tokens_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::PARAKEET_TOKENS)
}

/// The directory the streaming model loads from (the flat `model_dir()` holding all four
/// files). `None` only if the data dir won't resolve.
pub fn parakeet_dir() -> Option<PathBuf> {
    ds_config::model_dir()
}

/// Is the FULL Parakeet (streaming) asset set present AND checksum-valid (encoder + decoder +
/// joiner + tokens + the shared onnxruntime dylib)? The STT factory uses this as the cheap,
/// network-free availability probe so it degrades to ClaudeNative when the model is absent.
pub fn is_parakeet_present() -> bool {
    let specs = [
        parakeet_encoder_spec(),
        parakeet_decoder_spec(),
        parakeet_joiner_spec(),
        parakeet_tokens_spec(),
    ];
    let models_ok = specs.iter().all(|spec| {
        model_path(&spec.file_name)
            .map(|p| verify_sha256_cached(&p, &spec.sha256))
            .unwrap_or(false)
    });
    let dylib_ok = onnxruntime_dylib_path()
        .map(|p| p.is_file())
        .unwrap_or(false);
    models_ok && dylib_ok
}

// ─────────────────────────────────────────────────────────────────────────────
// SepFormer speech separator — the macOS dictation speaker-lock's int8 ONNX model
// (single file, loaded from the flat `model_dir()` like the Parakeet set; runs on the
// SAME shared onnxruntime dylib).
// ─────────────────────────────────────────────────────────────────────────────

/// [`ModelSpec`] for the speaker-lock separator (`sepformer_int8.onnx`, ~29 MB).
pub fn sepformer_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::SEPFORMER)
}

/// Is the SepFormer separator present AND checksum-valid (plus the shared onnxruntime
/// dylib it runs on)? Network-free presence probe, mirroring [`is_parakeet_present`] — the
/// speaker-lock fails open when this is false, so the status row must read "missing"
/// rather than green.
pub fn is_sepformer_present() -> bool {
    let spec = sepformer_spec();
    let model_ok = model_path(&spec.file_name)
        .map(|p| verify_sha256_cached(&p, &spec.sha256))
        .unwrap_or(false);
    let dylib_ok = onnxruntime_dylib_path()
        .map(|p| p.is_file())
        .unwrap_or(false);
    model_ok && dylib_ok
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

/// The files the FULL Parakeet (streaming) download fetches, in fetch order (encoder, decoder,
/// joiner, tokens, then the shared onnxruntime dylib `.tgz` on supported platforms).
pub fn parakeet_files() -> Vec<DownloadFile> {
    let mut v = vec![
        DownloadFile::of(crate::urls::PARAKEET_ENCODER),
        DownloadFile::of(crate::urls::PARAKEET_DECODER),
        DownloadFile::of(crate::urls::PARAKEET_JOINER),
        DownloadFile::of(crate::urls::PARAKEET_TOKENS),
    ];
    v.extend(onnxruntime_dylib_file_entry());
    v
}

/// One asset the installer should download for a component, with its pinned digest.
/// `file_name` is BOTH the name to save the download as AND the key
/// `crate::download::prefetch_local` matches — they must stay identical.
#[derive(Debug, Clone)]
pub struct PrefetchItem {
    pub url: String,
    pub file_name: String,
    pub sha256: String,
}

/// The files a component still NEEDS downloaded — already-present, sha-valid assets
/// are omitted, so re-running the installer downloads nothing. Takes the TYPED
/// [`DownloadTarget`] — wire tokens are parsed once at the
/// CLI edge (`ds-helper --print-manifest`), never inside the library. Only
/// [`Onnxruntime`](DownloadTarget::Onnxruntime) | [`KokoroModel`](DownloadTarget::KokoroModel) |
/// [`KokoroVoices`](DownloadTarget::KokoroVoices) | [`ParakeetModel`](DownloadTarget::ParakeetModel) |
/// [`Cuda`](DownloadTarget::Cuda) produce items; every other target yields `vec![]`.
/// This is the SINGLE source of the installer's download list; the URLs/SHAs never
/// leave ds-model.
pub fn prefetch_items(target: DownloadTarget) -> Vec<PrefetchItem> {
    let item = |url: &str, sha: &str| PrefetchItem {
        url: url.to_string(),
        file_name: url_basename(url).to_string(),
        sha256: sha.to_string(),
    };
    let spec_item = |spec: &ModelSpec| -> Option<PrefetchItem> {
        let present = model_path(&spec.file_name)
            .map(|p| verify_sha256_cached(&p, &spec.sha256))
            .unwrap_or(false);
        (!present).then(|| item(&spec.url, &spec.sha256))
    };
    match target {
        // NOTE: the CUDA runtime below is gated on EXISTENCE, not a pinned version (unlike the
        // SHA-checked model specs). So if CUDA_WHEELS is ever bumped, a reinstall will NOT
        // re-fetch it while the old files still exist — the user must clear model_dir().
        // The onnxruntime dylib does NOT have this gap: `is_downloaded_onnxruntime_up_to_date`
        // checks a version marker (not just existence) for the copy we manage, so a
        // ONNXRUNTIME_VERSION bump correctly re-lists it here too.
        DownloadTarget::Onnxruntime => {
            if onnxruntime_dylib_path()
                .map(|p| crate::ort::is_downloaded_onnxruntime_up_to_date(&p))
                .unwrap_or(false)
            {
                return vec![];
            }
            match onnxruntime_dist() {
                Some(d) => vec![item(d.url, d.archive_sha256)],
                None => vec![],
            }
        }
        DownloadTarget::KokoroModel => [kokoro_onnx_spec(), kokoro_voices_spec()]
            .iter()
            .filter_map(&spec_item)
            .collect(),
        DownloadTarget::KokoroVoices => [kokoro_voices_spec()]
            .iter()
            .filter_map(&spec_item)
            .collect(),
        DownloadTarget::ParakeetModel => [
            parakeet_encoder_spec(),
            parakeet_decoder_spec(),
            parakeet_joiner_spec(),
            parakeet_tokens_spec(),
        ]
        .iter()
        .filter_map(&spec_item)
        .collect(),
        DownloadTarget::SepformerModel => {
            [sepformer_spec()].iter().filter_map(&spec_item).collect()
        }
        #[cfg(all(
            any(target_os = "windows", target_os = "linux"),
            target_arch = "x86_64"
        ))]
        DownloadTarget::Cuda => {
            if crate::ort::is_cuda_runtime_present() {
                return vec![];
            }
            crate::ort::CUDA_WHEELS
                .iter()
                .map(|(u, s)| item(u, s))
                .collect()
        }
        // The portable Windows bundle is self-contained. Dotnet/Winapp deliberately have no
        // prefetch manifest because their aka.ms redirects are mutable and cannot be pinned to
        // the SHA required by the downloader. Models / Core ML sets (and off-x86_64 Cuda) also
        // have no dedicated arm and return vec![] via the `_` default.
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes the tests below that mutate the process-wide `DONTSPEAK_MODEL_DIR` env
    // var: without this, the default parallel test runner can interleave two such tests'
    // set/read/restore windows so one observes the other's temp dir instead of its own.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            parakeet_joiner_spec().url,
            parakeet_tokens_spec().url,
            sepformer_spec().url,
        ];
        if let Some(d) = onnxruntime_dist() {
            urls.push(d.url.to_string());
        }
        // Same platform gate as the `prefetch_items` Cuda arm (x86_64 Windows AND Linux) —
        // this was windows-only, silently skipping the CUDA basenames on the Linux leg.
        #[cfg(all(
            any(target_os = "windows", target_os = "linux"),
            target_arch = "x86_64"
        ))]
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
        // not empty — so `ensure`/`is_kokoro_present` actually verify bytes.
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
    fn is_kokoro_present_returns_a_bool_without_panicking() {
        // Hermetic: point `model_dir()` at a FRESH, EMPTY temp dir for the duration of this
        // test via `DONTSPEAK_MODEL_DIR` — the same override `ds_config::model_dir()` already
        // respects for portable/bundled builds, so no new plumbing is needed. Without this the
        // test read whatever the REAL ambient cache dir held: on a machine with the models
        // already downloaded, `is_kokoro_present()` streams + sha256-hashes the ~310 MB onnx PLUS
        // the ~28 MB voices file (and scans the onnxruntime dylib), ballooning this test from
        // milliseconds to 20-60+s. An empty dir also lets us assert a DEFINITE value instead of
        // just "doesn't panic".
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("DONTSPEAK_MODEL_DIR");
        // SAFETY: test-only env mutation, serialized by ENV_LOCK. Restored below before
        // returning.
        unsafe { std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path()) };

        let present: bool = is_kokoro_present();
        assert!(
            !present,
            "a fresh, empty model dir must never read as present"
        );

        // SAFETY: restore the prior value (or clear it) so later tests see the real env again.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }
    }

    #[test]
    fn parakeet_specs_have_right_urls_files_and_pins() {
        let enc = parakeet_encoder_spec();
        assert_eq!(enc.file_name, "encoder.int8.onnx");
        assert!(
            enc.url.contains(
                "sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8/resolve/main/encoder.int8.onnx"
            ),
            "encoder url: {}",
            enc.url
        );
        let dec = parakeet_decoder_spec();
        assert_eq!(dec.file_name, "decoder.int8.onnx");
        let joiner = parakeet_joiner_spec();
        assert_eq!(joiner.file_name, "joiner.int8.onnx");
        let tokens = parakeet_tokens_spec();
        assert_eq!(tokens.file_name, "tokens.txt");
        // All four pin distinct, lowercase, 64-hex digests.
        for spec in [&enc, &dec, &joiner, &tokens] {
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
        assert_ne!(dec.sha256, joiner.sha256);
        assert_ne!(joiner.sha256, tokens.sha256);
    }

    #[test]
    fn is_parakeet_present_returns_a_bool_without_panicking() {
        // Hermetic: same `DONTSPEAK_MODEL_DIR` override as `is_kokoro_present_returns_a_bool_
        // without_panicking` above — on a dev box with the real parakeet assets already
        // downloaded, `is_parakeet_present()` would otherwise sha256-hash them for real.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("DONTSPEAK_MODEL_DIR");
        // SAFETY: test-only env mutation, serialized by ENV_LOCK, restored below.
        unsafe { std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path()) };

        let present = is_parakeet_present();
        assert!(
            !present,
            "a fresh, empty model dir must never read as present"
        );

        // SAFETY: restore the prior value (or clear it) so later tests see the real env again.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }
    }

    #[test]
    fn sepformer_spec_pins_url_file_and_digest() {
        let spec = sepformer_spec();
        assert_eq!(spec.file_name, "sepformer_int8.onnx");
        assert!(
            spec.url
                .contains("sepformer-wsj02mix-int8-onnx/resolve/main/sepformer_int8.onnx"),
            "sepformer url: {}",
            spec.url
        );
        assert_eq!(spec.sha256.len(), 64, "sha256 must be 64 hex chars");
        assert!(
            spec.sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "sha256 must be lowercase hex: {}",
            spec.sha256
        );
        // Hermetic: same `DONTSPEAK_MODEL_DIR` override as `is_kokoro_present_returns_a_bool_
        // without_panicking` above — on a dev box with the real sepformer asset already
        // downloaded, `is_sepformer_present()` would otherwise sha256-hash it for real.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("DONTSPEAK_MODEL_DIR");
        // SAFETY: test-only env mutation, serialized by ENV_LOCK, restored below.
        unsafe { std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path()) };

        let present = is_sepformer_present();
        assert!(
            !present,
            "a fresh, empty model dir must never read as present"
        );

        // SAFETY: restore the prior value (or clear it) so later tests see the real env again.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }
    }
}
