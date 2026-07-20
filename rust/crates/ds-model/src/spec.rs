//! Model specs + manifests: the `ModelSpec`/`DownloadFile`/`PrefetchItem` builders (Kokoro
//! TTS, Parakeet STT), the network-free presence probes the engine factory uses, and the
//! installer's prefetch list. Every URL/digest/size is read from the single download
//! registry in [`crate::urls`] — this module holds only the logic that shapes them.

use std::path::PathBuf;

use crate::download::prefetch_key;
use crate::hash::verify_sha256_cached;
use crate::model_path;
use crate::ort::{onnxruntime_dist, onnxruntime_dylib_file, onnxruntime_dylib_path};
use crate::target::DownloadTarget;
use crate::urls::Download;

/// On-disk name + URL + pinned SHA-256. Size labels come from manifest `size_bytes` at display.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub file_name: String,
    pub url: String,
    /// Lowercase-hex; real specs always pin a digest.
    pub sha256: String,
}

impl ModelSpec {
    /// From registry [`crate::urls::Download`] (sole URL/digest source).
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
    KOKORO_G2P_DECODER_FILE, KOKORO_G2P_ENCODER_FILE, KOKORO_ONNX_FILE, KOKORO_VOICES_FILE,
    PARAKEET_DECODER_FILE, PARAKEET_ENCODER_FILE, PARAKEET_JOINER_FILE, PARAKEET_TOKENS_FILE,
    SEPFORMER_FILE,
};

pub fn kokoro_onnx_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_ONNX)
}

pub fn kokoro_voices_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_VOICES)
}

pub fn kokoro_g2p_encoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_G2P_ENCODER)
}

pub fn kokoro_g2p_decoder_spec() -> ModelSpec {
    ModelSpec::of(crate::urls::KOKORO_G2P_DECODER)
}

pub fn is_kokoro_g2p_present() -> bool {
    let graphs_ok = [kokoro_g2p_encoder_spec(), kokoro_g2p_decoder_spec()]
        .iter()
        .all(|spec| {
            model_path(&spec.file_name)
                .map(|p| verify_sha256_cached(&p, &spec.sha256))
                .unwrap_or(false)
        });
    graphs_ok && crate::ort::is_onnxruntime_dylib_version_ok()
}

/// Shared Kokoro text frontend assets (G2P + version-checked ORT).
pub fn is_kokoro_frontend_present() -> bool {
    is_kokoro_g2p_present()
}

/// Full portable Kokoro set present (SHA + ORT version-gate). TTS factory fail-quiet probe.
pub fn is_kokoro_present() -> bool {
    let onnx = kokoro_onnx_spec();
    let model_ok = model_path(&onnx.file_name)
        .map(|p| verify_sha256_cached(&p, &onnx.sha256))
        .unwrap_or(false);
    let voices = kokoro_voices_spec();
    let voices_ok = model_path(&voices.file_name)
        .map(|p| verify_sha256_cached(&p, &voices.sha256))
        .unwrap_or(false);
    model_ok && voices_ok && is_kokoro_frontend_present()
}

// ─────────────────────────────────────────────────────────────────────────────
// Parakeet STT — streaming FastConformer transducer (int8 ONNX: encoder + decoder LSTM +
// joiner + tokens) via `ds-stt::streaming` on the shared `ort` runtime as Kokoro.
// All four files load flat from `model_dir()`.
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

/// Text frontend shared by both Kokoro synthesis backends: unknown-word G2P graphs and ORT.
pub fn kokoro_frontend_files() -> Vec<DownloadFile> {
    let mut v = vec![
        DownloadFile::of(crate::urls::KOKORO_G2P_ENCODER),
        DownloadFile::of(crate::urls::KOKORO_G2P_DECODER),
    ];
    v.extend(onnxruntime_dylib_file_entry());
    v
}

/// The files the full portable Kokoro download fetches, in fetch order. All URLs/sizes come
/// from the registry; the text-frontend subset is also used by the MLX backend.
pub fn kokoro_files() -> Vec<DownloadFile> {
    let mut v = vec![
        DownloadFile::of(crate::urls::KOKORO_ONNX),
        DownloadFile::of(crate::urls::KOKORO_VOICES),
    ];
    v.extend(kokoro_frontend_files());
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
/// `file_name` is BOTH the name to save the download as AND the staging key the
/// downloader's prefetch lookup matches — always [`prefetch_key`]`(url)`, never the bare
/// basename (several assets share one).
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
/// ORT runtime, named ONNX model, Kokoro frontend, SepFormer, and CUDA targets produce items;
/// repository-based MLX and installer-group targets yield `vec![]`.
/// This is the SINGLE source of the installer's download list; the URLs/SHAs never
/// leave ds-model.
pub fn prefetch_items(target: DownloadTarget) -> Vec<PrefetchItem> {
    let item = |url: &str, sha: &str| PrefetchItem {
        url: url.to_string(),
        file_name: prefetch_key(url),
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
        // Per-file, sha-gated, subdir-aware: one missing/stale file re-lists only itself,
        // never the whole multi-GB set. The ORT dylib is `DownloadTarget::Onnxruntime`'s
        // concern, not a condition here. (`spec_item` can't be reused: it resolves through
        // the flat `model_path`, wrong for the per-model subdirectory sets.)
        DownloadTarget::KokoroModel
        | DownloadTarget::ChatterboxModel
        | DownloadTarget::QwenModel
        | DownloadTarget::OmniVoiceModel => {
            let model = target.tts_model().expect("matched TTS target");
            let dir = crate::tts_assets::tts_model_dir(model);
            tts_prefetch_items(crate::tts_assets::tts_ort_asset_set(model).files, |file| {
                dir.as_deref()
                    .map(|d| verify_sha256_cached(&d.join(file.file_name), file.sha256))
                    .unwrap_or(false)
            })
        }
        DownloadTarget::KokoroFrontend => [kokoro_g2p_encoder_spec(), kokoro_g2p_decoder_spec()]
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
        // the SHA required by the downloader. MLX sets and off-x86_64 CUDA have no static
        // manifest because MLX downloads a pinned repository tree rather than named files.
        _ => vec![],
    }
}

/// The TTS half of [`prefetch_items`]: list only the files `present` denies, keyed by
/// [`prefetch_key`]. `present` is injected so partial-install filtering is unit-testable
/// without model bytes (`verify_sha256_cached` has no fakeable sidecar).
fn tts_prefetch_items(
    files: &[Download],
    present: impl Fn(&Download) -> bool,
) -> Vec<PrefetchItem> {
    files
        .iter()
        .filter(|file| !present(file))
        .map(|file| PrefetchItem {
            url: file.url.to_string(),
            file_name: prefetch_key(file.url),
            sha256: file.sha256.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes the tests below that mutate the process-wide `DONTSPEAK_MODEL_DIR` env
    // var: without this, the default parallel test runner can interleave two such tests'
    // set/read/restore windows so one observes the other's temp dir instead of its own.
    // Crate-level (`crate::TEST_ENV_LOCK`) so tts_assets.rs's hermetic tests serialize on
    // the SAME lock — two module-local locks would not serialize against each other.
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    // The installer stages each prefetched file under prefetch_key(url): the manifest
    // saves the download under that key and prefetch_local() looks it up by it. Several
    // TTS assets intentionally SHARE a basename (`config.json`, `tokenizer.json`,
    // `tokenizer_config.json` across per-model subdirs), so uniqueness must hold on the
    // URL and its prefetch key — a collision would cross-wire two staged assets. Guard
    // EVERY registered download so a future URL edit can't silently break the installer.
    #[test]
    fn prefetch_keys_are_unique_and_nonempty() {
        // TTS_ORT_ASSETS covers the full Kokoro set (onnx, voices, both G2P graphs) plus
        // the Chatterbox/Qwen/OmniVoice sets — no separate kokoro_*_spec() pushes.
        let mut urls: Vec<String> = crate::tts_assets::TTS_ORT_ASSETS
            .iter()
            .flat_map(|set| set.files.iter())
            .map(|file| file.url.to_string())
            .collect();
        urls.extend([
            parakeet_encoder_spec().url,
            parakeet_decoder_spec().url,
            parakeet_joiner_spec().url,
            parakeet_tokens_spec().url,
            sepformer_spec().url,
        ]);
        if let Some(d) = onnxruntime_dist() {
            urls.push(d.url.to_string());
        }
        // Same platform gate as the `prefetch_items` Cuda arm (x86_64 Windows AND Linux) —
        // this was windows-only, silently skipping the CUDA wheels on the Linux leg.
        #[cfg(all(
            any(target_os = "windows", target_os = "linux"),
            target_arch = "x86_64"
        ))]
        for (u, _) in crate::ort::CUDA_WHEELS {
            urls.push(u.to_string());
        }

        let total = urls.len();
        let mut unique_urls = urls.clone();
        unique_urls.sort_unstable();
        unique_urls.dedup();
        assert_eq!(
            total,
            unique_urls.len(),
            "two registered downloads share a URL — one asset would shadow the other"
        );

        let mut keys: Vec<String> = Vec::with_capacity(urls.len());
        for u in &urls {
            assert!(
                !crate::download::url_basename(u).is_empty(),
                "a source URL has no basename: {u}"
            );
            keys.push(prefetch_key(u));
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            total,
            keys.len(),
            "two source URLs share a prefetch key — the installer staging would \
             cross-wire them"
        );
    }

    /// Partial-install filtering is PURE over the injected presence probe: only absent
    /// files are listed, each keyed by `prefetch_key(url)` — one stale file must never
    /// re-list the whole set.
    #[test]
    fn tts_prefetch_items_list_only_absent_files_with_prefetch_keys() {
        let files = crate::tts_assets::tts_ort_asset_set(ds_config::TtsModel::Chatterbox).files;
        let present_name = files[0].file_name;

        let partial = tts_prefetch_items(files, |file| file.file_name == present_name);
        assert_eq!(partial.len(), files.len() - 1);
        assert!(
            partial.iter().all(|i| i.url != files[0].url),
            "the present file must be omitted"
        );
        for item in &partial {
            assert_eq!(item.file_name, prefetch_key(&item.url));
            assert!(!item.sha256.is_empty());
        }

        assert!(
            tts_prefetch_items(files, |_| true).is_empty(),
            "a fully present set downloads nothing"
        );
        assert_eq!(tts_prefetch_items(files, |_| false).len(), files.len());
    }

    /// Empty model dir: subdirectory model lists every file under prefetch keys.
    #[test]
    fn tts_model_prefetch_lists_every_file_for_an_empty_model_dir() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("DONTSPEAK_MODEL_DIR");
        // SAFETY: test-only env mutation, serialized by ENV_LOCK, restored below.
        unsafe { std::env::set_var("DONTSPEAK_MODEL_DIR", tmp.path()) };

        let items = prefetch_items(DownloadTarget::QwenModel);

        // SAFETY: restore the prior value (or clear it) so later tests see the real env again.
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DONTSPEAK_MODEL_DIR", v),
                None => std::env::remove_var("DONTSPEAK_MODEL_DIR"),
            }
        }

        let files = crate::tts_assets::tts_ort_asset_set(ds_config::TtsModel::Qwen).files;
        assert_eq!(items.len(), files.len());
        for (item, file) in items.iter().zip(files) {
            assert_eq!(item.url, file.url);
            assert_eq!(item.file_name, prefetch_key(file.url));
            assert_eq!(item.sha256, file.sha256);
        }
    }

    #[test]
    fn kokoro_specs_have_right_urls_and_files() {
        let onnx = kokoro_onnx_spec();
        assert_eq!(onnx.file_name, "kokoro-v1.0-fp16.onnx");
        assert_eq!(
            onnx.url,
            "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/1939ad2a8e416c0acfeecc08a694d14ef25f2231/onnx/model_fp16.onnx"
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
        // Pins are 64-hex lowercase release digests (empty would skip verify).
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
    fn kokoro_g2p_specs_pin_the_immutable_export_graphs() {
        let encoder = kokoro_g2p_encoder_spec();
        let decoder = kokoro_g2p_decoder_spec();
        assert_eq!(encoder.file_name, "encoder_model.onnx");
        assert_eq!(decoder.file_name, "decoder_model.onnx");
        for spec in [&encoder, &decoder] {
            assert!(
                spec.url
                    .contains("9470bafd46d1e5c05225f2942853b1de90bc9658/onnx/"),
                "G2P URL is not revision-pinned: {}",
                spec.url
            );
            assert_eq!(spec.sha256.len(), 64);
            assert!(spec.sha256.bytes().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(encoder.sha256, decoder.sha256);

        let frontend: Vec<String> = kokoro_frontend_files()
            .into_iter()
            .map(|file| file.file_name)
            .collect();
        assert!(frontend.contains(&encoder.file_name));
        assert!(frontend.contains(&decoder.file_name));
    }

    #[test]
    fn is_kokoro_present_returns_a_bool_without_panicking() {
        // Hermetic empty model dir (ambient cache would hash hundreds of MB).
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
                "sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-1040ms-int8/resolve/main/encoder.int8.onnx"
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
        let variant = "sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-1040ms-int8";
        for spec in [&enc, &dec, &joiner, &tokens] {
            assert!(
                spec.url.contains(variant),
                "portable STT assets must all use the 1040ms export: {}",
                spec.url
            );
        }
        assert!(crate::urls::PARAKEET.usage.contains("1040ms"));
        assert!(crate::urls::PARAKEET.homepage.contains("1040ms"));
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
        // Hermetic empty model dir (same ambient-cache trap as kokoro present test).
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
        // Hermetic empty model dir (same ambient-cache trap as kokoro present test).
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
