//! ds-model tests that set `DONTSPEAK_MODEL_DIR`, isolated in their own binary.
//!
//! Edition 2024: `set_var` is unsafe if a concurrent `getenv` races. A mutex only helps when
//! readers take it too; the lib test binary has many ambient `model_dir()` readers. Process
//! isolation makes these writes sound — this binary has no other tests, so `ENV_LOCK` covers
//! every observer. Drift guard: lib `src/` stays writer-free except `ort::set_ort_dylib_path`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use ds_config::TtsModel;
use ds_model::{
    DownloadTarget, is_kokoro_present, is_parakeet_present, is_sepformer_present, prefetch_items,
    prefetch_key, tts_model_dir, tts_model_files_present, tts_ort_asset_set,
};

const MODEL_DIR_VAR: &str = "DONTSPEAK_MODEL_DIR";

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Empty tempdir as `DONTSPEAK_MODEL_DIR` while holding `ENV_LOCK` (avoids ambient cache).
struct EmptyModelDir {
    tmp: tempfile::TempDir,
    previous: Option<OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl EmptyModelDir {
    fn enter() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os(MODEL_DIR_VAR);
        // SAFETY: `ENV_LOCK` is held until this guard drops and every env reader in this
        // binary runs behind it, so no thread can be inside `getenv` concurrently.
        unsafe { std::env::set_var(MODEL_DIR_VAR, tmp.path()) };
        Self {
            tmp,
            previous,
            _lock: lock,
        }
    }

    fn path(&self) -> &Path {
        self.tmp.path()
    }
}

impl Drop for EmptyModelDir {
    /// Restore ambient even on panic (leaked override would point later tests at a deleted dir).
    fn drop(&mut self) {
        // SAFETY: Drop order keeps `_lock` until after this restore.
        unsafe {
            match self.previous.take() {
                Some(value) => std::env::set_var(MODEL_DIR_VAR, value),
                None => std::env::remove_var(MODEL_DIR_VAR),
            }
        }
    }
}

#[test]
fn tts_model_prefetch_lists_every_file_for_an_empty_model_dir() {
    let _model_dir = EmptyModelDir::enter();

    let items = prefetch_items(DownloadTarget::QwenModel);

    let files = tts_ort_asset_set(TtsModel::Qwen).files;
    assert_eq!(items.len(), files.len());
    for (item, file) in items.iter().zip(files) {
        assert_eq!(item.url, file.url);
        assert_eq!(item.file_name, prefetch_key(file.url));
        assert_eq!(item.sha256, file.sha256);
    }
}

#[test]
fn omnivoice_prefetch_manifest_stages_no_cuda_asset() {
    let _model_dir = EmptyModelDir::enter();

    let items = prefetch_items(DownloadTarget::OmniVoiceModel);

    let set = tts_ort_asset_set(TtsModel::OmniVoice);
    assert_eq!(items.len(), set.files.len());
    assert!(
        set.cuda_files.is_empty(),
        "one profile serves every provider"
    );
    for item in &items {
        assert!(
            !item.url.contains("/cuda/"),
            "staged CUDA asset: {}",
            item.url
        );
    }
}

/// Fresh dir: factory probes must read absent (wrong answer strands the engine).
#[test]
fn presence_probes_read_absent_on_an_empty_model_dir() {
    let _model_dir = EmptyModelDir::enter();

    assert!(!is_kokoro_present());
    assert!(!is_parakeet_present());
    assert!(!is_sepformer_present());
}

#[test]
fn model_subdirectories_do_not_collide() {
    let model_dir = EmptyModelDir::enter();

    assert_eq!(
        tts_model_dir(TtsModel::Kokoro),
        Some(model_dir.path().to_path_buf())
    );
    assert_eq!(
        tts_model_dir(TtsModel::Qwen),
        Some(model_dir.path().join("qwen3-tts"))
    );
    assert!(!tts_model_files_present(TtsModel::OmniVoice, false));
    assert!(!tts_model_files_present(TtsModel::OmniVoice, true));
}

/// Ambient `fluid_kokoro_dir_arg` is the Core ML root (see `kokoro_hub_layout`).
#[cfg(target_os = "macos")]
#[test]
fn fluid_kokoro_dir_arg_is_the_coreml_root() {
    let model_dir = EmptyModelDir::enter();
    let coreml = model_dir.path().join("coreml");

    let arg = ds_model::shim::fluid_kokoro_dir_arg();
    let arg = PathBuf::from(arg.to_str().expect("utf-8 model path"));

    assert_eq!(arg, coreml);
    assert_ne!(
        arg,
        coreml.join("kokoro-82m-coreml"),
        "root, not the set dir"
    );
}

/// Lib `src/` mutates env only in `ort` — other writers belong in this binary (#204).
#[test]
fn crate_sources_keep_env_mutation_to_the_single_ort_writer() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut writers: Vec<String> = Vec::new();
    collect_env_writers(&src, &mut writers);
    writers.sort_unstable();
    writers.dedup();
    assert_eq!(
        writers,
        ["ort.rs"],
        "ds-model/src mutates the environment outside `ort::set_ort_dylib_path` — a test that \
         needs its own model dir belongs in this binary instead (#204)"
    );
}

fn collect_env_writers(dir: &Path, writers: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
    for entry in entries {
        let path: PathBuf = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_env_writers(&path, writers);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let name = path.file_name().expect("file name").to_string_lossy();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        if source.contains("env::set_var(") || source.contains("env::remove_var(") {
            writers.push(name.into_owned());
        }
    }
}
