//! `models` tool backend: on-disk inventory + removal, over `ds-ipc`.
//!
//! Lives in the engine because the engine owns the process-local download flights and the
//! authoritative in-flight download state — a cross-process file lock alone would not
//! serialize a removal against a download running in this same process. It is also the only
//! place `DownloadState` is visible.

use std::path::Path;

use ds_config::{Paths, VoiceConfig};
use ds_model::DownloadTarget;

use crate::downloads::{DownloadProg, TargetState};

/// Targets currently transferring — the authoritative "is this downloading right now".
fn active_targets(downloads: &DownloadProg) -> Vec<DownloadTarget> {
    downloads
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .targets
        .iter()
        .filter(|(_, state)| matches!(state, TargetState::Active(_)))
        .map(|(target, _)| *target)
        .collect()
}

/// Refuse a removal the engine would immediately undo, or that would break an installed
/// model. `None` ⇒ the removal may proceed. First match wins.
fn refusal(root: &Path, cfg: &VoiceConfig, id: &str, active: &[DownloadTarget]) -> Option<String> {
    let Some(targets) = ds_model::asset_targets(id) else {
        // The tool schema already rejects this, so reaching here means the CLI and the engine
        // disagree about the asset list.
        return Some(format!(
            "models: unknown model `{id}` — the running engine may be older than this CLI"
        ));
    };
    if !targets
        .iter()
        .any(|target| target.is_supported_on_this_host())
    {
        // The remove enum is one platform-independent list while `scan_at` drops a row this
        // host cannot hold; without this the answer is a confusing 0-byte "success".
        return Some(format!("models: `{id}` is not available on this platform"));
    }
    if ds_model::asset_in_use(cfg, id) {
        // Distinct texts: on Windows and Linux the STT ladder resolves to built_in by
        // default, so pointing that user at tts_model would be actively wrong.
        return Some(if id == ds_config::STT_MODEL_TOKEN {
            format!(
                "models: `{id}` is the active STT model — switch with set_config stt_engine first"
            )
        } else {
            format!(
                "models: `{id}` is the active TTS model — switch with set_config tts_model first"
            )
        });
    }
    if ds_model::shared_asset_referenced(root, cfg, id) {
        return Some(match id {
            ds_config::ONNXRUNTIME_ASSET_TOKEN => format!(
                "models: `{id}` is still needed by an installed or selected ONNX model — remove those models first"
            ),
            ds_config::KOKORO_FRONTEND_ASSET_TOKEN => format!(
                "models: `{id}` is still needed by Kokoro — remove or deselect `kokoro` first"
            ),
            _ => format!(
                "models: `{id}` is the resolved compute provider — set_config provider without `cuda` first"
            ),
        });
    }
    if ds_model::is_shared_asset(id) && !active.is_empty() {
        // `DownloadTarget::Onnxruntime` is never an engine download target — every ORT fetch
        // is a step inside another target's setup — so the per-target check below cannot see
        // the Chatterbox download that is about to install the dylib.
        return Some(format!(
            "models: `{id}` is shared and a download is in flight — try again when it finishes"
        ));
    }
    if targets.iter().any(|target| active.contains(target)) {
        return Some(format!(
            "models: `{id}` is downloading right now — try again when it finishes"
        ));
    }
    None
}

/// List the on-disk inventory, optionally removing one model or shared-asset id first. A
/// failed removal answers with an error and no payload — never a success carrying a stale row.
pub(crate) fn respond(
    paths: &Paths,
    downloads: &DownloadProg,
    root: &Path,
    remove: Option<&str>,
) -> ds_ipc::Response {
    let cfg = VoiceConfig::load(paths);
    let active = active_targets(downloads);
    let mut removed = None;
    if let Some(id) = remove {
        if let Some(message) = refusal(root, &cfg, id, &active) {
            return ds_ipc::Response::error(message);
        }
        match ds_model::remove_at(root, &cfg, id) {
            Ok(bytes) => removed = Some((id, bytes)),
            Err(e) => {
                return ds_ipc::Response::error(format!("models: could not remove `{id}`: {e}"));
            }
        }
    }
    ds_ipc::Response::Models {
        models: ds_model::inventory_json(root, &cfg, &active, removed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloads::{DownloadProgress, DownloadState};
    use std::sync::{Arc, Mutex};

    /// Guards a fixture against a `DONTSPEAK_MODEL_DIR` that covers `TMPDIR`: the flight would
    /// then take its sweep gate in the REAL model dir (#204).
    fn assert_fixture_is_isolated(root: &Path, path: &Path) {
        assert!(
            ds_model::sweep_root_of(path).is_some_and(|resolved| resolved.starts_with(root)),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );
    }

    fn seed_onnx_set(root: &Path, model: ds_config::TtsModel) {
        let set = ds_model::tts_ort_asset_set(model);
        let dir = set
            .dir_name
            .map_or_else(|| root.to_path_buf(), |name| root.join(name));
        std::fs::create_dir_all(&dir).unwrap();
        for file in set.files_for(false) {
            std::fs::write(dir.join(file.file_name), b"weights").unwrap();
        }
    }

    fn seed_ort_dylib(root: &Path) -> std::path::PathBuf {
        let dylib = root.join(ds_model::onnxruntime_dylib_file());
        std::fs::write(&dylib, b"managed runtime").unwrap();
        dylib
    }

    /// `Paths::rooted_at` keeps the fixture off the developer's real config.toml.
    fn fixture(config: &str) -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(dir.path());
        std::fs::create_dir_all(paths.config_toml.parent().unwrap()).unwrap();
        std::fs::write(&paths.config_toml, config).unwrap();
        (dir, paths)
    }

    fn downloads(active: &[DownloadTarget]) -> DownloadProg {
        let mut state = DownloadState::default();
        for target in active {
            state.targets.insert(
                *target,
                TargetState::Active(DownloadProgress { done: 1, total: 2 }),
            );
        }
        Arc::new(Mutex::new(state))
    }

    fn error_of(response: ds_ipc::Response) -> String {
        match response {
            ds_ipc::Response::Error { message } => message,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    fn payload(response: ds_ipc::Response) -> serde_json::Value {
        match response {
            ds_ipc::Response::Models { models } => models,
            other => panic!("expected a models payload, got {other:?}"),
        }
    }

    #[test]
    fn the_active_tts_model_is_refused_by_name() {
        let (_config, paths) = fixture("tts_engine = \"built_in\"\ntts_model = \"chatterbox\"\n");
        let models = tempfile::tempdir().unwrap();
        let message = error_of(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("chatterbox"),
        ));
        assert_eq!(
            message,
            "models: `chatterbox` is the active TTS model — switch with set_config tts_model first"
        );
    }

    #[test]
    fn the_active_stt_model_is_refused_with_its_own_engine_key() {
        let (_config, paths) = fixture("stt_engine = \"built_in\"\n");
        let models = tempfile::tempdir().unwrap();
        let message = error_of(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("parakeet"),
        ));
        assert_eq!(
            message,
            "models: `parakeet` is the active STT model — switch with set_config stt_engine first"
        );
    }

    #[test]
    fn a_downloading_model_is_refused() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let message = error_of(respond(
            &paths,
            &downloads(&[DownloadTarget::QwenMlx]),
            models.path(),
            Some("qwen"),
        ));
        assert_eq!(
            message,
            "models: `qwen` is downloading right now — try again when it finishes"
        );
    }

    #[test]
    fn a_referenced_shared_id_is_refused_engine_side_too() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        seed_onnx_set(models.path(), ds_config::TtsModel::Chatterbox);
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[]),
                models.path(),
                Some("onnxruntime")
            )),
            "models: `onnxruntime` is still needed by an installed or selected ONNX model — remove those models first"
        );

        seed_onnx_set(models.path(), ds_config::TtsModel::Kokoro);
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[]),
                models.path(),
                Some("kokoro_frontend")
            )),
            "models: `kokoro_frontend` is still needed by Kokoro — remove or deselect `kokoro` first"
        );
    }

    #[test]
    fn an_unreferenced_shared_asset_is_reclaimed() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\nprovider = [\"cpu\"]\n");
        let models = tempfile::tempdir().unwrap();
        let dylib = seed_ort_dylib(models.path());
        assert_fixture_is_isolated(models.path(), &dylib);

        let after = payload(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("onnxruntime"),
        ));
        assert_eq!(after["removed"]["id"], "onnxruntime");
        assert!(after["removed"]["bytes"].as_u64().unwrap() > 0);
        let row = after["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|asset| asset["id"] == "onnxruntime")
            .unwrap()
            .clone();
        assert_eq!(row["installed"], false);
        assert!(!dylib.exists());
    }

    /// The CUDA runtime is referenced by SELECTION alone, so this fixture must resolve a
    /// built-in engine — with a model whose own row is not the one being removed.
    #[test]
    fn the_cuda_runtime_follows_the_resolved_provider() {
        let cfg = |provider: &str| {
            format!(
                "tts_engine = \"built_in\"\ntts_model = \"chatterbox\"\nstt_engine = []\nprovider = [\"{provider}\"]\n"
            )
        };
        if !DownloadTarget::Cuda.is_supported_on_this_host() {
            let (_config, paths) = fixture(&cfg("cpu"));
            let models = tempfile::tempdir().unwrap();
            assert_eq!(
                error_of(respond(
                    &paths,
                    &downloads(&[]),
                    models.path(),
                    Some("cuda")
                )),
                "models: `cuda` is not available on this platform"
            );
            return;
        }

        let (_config, paths) = fixture(&cfg("cuda"));
        let models = tempfile::tempdir().unwrap();
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[]),
                models.path(),
                Some("cuda")
            )),
            "models: `cuda` is the resolved compute provider — set_config provider without `cuda` first"
        );

        let (_config, paths) = fixture(&cfg("cpu"));
        let models = tempfile::tempdir().unwrap();
        assert_fixture_is_isolated(models.path(), &models.path().join("cuda"));
        let after = payload(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("cuda"),
        ));
        assert_eq!(after["removed"]["id"], "cuda");
    }

    /// `DownloadTarget::Onnxruntime` is never an engine download target — the dylib installs
    /// as a step inside another target's setup — so only the shared clause can catch this.
    #[test]
    fn a_shared_id_is_refused_while_any_download_is_in_flight() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\nprovider = [\"cpu\"]\n");
        let models = tempfile::tempdir().unwrap();
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[DownloadTarget::ChatterboxModel]),
                models.path(),
                Some("onnxruntime")
            )),
            "models: `onnxruntime` is shared and a download is in flight — try again when it finishes"
        );
    }

    /// An id this engine does not list is version skew, not a shared asset: a CLI updated
    /// ahead of the app-hosted engine advertises the new token in its schema, and the stale
    /// engine must point at itself rather than at a shared-asset rule that does not apply.
    #[test]
    fn an_unlisted_id_names_the_engine_as_the_stale_side() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let message = error_of(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("future_model"),
        ));
        assert_eq!(
            message,
            "models: unknown model `future_model` — the running engine may be older than this CLI"
        );
    }

    #[test]
    fn a_successful_removal_reports_the_reclaimed_bytes_and_flips_the_row() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let dir = models.path().join("qwen3-tts");
        assert_fixture_is_isolated(models.path(), &dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("talker_cache.onnx"), b"weights on disk").unwrap();

        let before = payload(respond(&paths, &downloads(&[]), models.path(), None));
        let row = |value: &serde_json::Value| {
            value["assets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|asset| asset["id"] == "qwen")
                .unwrap()
                .clone()
        };
        assert_eq!(row(&before)["bytes"], 15);
        assert_eq!(row(&before)["removable"], true);

        let after = payload(respond(
            &paths,
            &downloads(&[]),
            models.path(),
            Some("qwen"),
        ));
        assert_eq!(
            after["removed"],
            serde_json::json!({ "id": "qwen", "bytes": 15 })
        );
        assert_eq!(row(&after)["installed"], false);
        assert_eq!(row(&after)["bytes"], 0);
        assert!(!dir.exists());
    }
}
