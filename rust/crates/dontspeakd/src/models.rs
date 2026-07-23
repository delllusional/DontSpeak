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

/// Refuse a removal the engine would immediately undo, or that would break every model.
/// `None` ⇒ the removal may proceed.
fn refusal(cfg: &VoiceConfig, id: &str, active: &[DownloadTarget]) -> Option<String> {
    let Some(targets) = ds_model::removal_targets(id) else {
        // The tool schema already rejects both, so reaching here means the CLI and the engine
        // disagree about the model list — name that, instead of telling the user a model this
        // build has never heard of is "shared by every model".
        return Some(if ds_model::is_known_asset(id) {
            format!("models: `{id}` is shared by every model and cannot be removed")
        } else {
            format!("models: unknown model `{id}` — the running engine may be older than this CLI")
        });
    };
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
    if targets.iter().any(|target| active.contains(target)) {
        return Some(format!(
            "models: `{id}` is downloading right now — try again when it finishes"
        ));
    }
    None
}

/// List the on-disk inventory, optionally removing one model id first. A failed removal
/// answers with an error and no payload — never a success carrying a stale row.
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
        if let Some(message) = refusal(&cfg, id, &active) {
            return ds_ipc::Response::error(message);
        }
        match ds_model::remove_at(root, id) {
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
        let (_config, paths) = fixture("tts_engine = \"off\"\nstt_engine = \"off\"\n");
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
    fn shared_ids_are_refused_engine_side_too() {
        let (_config, paths) = fixture("tts_engine = \"off\"\nstt_engine = \"off\"\n");
        let models = tempfile::tempdir().unwrap();
        for id in ["onnxruntime", "kokoro_frontend", "cuda"] {
            let message = error_of(respond(&paths, &downloads(&[]), models.path(), Some(id)));
            assert_eq!(
                message,
                format!("models: `{id}` is shared by every model and cannot be removed")
            );
        }
    }

    /// An id this engine does not list is version skew, not a shared asset: a CLI updated
    /// ahead of the app-hosted engine advertises the new token in its schema, and the stale
    /// engine must point at itself rather than at a shared-asset rule that does not apply.
    #[test]
    fn an_unlisted_id_names_the_engine_as_the_stale_side() {
        let (_config, paths) = fixture("tts_engine = \"off\"\nstt_engine = \"off\"\n");
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
        let (_config, paths) = fixture("tts_engine = \"off\"\nstt_engine = \"off\"\n");
        let models = tempfile::tempdir().unwrap();
        let dir = models.path().join("qwen3-tts");
        // This is the one test here that enters a destination flight, which resolves its sweep
        // root from the ambient `DONTSPEAK_MODEL_DIR`: a value covering `TMPDIR` would put the
        // gate in the real cache (#204).
        assert!(
            ds_model::sweep_root_of(&dir)
                .is_some_and(|resolved| resolved.starts_with(models.path())),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );
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
