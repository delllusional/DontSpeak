//! `models` tool: inventory + removal over `ds-ipc`.
//!
//! Engine-local: only here sees `DownloadState` / in-process flights (file locks can't
//! serialize remove vs same-process download).

use ds_config::{Paths, VoiceConfig};
use ds_model::{DownloadTarget, ModelRoots};

use crate::downloads::{DownloadProg, TargetState};

/// In-flight targets (authoritative "downloading right now").
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

/// Cause text per [`ds_config::SHARED_ASSET_TOKENS`] (one arm each; test pins completeness).
fn shared_refusal(id: &str) -> String {
    match id {
        ds_config::ONNXRUNTIME_ASSET_TOKEN => format!(
            "models: `{id}` is still needed by an installed or selected ONNX model — remove those models first"
        ),
        ds_config::KOKORO_FRONTEND_ASSET_TOKEN => {
            format!("models: `{id}` is still needed by Kokoro — remove or deselect `kokoro` first")
        }
        ds_config::CUDA_ASSET_TOKEN => format!(
            "models: `{id}` is the resolved compute provider — set_config provider without `cuda` first"
        ),
        _ => unnamed_shared_refusal(id),
    }
}

/// Fallback when [`shared_refusal`] has no named cause.
fn unnamed_shared_refusal(id: &str) -> String {
    format!("models: `{id}` is shared and still referenced — remove or deselect what needs it")
}

fn unavailable_refusal(id: &str) -> String {
    format!("models: `{id}` is not available on this platform")
}

/// Refuse unsafe/undoable removal. `None` = may proceed. First match wins.
fn refusal(
    roots: &ModelRoots,
    cfg: &VoiceConfig,
    id: &str,
    active: &[DownloadTarget],
) -> Option<String> {
    let Some(targets) = ds_model::asset_targets(id) else {
        // Schema already rejects — CLI/engine asset-list skew.
        return Some(format!(
            "models: unknown model `{id}` — the running engine may be older than this CLI"
        ));
    };
    if !targets
        .iter()
        .any(|target| target.is_supported_on_this_host())
    {
        // Platform-independent remove enum vs host-dropped scan rows → avoid 0-byte "success".
        return Some(unavailable_refusal(id));
    }
    if ds_model::asset_in_use(cfg, id) {
        // Distinct STT/TTS texts (Win/Linux STT default is built_in).
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
    if ds_model::shared_asset_referenced(roots, cfg, id) {
        return Some(shared_refusal(id));
    }
    if ds_model::is_shared_asset(id) && !active.is_empty() {
        // ORT is never its own engine target — per-target check can't see the parent fetch.
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

/// Inventory, optionally remove first. Failed remove → error, never stale success payload.
pub(crate) fn respond(
    paths: &Paths,
    downloads: &DownloadProg,
    roots: &ModelRoots,
    remove: Option<&str>,
) -> ds_ipc::Response {
    let cfg = VoiceConfig::load(paths);
    let active = active_targets(downloads);
    let mut removed = None;
    if let Some(id) = remove {
        if let Some(message) = refusal(roots, &cfg, id, &active) {
            return ds_ipc::Response::error(message);
        }
        match ds_model::remove_at(roots, &cfg, id) {
            Ok(bytes) => removed = Some((id, bytes)),
            Err(e) => {
                return ds_ipc::Response::error(format!("models: could not remove `{id}`: {e}"));
            }
        }
    }
    ds_ipc::Response::Models {
        models: ds_model::inventory_json(roots, &cfg, &active, removed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloads::{DownloadProgress, DownloadState};
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    const MCP_TOOLS_DOC: &str = include_str!("../../../../docs/MCP-TOOLS.md");

    fn normalize(s: &str) -> String {
        s.replace('`', "")
            .replace(['—', '–'], "--")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Fixture must not sit under `DONTSPEAK_MODEL_DIR` (#204 sweep into real model dir).
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
        std::fs::create_dir_all(root).unwrap();
        let dylib = root.join(ds_model::onnxruntime_dylib_file());
        std::fs::write(&dylib, b"managed runtime").unwrap();
        dylib
    }

    /// Isolated config root (not the developer's real config.toml).
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

    /// docs/MCP-TOOLS.md quotes these refusals.
    #[test]
    fn mcp_tools_doc_matches_models_refusals() {
        let doc = normalize(MCP_TOOLS_DOC);
        let documented = |message: String| {
            assert!(
                doc.contains(&normalize(&message)),
                "docs/MCP-TOOLS.md is missing the models refusal:\n{message}"
            );
        };

        let (_config, paths) =
            fixture("tts_engine = \"built_in\"\ntts_model = \"kokoro\"\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        documented(refusal(&roots, &VoiceConfig::load(&paths), "kokoro", &[]).unwrap());

        let (_config, paths) = fixture("tts_engine = []\nstt_engine = \"built_in\"\n");
        documented(refusal(&roots, &VoiceConfig::load(&paths), "parakeet", &[]).unwrap());

        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let cfg = VoiceConfig::load(&paths);
        documented(
            refusal(
                &roots,
                &cfg,
                "chatterbox",
                &[DownloadTarget::ChatterboxModel],
            )
            .unwrap(),
        );

        let referenced_models = tempfile::tempdir().unwrap();
        let referenced_roots = ModelRoots::under(referenced_models.path());
        seed_onnx_set(&referenced_roots.model, ds_config::TtsModel::Chatterbox);
        documented(refusal(&referenced_roots, &cfg, "onnxruntime", &[]).unwrap());
        seed_onnx_set(&referenced_roots.model, ds_config::TtsModel::Kokoro);
        documented(refusal(&referenced_roots, &cfg, "kokoro_frontend", &[]).unwrap());

        documented(shared_refusal("cuda"));
        documented(unavailable_refusal("cuda"));
        documented(
            refusal(
                &roots,
                &cfg,
                "onnxruntime",
                &[DownloadTarget::ChatterboxModel],
            )
            .unwrap(),
        );
        documented(refusal(&roots, &cfg, "<id>", &[]).unwrap());
    }

    #[test]
    fn the_active_tts_model_is_refused_by_name() {
        let (_config, paths) = fixture("tts_engine = \"built_in\"\ntts_model = \"chatterbox\"\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        let message = error_of(respond(&paths, &downloads(&[]), &roots, Some("chatterbox")));
        assert_eq!(
            message,
            "models: `chatterbox` is the active TTS model — switch with set_config tts_model first"
        );
    }

    #[test]
    fn the_active_stt_model_is_refused_with_its_own_engine_key() {
        let (_config, paths) = fixture("stt_engine = \"built_in\"\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        let message = error_of(respond(&paths, &downloads(&[]), &roots, Some("parakeet")));
        assert_eq!(
            message,
            "models: `parakeet` is the active STT model — switch with set_config stt_engine first"
        );
    }

    #[test]
    fn a_downloading_model_is_refused() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        let message = error_of(respond(
            &paths,
            &downloads(&[DownloadTarget::QwenMlx]),
            &roots,
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
        let roots = ModelRoots::under(models.path());
        seed_onnx_set(&roots.model, ds_config::TtsModel::Chatterbox);
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[]),
                &roots,
                Some("onnxruntime")
            )),
            "models: `onnxruntime` is still needed by an installed or selected ONNX model — remove those models first"
        );

        seed_onnx_set(&roots.model, ds_config::TtsModel::Kokoro);
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[]),
                &roots,
                Some("kokoro_frontend")
            )),
            "models: `kokoro_frontend` is still needed by Kokoro — remove or deselect `kokoro` first"
        );
    }

    #[test]
    fn an_unreferenced_shared_asset_is_reclaimed() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\nprovider = [\"cpu\"]\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        let dylib = seed_ort_dylib(&roots.model);
        assert_fixture_is_isolated(models.path(), &dylib);

        let after = payload(respond(
            &paths,
            &downloads(&[]),
            &roots,
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

    /// Shared tokens must not fall through to the unnamed refusal.
    #[test]
    fn every_shared_token_names_its_own_cause() {
        for id in ds_config::SHARED_ASSET_TOKENS {
            assert_ne!(shared_refusal(id), unnamed_shared_refusal(id), "{id}");
        }
    }

    /// CUDA remove: selection + driver; fixture keeps a different model selected.
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
            let roots = ModelRoots::under(models.path());
            assert_eq!(
                error_of(respond(&paths, &downloads(&[]), &roots, Some("cuda"))),
                "models: `cuda` is not available on this platform"
            );
            return;
        }

        let (_config, paths) = fixture(&cfg("cuda"));
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        assert_fixture_is_isolated(models.path(), &roots.model.join("cuda"));
        let response = respond(&paths, &downloads(&[]), &roots, Some("cuda"));
        if ds_model::cuda_driver_available() {
            assert_eq!(
                error_of(response),
                "models: `cuda` is the resolved compute provider — set_config provider without `cuda` first"
            );
        } else {
            // No driver → CPU EP; do not strand the runtime.
            assert_eq!(payload(response)["removed"]["id"], "cuda");
        }

        let (_config, paths) = fixture(&cfg("cpu"));
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        assert_fixture_is_isolated(models.path(), &roots.model.join("cuda"));
        let after = payload(respond(&paths, &downloads(&[]), &roots, Some("cuda")));
        assert_eq!(after["removed"]["id"], "cuda");
    }

    /// Shared ids refused while any download is in flight (ORT has no own target).
    #[test]
    fn a_shared_id_is_refused_while_any_download_is_in_flight() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\nprovider = [\"cpu\"]\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        assert_eq!(
            error_of(respond(
                &paths,
                &downloads(&[DownloadTarget::ChatterboxModel]),
                &roots,
                Some("onnxruntime")
            )),
            "models: `onnxruntime` is shared and a download is in flight — try again when it finishes"
        );
    }

    /// Unlisted id = CLI/engine skew (blame engine), not shared-asset rules.
    #[test]
    fn an_unlisted_id_names_the_engine_as_the_stale_side() {
        let (_config, paths) = fixture("tts_engine = []\nstt_engine = []\n");
        let models = tempfile::tempdir().unwrap();
        let roots = ModelRoots::under(models.path());
        let message = error_of(respond(
            &paths,
            &downloads(&[]),
            &roots,
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
        let roots = ModelRoots::under(models.path());
        let dir = roots.model.join("qwen3-tts");
        assert_fixture_is_isolated(models.path(), &dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("talker_cache.onnx"), b"weights on disk").unwrap();

        let before = payload(respond(&paths, &downloads(&[]), &roots, None));
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

        let after = payload(respond(&paths, &downloads(&[]), &roots, Some("qwen")));
        assert_eq!(
            after["removed"],
            serde_json::json!({ "id": "qwen", "bytes": 15 })
        );
        assert_eq!(row(&after)["installed"], false);
        assert_eq!(row(&after)["bytes"], 0);
        assert!(!dir.exists());
    }
}
