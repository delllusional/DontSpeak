//! On-disk inventory of the model cache: what each built-in model costs, and removal.
//!
//! Every entry is derived from the existing registries ([`crate::tts_assets`],
//! [`crate::mlx_repo`], [`crate::spec`], [`crate::ort`]) — there is no second taxonomy to
//! keep in sync. The whole API is parameterized on the model root; nothing here resolves
//! [`ds_config::model_dir`], so a test can never scan or delete the developer's real cache.
//! (`remove_at` still reads the ambient root INSIDE `with_destination_flight`, which picks
//! its sweep root there — flight-entering tests carry the `sweep_root_of` guard, #204.)
//!
//! `installed` is existence-based, matching the engine's cheap presence gate
//! (`tts_model_files_present`): the files are present. The engine additionally verifies
//! checksums when it loads, so a corrupt-but-present set reports installed here and fails
//! at load. Sizes are logical bytes (`symlink_metadata().len()`), never block usage, and
//! symlinks are not followed — the same walk shape as the orphan sweep.
//!
//! Diarization (`diarization_mlx`, `sepformer_model`) is deliberately unlisted while the
//! feature is hidden (#77); enabling it must add its row here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ds_config::{TtsModel, VoiceConfig};
use serde_json::{Value, json};

use crate::download::with_destination_flight;
use crate::target::DownloadTarget;

/// Row category. Wire: `tts` | `stt` | `frontend` | `runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Tts,
    Stt,
    Frontend,
    Runtime,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Tts => "tts",
            AssetKind::Stt => "stt",
            AssetKind::Frontend => "frontend",
            AssetKind::Runtime => "runtime",
        }
    }
}

/// One on-disk flavor of an asset, keyed by its existing [`DownloadTarget`] wire token.
#[derive(Debug, Clone)]
pub struct Variant {
    pub target: DownloadTarget,
    pub installed: bool,
    pub bytes: u64,
}

/// One inventory row: a model id and every variant of it this host can hold.
#[derive(Debug, Clone)]
pub struct Asset {
    pub id: &'static str,
    pub kind: AssetKind,
    pub model: Option<TtsModel>,
    /// Shared assets (ORT, the Kokoro text frontend, the CUDA runtime) are listed with
    /// their sizes but never removed — every model depends on them (#220).
    pub removable: bool,
    pub variants: Vec<Variant>,
}

impl Asset {
    /// Any variant present on disk.
    pub fn installed(&self) -> bool {
        self.variants.iter().any(|variant| variant.installed)
    }

    pub fn bytes(&self) -> u64 {
        self.variants
            .iter()
            .fold(0u64, |sum, variant| sum.saturating_add(variant.bytes))
    }
}

struct Row {
    id: &'static str,
    kind: AssetKind,
    model: Option<TtsModel>,
    removable: bool,
    targets: &'static [DownloadTarget],
}

/// Row order is the wire order: TTS models in `TTS_MODELS` order, then STT, then the
/// shared frontend and runtimes.
static ROWS: &[Row] = &[
    Row {
        id: "kokoro",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Kokoro),
        removable: true,
        targets: &[DownloadTarget::KokoroModel, DownloadTarget::KokoroMlx],
    },
    Row {
        id: "chatterbox",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Chatterbox),
        removable: true,
        targets: &[
            DownloadTarget::ChatterboxModel,
            DownloadTarget::ChatterboxMlx,
        ],
    },
    Row {
        id: "qwen",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Qwen),
        removable: true,
        targets: &[DownloadTarget::QwenModel, DownloadTarget::QwenMlx],
    },
    Row {
        id: "omnivoice",
        kind: AssetKind::Tts,
        model: Some(TtsModel::OmniVoice),
        removable: true,
        targets: &[DownloadTarget::OmniVoiceModel, DownloadTarget::OmniVoiceMlx],
    },
    Row {
        id: ds_config::STT_MODEL_TOKEN,
        kind: AssetKind::Stt,
        model: None,
        removable: true,
        targets: &[DownloadTarget::ParakeetModel, DownloadTarget::ParakeetMlx],
    },
    Row {
        id: "kokoro_frontend",
        kind: AssetKind::Frontend,
        model: None,
        removable: false,
        targets: &[DownloadTarget::KokoroFrontend],
    },
    Row {
        id: "onnxruntime",
        kind: AssetKind::Runtime,
        model: None,
        removable: false,
        targets: &[DownloadTarget::Onnxruntime],
    },
    Row {
        id: "cuda",
        kind: AssetKind::Runtime,
        model: None,
        removable: false,
        targets: &[DownloadTarget::Cuda],
    },
];

fn row(id: &str) -> Option<&'static Row> {
    ROWS.iter().find(|row| row.id == id)
}

/// The removable targets behind an id, or `None` when the id is unknown or shared.
/// Checked engine-side as well as by the tool schema — defence in depth across the IPC edge.
pub fn removal_targets(id: &str) -> Option<&'static [DownloadTarget]> {
    row(id).filter(|row| row.removable).map(|row| row.targets)
}

/// Does this build list `id` at all? Separates the two reasons [`removal_targets`] answers
/// `None`, so an id a NEWER CLI knows and this engine does not is reported as version skew
/// instead of as a shared asset.
pub fn is_known_asset(id: &str) -> bool {
    row(id).is_some()
}

/// A flat set's own ONNX weights: THAT model's files minus the shared text-frontend files, so
/// the G2P graphs stay with `kokoro_frontend` and are never deleted with the model. Derived
/// from the model passed in, never from Kokoro — a second `dir_name: None` set must resolve to
/// its own weights, not to Kokoro's.
fn flat_weight_files(model: TtsModel) -> Vec<&'static str> {
    let frontend: HashSet<String> = crate::spec::kokoro_frontend_files()
        .into_iter()
        .map(|file| file.file_name)
        .collect();
    crate::tts_assets::tts_ort_asset_set(model)
        .files
        .iter()
        .map(|file| file.file_name)
        .filter(|name| !frontend.contains(*name))
        .collect()
}

const PARAKEET_ONNX_FILES: [&str; 4] = [
    crate::spec::PARAKEET_ENCODER_FILE,
    crate::spec::PARAKEET_DECODER_FILE,
    crate::spec::PARAKEET_JOINER_FILE,
    crate::spec::PARAKEET_TOKENS_FILE,
];

const KOKORO_G2P_FILES: [&str; 2] = [
    crate::spec::KOKORO_G2P_ENCODER_FILE,
    crate::spec::KOKORO_G2P_DECODER_FILE,
];

fn mlx_dirs_under(root: &Path, repos: &[&'static crate::mlx_repo::MlxRepo]) -> Vec<PathBuf> {
    repos
        .iter()
        .map(|repo| crate::mlx_repo::repo_dir_under(root, repo))
        .collect()
}

/// Everything one target owns under `root`. Files and directories both appear; a directory
/// entry is removed whole.
pub fn owned_paths_under(root: &Path, target: DownloadTarget) -> Vec<PathBuf> {
    let onnx_tts = |model: TtsModel| -> Vec<PathBuf> {
        match crate::tts_assets::tts_ort_asset_set(model).dir_name {
            Some(_) => vec![crate::tts_assets::tts_model_dir_under(root, model)],
            None => flat_weight_files(model)
                .into_iter()
                .map(|name| root.join(name))
                .collect(),
        }
    };
    match target {
        DownloadTarget::KokoroModel => onnx_tts(TtsModel::Kokoro),
        DownloadTarget::ChatterboxModel => onnx_tts(TtsModel::Chatterbox),
        DownloadTarget::QwenModel => onnx_tts(TtsModel::Qwen),
        DownloadTarget::OmniVoiceModel => onnx_tts(TtsModel::OmniVoice),
        DownloadTarget::KokoroMlx
        | DownloadTarget::ChatterboxMlx
        | DownloadTarget::QwenMlx
        | DownloadTarget::OmniVoiceMlx => mlx_dirs_under(
            root,
            crate::mlx_repo::tts_mlx_set(target.tts_model().expect("mlx tts target has a model")),
        ),
        DownloadTarget::ParakeetModel => PARAKEET_ONNX_FILES
            .iter()
            .map(|name| root.join(name))
            .collect(),
        DownloadTarget::ParakeetMlx => mlx_dirs_under(root, &crate::mlx_repo::PARAKEET_MLX_SET),
        DownloadTarget::KokoroFrontend => {
            let mut paths: Vec<PathBuf> = KOKORO_G2P_FILES
                .iter()
                .map(|name| root.join(name))
                .collect();
            paths.push(crate::kokoro_frontend::espeak_dir_under(root));
            paths
        }
        DownloadTarget::Onnxruntime => crate::ort::onnxruntime_paths_under(root),
        DownloadTarget::Cuda => vec![crate::ort::cuda_runtime_dir_under(root)],
        // Unlisted: diarization is hidden (#77), and `Models` is an installer group.
        DownloadTarget::DiarizationMlx
        | DownloadTarget::SepformerModel
        | DownloadTarget::Models => Vec::new(),
    }
}

/// Recursive logical size. Missing ⇒ 0; symlinks counted as themselves, never followed.
pub fn dir_size_at(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .fold(0u64, |sum, e| sum.saturating_add(dir_size_at(&e.path())))
}

fn mlx_set_installed(root: &Path, repos: &[&'static crate::mlx_repo::MlxRepo]) -> bool {
    repos.iter().all(|repo| {
        let dir = crate::mlx_repo::repo_dir_under(root, repo);
        crate::mlx_repo::ready_marker_matches(&dir, repo)
            && repo.files.iter().all(|file| dir.join(file.path).is_file())
    })
}

fn onnx_tts_installed(root: &Path, model: TtsModel) -> bool {
    let dir = crate::tts_assets::tts_model_dir_under(root, model);
    crate::tts_assets::tts_ort_asset_set(model)
        .files_for(false)
        .all(|file| dir.join(file.file_name).is_file())
}

fn variant_installed(root: &Path, target: DownloadTarget) -> bool {
    match target {
        DownloadTarget::KokoroModel => onnx_tts_installed(root, TtsModel::Kokoro),
        DownloadTarget::ChatterboxModel => onnx_tts_installed(root, TtsModel::Chatterbox),
        DownloadTarget::QwenModel => onnx_tts_installed(root, TtsModel::Qwen),
        DownloadTarget::OmniVoiceModel => onnx_tts_installed(root, TtsModel::OmniVoice),
        DownloadTarget::KokoroMlx
        | DownloadTarget::ChatterboxMlx
        | DownloadTarget::QwenMlx
        | DownloadTarget::OmniVoiceMlx => mlx_set_installed(
            root,
            crate::mlx_repo::tts_mlx_set(target.tts_model().expect("mlx tts target has a model")),
        ),
        DownloadTarget::ParakeetModel => PARAKEET_ONNX_FILES
            .iter()
            .all(|name| root.join(name).is_file()),
        DownloadTarget::ParakeetMlx => mlx_set_installed(root, &crate::mlx_repo::PARAKEET_MLX_SET),
        DownloadTarget::KokoroFrontend => {
            KOKORO_G2P_FILES
                .iter()
                .all(|name| root.join(name).is_file())
                && crate::kokoro_frontend::espeak_dir_under(root)
                    .join(crate::kokoro_frontend::COMPLETE_MARKER)
                    .is_file()
        }
        DownloadTarget::Onnxruntime => root.join(crate::ort::onnxruntime_dylib_file()).is_file(),
        DownloadTarget::Cuda => crate::ort::cuda_runtime_dir_under(root).is_dir(),
        DownloadTarget::DiarizationMlx
        | DownloadTarget::SepformerModel
        | DownloadTarget::Models => false,
    }
}

fn variant_bytes(root: &Path, target: DownloadTarget) -> u64 {
    owned_paths_under(root, target)
        .iter()
        .fold(0u64, |sum, path| sum.saturating_add(dir_size_at(path)))
}

/// Read-only walk of `root`. Creates nothing — not even `root` itself.
pub fn scan_at(root: &Path) -> Vec<Asset> {
    ROWS.iter()
        .filter_map(|row| {
            let variants: Vec<Variant> = row
                .targets
                .iter()
                .copied()
                .filter(|target| target.is_supported_on_this_host())
                .map(|target| Variant {
                    target,
                    installed: variant_installed(root, target),
                    bytes: variant_bytes(root, target),
                })
                .collect();
            if variants.is_empty() {
                return None;
            }
            Some(Asset {
                id: row.id,
                kind: row.kind,
                model: row.model,
                removable: row.removable,
                variants,
            })
        })
        .collect()
}

/// Is this id the model the engine would immediately re-fetch? Coarse per model (both
/// variants): the live variant depends on the app-hosted MLX shim, which this process
/// cannot see, so refusing the whole model is the predictable rule.
pub fn asset_in_use(cfg: &VoiceConfig, id: &str) -> bool {
    match row(id) {
        Some(row) if row.kind == AssetKind::Stt => {
            cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
        }
        Some(row) => row.model.is_some_and(|model| {
            cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn) && cfg.tts_model == model
        }),
        None => false,
    }
}

/// Delete every path `id` owns under `root`, returning the reclaimed bytes.
///
/// Idempotent in outcome, not side-effect-free: each path is deleted inside its own
/// destination flight, and entering a flight materializes the destination's parent, the
/// sweep root, `.orphan-sweep.gate`, and a `.{name}.lock` sidecar — the same footprint a
/// download attempt leaves. Partial failure is surfaced, never repaired: on an `io::Error`
/// the asset stays half-deleted and re-running this is the recovery (removal operates on
/// paths present, not on `installed`).
pub fn remove_at(root: &Path, id: &str) -> std::io::Result<u64> {
    let targets = removal_targets(id).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("`{id}` is not a removable model"),
        )
    })?;
    let mut paths: Vec<PathBuf> = targets
        .iter()
        .flat_map(|target| owned_paths_under(root, *target))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut reclaimed: u64 = 0;
    for path in &paths {
        let bytes = with_destination_flight(path, |_| remove_locked(path))?;
        reclaimed = reclaimed.saturating_add(bytes);
    }
    log::info!(
        target: "model",
        "removed model `{id}`: reclaimed {reclaimed} bytes under {}",
        root.display()
    );
    Ok(reclaimed)
}

/// Measure then delete, inside the flight, so the reported bytes are the bytes that left.
fn remove_locked(path: &Path) -> std::io::Result<u64> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let bytes = dir_size_at(path);
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(bytes)
}

fn tts_param_json(param: &ds_config::TtsParamDescriptor) -> Value {
    let mut out = json!({ "key": param.key, "visible": param.user_visible });
    match param.kind {
        ds_config::TtsParamKind::Float { min, max } => {
            out["kind"] = json!("float");
            out["min"] = json!(min);
            out["max"] = json!(max);
        }
        ds_config::TtsParamKind::Int { min, max } => {
            out["kind"] = json!("int");
            out["min"] = json!(min);
            out["max"] = json!(max);
        }
        ds_config::TtsParamKind::Choice(choices) => {
            out["kind"] = json!("choice");
            out["choices"] = json!(choices);
        }
    }
    out["default"] = match param.default {
        ds_config::TtsParamDefault::Float(value) => json!(value),
        ds_config::TtsParamDefault::Int(value) => json!(value),
        ds_config::TtsParamDefault::Choice(value) => json!(value),
    };
    out
}

fn capabilities_json(model: TtsModel) -> Value {
    let descriptor = model.descriptor();
    json!({
        "name": descriptor.display_name,
        "default_language": descriptor.default_language,
        "languages": descriptor.languages,
        "providers": descriptor.providers.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "supports_rate": descriptor.supports_rate,
        "supports_full_duplex": descriptor.supports_full_duplex,
        "params": descriptor.config_params.iter().map(tts_param_json).collect::<Vec<_>>(),
    })
}

/// The `models` tool payload. `removed` is present only after a successful removal.
///
/// `removable` answers "can this be removed right now": a shared asset never can, the
/// selected model cannot, and neither can one whose download is in flight. `reason` names
/// only the durable cause (`active` / `shared`) — live download state belongs to `status`.
pub fn inventory_json(
    root: &Path,
    cfg: &VoiceConfig,
    active_downloads: &[DownloadTarget],
    removed: Option<(&str, u64)>,
) -> Value {
    let assets: Vec<Value> = scan_at(root)
        .into_iter()
        .map(|asset| {
            let active = asset.removable && asset_in_use(cfg, asset.id);
            let downloading = asset
                .variants
                .iter()
                .any(|variant| active_downloads.contains(&variant.target));
            let reason = if active {
                Some("active")
            } else if !asset.removable {
                Some("shared")
            } else {
                None
            };
            json!({
                "id": asset.id,
                "kind": asset.kind.as_str(),
                "installed": asset.installed(),
                "bytes": asset.bytes(),
                "active": active,
                "removable": asset.removable && !active && !downloading,
                "reason": reason,
                "variants": asset.variants.iter().map(|variant| json!({
                    "id": variant.target.as_str(),
                    "installed": variant.installed,
                    "bytes": variant.bytes,
                })).collect::<Vec<_>>(),
                "capabilities": asset.model.map(capabilities_json),
            })
        })
        .collect();
    let mut out = json!({
        "model_dir": root.display().to_string(),
        "total_bytes": dir_size_at(root),
        "assets": assets,
    });
    if let Some((id, bytes)) = removed {
        out["removed"] = json!({ "id": id, "bytes": bytes });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::sweep_root_of;

    /// Guards a fixture against a `DONTSPEAK_MODEL_DIR` that covers `TMPDIR`: the flight
    /// would then take its sweep gate in the REAL model dir (#204).
    fn assert_fixture_is_isolated(root: &Path, path: &Path) {
        assert!(
            sweep_root_of(path).is_some_and(|resolved| resolved.starts_with(root)),
            "the fixture must not sit under DONTSPEAK_MODEL_DIR (#204)"
        );
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|read| {
                read.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn owned_paths_are_root_relative_and_split_the_frontend_from_kokoro() {
        let root = Path::new("/models");
        assert_eq!(
            flat_weight_files(TtsModel::Kokoro),
            vec![
                crate::spec::KOKORO_ONNX_FILE,
                crate::spec::KOKORO_VOICES_FILE
            ]
        );
        assert_eq!(
            owned_paths_under(root, DownloadTarget::KokoroModel),
            vec![
                root.join(crate::spec::KOKORO_ONNX_FILE),
                root.join(crate::spec::KOKORO_VOICES_FILE),
            ]
        );
        assert_eq!(
            owned_paths_under(root, DownloadTarget::ChatterboxModel),
            vec![root.join("chatterbox-multilingual")]
        );
        assert_eq!(
            owned_paths_under(root, DownloadTarget::ChatterboxMlx),
            vec![
                root.join("mlx")
                    .join(crate::mlx_repo::CHATTERBOX_MLX_DIR_NAME),
                root.join("mlx")
                    .join(crate::mlx_repo::CHATTERBOX_S3_MLX_DIR_NAME),
            ]
        );
        assert_eq!(
            owned_paths_under(root, DownloadTarget::ParakeetMlx),
            vec![
                root.join("mlx")
                    .join(crate::mlx_repo::PARAKEET_MLX_DIR_NAME)
            ]
        );
        assert_eq!(
            owned_paths_under(root, DownloadTarget::ParakeetModel),
            vec![
                root.join(crate::spec::PARAKEET_ENCODER_FILE),
                root.join(crate::spec::PARAKEET_DECODER_FILE),
                root.join(crate::spec::PARAKEET_JOINER_FILE),
                root.join(crate::spec::PARAKEET_TOKENS_FILE),
            ]
        );

        // The G2P graphs and the espeak runtime belong to the shared frontend row only.
        let frontend = owned_paths_under(root, DownloadTarget::KokoroFrontend);
        assert!(frontend.contains(&root.join(crate::spec::KOKORO_G2P_ENCODER_FILE)));
        assert!(frontend.contains(&root.join(crate::spec::KOKORO_G2P_DECODER_FILE)));
        assert!(frontend.contains(&crate::kokoro_frontend::espeak_dir_under(root)));
        let kokoro = owned_paths_under(root, DownloadTarget::KokoroModel);
        for path in &frontend {
            assert!(!kokoro.contains(path), "{path:?} must not be a kokoro path");
        }
        assert!(
            owned_paths_under(root, DownloadTarget::Onnxruntime)
                .contains(&root.join(crate::ort::onnxruntime_dylib_file()))
        );
        assert!(owned_paths_under(root, DownloadTarget::DiarizationMlx).is_empty());
    }

    /// A second flat (`dir_name: None`) set must own ITS files. Kokoro is the only flat set
    /// today, so this calls the helper for a subdirectory model to pin that the derivation
    /// follows the argument — a Kokoro-hardcoded helper would make the new model's removal
    /// delete `kokoro-v1.0-fp32.onnx` and `voices-v1.0.bin`.
    #[test]
    fn flat_weights_are_derived_from_the_model_passed_in() {
        let qwen: Vec<&'static str> = crate::tts_assets::tts_ort_asset_set(TtsModel::Qwen)
            .files
            .iter()
            .map(|file| file.file_name)
            .collect();
        assert_eq!(flat_weight_files(TtsModel::Qwen), qwen);
        assert_ne!(
            flat_weight_files(TtsModel::Qwen),
            flat_weight_files(TtsModel::Kokoro)
        );
    }

    #[test]
    fn dir_size_sums_nested_files_and_ignores_what_is_missing() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(dir_size_at(&root.path().join("absent")), 0);
        write(&root.path().join("a.bin"), b"1234");
        write(&root.path().join("deep/b.bin"), b"567");
        assert_eq!(dir_size_at(&root.path().join("a.bin")), 4);
        assert_eq!(dir_size_at(root.path()), 7);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_never_follows_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("real");
        write(&target.join("big.bin"), &[0u8; 4096]);
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(dir_size_at(&link), 0, "a symlink is never walked");
        assert_eq!(dir_size_at(root.path()), 4096);
    }

    /// Seeds one directory asset, one flat asset, and one MLX asset — the three shapes.
    fn seed(root: &Path) {
        let chatterbox = crate::tts_assets::tts_model_dir_under(root, TtsModel::Chatterbox);
        for file in crate::tts_assets::tts_ort_asset_set(TtsModel::Chatterbox).files_for(false) {
            write(&chatterbox.join(file.file_name), b"chatterbox");
        }
        for name in PARAKEET_ONNX_FILES {
            write(&root.join(name), b"parakeet");
        }
        let kokoro_mlx = crate::mlx_repo::repo_dir_under(root, &crate::mlx_repo::KOKORO_MLX);
        for file in crate::mlx_repo::KOKORO_MLX.files {
            write(&kokoro_mlx.join(file.path), b"k");
        }
        write(
            &kokoro_mlx.join(".ds-ready"),
            crate::mlx_repo::KOKORO_MLX.revision.as_bytes(),
        );
    }

    fn asset<'a>(assets: &'a [Asset], id: &str) -> &'a Asset {
        assets
            .iter()
            .find(|asset| asset.id == id)
            .unwrap_or_else(|| panic!("row {id}"))
    }

    fn variant(asset: &Asset, target: DownloadTarget) -> Option<&Variant> {
        asset.variants.iter().find(|v| v.target == target)
    }

    #[test]
    fn scan_reports_presence_and_bytes_without_creating_anything() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        let before = entries(root.path());
        let assets = scan_at(root.path());
        assert_eq!(entries(root.path()), before, "scan_at must create nothing");

        let chatterbox = asset(&assets, "chatterbox");
        let onnx = variant(chatterbox, DownloadTarget::ChatterboxModel).unwrap();
        assert!(onnx.installed);
        assert!(onnx.bytes > 0);
        let parakeet = asset(&assets, "parakeet");
        let parakeet_onnx = variant(parakeet, DownloadTarget::ParakeetModel).unwrap();
        assert!(parakeet_onnx.installed);
        assert_eq!(parakeet_onnx.bytes, 8 * PARAKEET_ONNX_FILES.len() as u64);

        // Rows/variants are host-gated; MLX only exists on Apple Silicon.
        let kokoro = asset(&assets, "kokoro");
        match variant(kokoro, DownloadTarget::KokoroMlx) {
            Some(mlx) => {
                assert!(
                    mlx.installed,
                    "a seeded set at the pinned revision is ready"
                );
                // A stale marker invalidates the whole set without touching the files.
                let dir =
                    crate::mlx_repo::repo_dir_under(root.path(), &crate::mlx_repo::KOKORO_MLX);
                std::fs::write(dir.join(".ds-ready"), "0000000").unwrap();
                let stale = scan_at(root.path());
                assert!(
                    !variant(asset(&stale, "kokoro"), DownloadTarget::KokoroMlx)
                        .unwrap()
                        .installed
                );
            }
            None => assert!(!DownloadTarget::KokoroMlx.is_supported_on_this_host()),
        }
        assert!(
            !assets.iter().any(|asset| asset.id == "diarization"),
            "diarization stays unlisted while the feature is hidden (#77)"
        );
    }

    #[test]
    fn a_half_seeded_set_is_not_installed_but_still_reports_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let dir = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Qwen);
        let first = crate::tts_assets::tts_ort_asset_set(TtsModel::Qwen).files[0];
        write(&dir.join(first.file_name), b"partial download");
        let assets = scan_at(root.path());
        let qwen = variant(asset(&assets, "qwen"), DownloadTarget::QwenModel).unwrap();
        assert!(!qwen.installed);
        assert_eq!(qwen.bytes, 16);
    }

    #[test]
    fn scan_of_a_missing_root_is_all_zero_and_leaves_it_missing() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("never-created");
        let assets = scan_at(&root);
        assert!(!assets.is_empty());
        for asset in &assets {
            assert!(!asset.installed(), "{}", asset.id);
            assert_eq!(asset.bytes(), 0, "{}", asset.id);
        }
        assert!(!root.exists(), "scan_at must not create the model root");
    }

    #[test]
    fn removal_targets_cover_every_removable_token_and_nothing_else() {
        for id in ds_config::MODEL_ASSET_TOKENS {
            let targets = removal_targets(id).unwrap_or_else(|| panic!("{id} is removable"));
            assert!(!targets.is_empty(), "{id}");
        }
        for id in ["onnxruntime", "cuda", "kokoro_frontend", "sepformer", ""] {
            assert!(removal_targets(id).is_none(), "{id} must not be removable");
        }
        // Every row id is either a removable token or a shared asset — no third state.
        for row in ROWS {
            assert_eq!(
                row.removable,
                ds_config::MODEL_ASSET_TOKENS.contains(&row.id),
                "{}",
                row.id
            );
        }
    }

    #[test]
    fn remove_deletes_only_what_the_asset_owns() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        let chatterbox = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        let qwen = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Qwen);
        write(&qwen.join("keep.onnx"), b"another model");
        let dylib = root.path().join(crate::ort::onnxruntime_dylib_file());
        write(&dylib, b"shared runtime");
        let expected = dir_size_at(&chatterbox);

        assert_eq!(remove_at(root.path(), "chatterbox").unwrap(), expected);
        assert!(!chatterbox.exists());
        assert!(qwen.join("keep.onnx").is_file(), "a sibling model survives");
        assert!(dylib.is_file(), "the shared runtime is never removed");
        assert!(
            root.path().is_dir(),
            "the model root itself is never removed"
        );
        assert!(
            root.path()
                .join(crate::spec::PARAKEET_ENCODER_FILE)
                .is_file(),
            "a flat sibling asset survives"
        );
    }

    /// #4.5: removing a never-downloaded model is a 0-byte success whose only footprint is
    /// what entering a flight creates. Pins that a lock change cannot start materializing
    /// model directories.
    #[test]
    fn removing_an_absent_model_reclaims_nothing_and_creates_only_lock_scaffolding() {
        let root = tempfile::tempdir().unwrap();
        let chatterbox = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        assert_eq!(remove_at(root.path(), "chatterbox").unwrap(), 0);

        assert!(!chatterbox.exists(), "no model directory is materialized");
        let mut expected = vec![
            ".chatterbox-multilingual.lock".to_string(),
            ".orphan-sweep.gate".to_string(),
            "mlx".to_string(),
        ];
        expected.sort();
        assert_eq!(entries(root.path()), expected);
        let mlx = ds_config::mlx_dir_under(root.path());
        // CHATTERBOX_MLX_DIR_NAME nests, so its flight also creates the `mlx-audio` parent.
        assert_eq!(entries(&mlx), vec!["mlx-audio".to_string()]);
        // A fixture outside the real model root resolves its sweep root to the lock's own
        // parent; in production every gate collapses onto `<model_dir>/.orphan-sweep.gate`.
        assert_eq!(
            entries(&mlx.join("mlx-audio")),
            vec![
                ".mlx-community_S3TokenizerV2.lock".to_string(),
                ".mlx-community_chatterbox-8bit.lock".to_string(),
                ".orphan-sweep.gate".to_string(),
            ]
        );
    }

    #[test]
    fn remove_rejects_an_id_it_does_not_own() {
        let root = tempfile::tempdir().unwrap();
        for id in ["onnxruntime", "kokoro_frontend", "bogus"] {
            let err = remove_at(root.path(), id).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{id}");
        }
        assert!(!root.path().join("mlx").exists(), "a refusal locks nothing");
    }

    /// The invariant this preserves: you can never remove something the engine
    /// (`dontspeakd::downloads::compute_needs`) would immediately re-fetch.
    #[test]
    fn in_use_follows_the_resolved_engine_ladders() {
        let built_in = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            stt_engine: Some(vec![ds_config::SttEngine::BuiltIn]),
            tts_model: TtsModel::Qwen,
            ..VoiceConfig::default()
        };
        assert!(asset_in_use(&built_in, "qwen"));
        assert!(!asset_in_use(&built_in, "kokoro"));
        assert!(asset_in_use(&built_in, "parakeet"));
        assert!(!asset_in_use(&built_in, "onnxruntime"));
        assert!(!asset_in_use(&built_in, "bogus"));

        let system_tts = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::System]),
            tts_model: TtsModel::Qwen,
            ..built_in.clone()
        };
        assert!(
            !asset_in_use(&system_tts, "qwen"),
            "a set tts_model is not in use while the engine is System"
        );

        let stt_off = VoiceConfig {
            stt_engine: Some(Vec::new()),
            ..built_in.clone()
        };
        assert_eq!(stt_off.resolved_stt(), None);
        assert!(!asset_in_use(&stt_off, "parakeet"));
    }

    #[test]
    fn payload_marks_the_active_model_and_the_shared_assets() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        // STT off explicitly: the default ladder resolves to `system` on macOS but falls
        // through to `built_in` on Linux/Windows, which would make `parakeet` the active
        // STT and flip the `removable` row below by host.
        let cfg = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: TtsModel::Chatterbox,
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        };
        let payload = inventory_json(root.path(), &cfg, &[DownloadTarget::QwenModel], None);
        assert_eq!(payload["model_dir"], root.path().display().to_string());
        assert!(payload["total_bytes"].as_u64().unwrap() > 0);
        assert!(payload.get("removed").is_none());

        let assets = payload["assets"].as_array().unwrap();
        let by_id = |id: &str| {
            assets
                .iter()
                .find(|asset| asset["id"] == id)
                .unwrap_or_else(|| panic!("row {id}"))
        };
        let chatterbox = by_id("chatterbox");
        assert_eq!(chatterbox["kind"], "tts");
        assert_eq!(chatterbox["active"], true);
        assert_eq!(chatterbox["removable"], false);
        assert_eq!(chatterbox["reason"], "active");
        assert_eq!(chatterbox["installed"], true);
        assert!(chatterbox["capabilities"]["languages"].is_array());

        let qwen = by_id("qwen");
        assert_eq!(qwen["active"], false);
        assert_eq!(
            qwen["removable"], false,
            "an in-flight download blocks removal"
        );
        assert_eq!(qwen["reason"], Value::Null);

        let parakeet = by_id("parakeet");
        assert_eq!(parakeet["kind"], "stt");
        assert_eq!(parakeet["removable"], true);
        assert_eq!(parakeet["capabilities"], Value::Null);

        for id in ["kokoro_frontend", "onnxruntime"] {
            let shared = by_id(id);
            assert_eq!(shared["removable"], false, "{id}");
            assert_eq!(shared["reason"], "shared", "{id}");
            assert_eq!(shared["active"], false, "{id}");
        }
        // Deterministic order: TTS models, then STT, then the shared rows.
        let ids: Vec<&str> = assets
            .iter()
            .map(|asset| asset["id"].as_str().unwrap())
            .collect();
        let expected_prefix: Vec<&str> = ds_config::MODEL_ASSET_TOKENS.to_vec();
        assert_eq!(&ids[..expected_prefix.len()], expected_prefix.as_slice());
    }

    #[test]
    fn a_removal_payload_reports_the_reclaimed_bytes() {
        let root = tempfile::tempdir().unwrap();
        seed(root.path());
        let chatterbox = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        let bytes = remove_at(root.path(), "chatterbox").unwrap();
        assert!(bytes > 0);
        let payload = inventory_json(
            root.path(),
            &VoiceConfig::default(),
            &[],
            Some(("chatterbox", bytes)),
        );
        assert_eq!(
            payload["removed"],
            json!({ "id": "chatterbox", "bytes": bytes })
        );
        let row = payload["assets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|asset| asset["id"] == "chatterbox")
            .unwrap()
            .clone();
        assert_eq!(row["installed"], false);
        assert_eq!(row["bytes"], 0);
    }

    /// #208 class: a cross-process installer holding the asset's directory flight must block
    /// the removal until it releases.
    #[test]
    fn removal_waits_for_a_cross_process_directory_installer() {
        const CHILD_TARGET: &str = "DS_MODEL_INVENTORY_CHILD_TARGET";
        const CHILD_READY: &str = "DS_MODEL_INVENTORY_CHILD_READY";
        const CHILD_RELEASE: &str = "DS_MODEL_INVENTORY_CHILD_RELEASE";

        let root = tempfile::tempdir().unwrap();
        let target = crate::tts_assets::tts_model_dir_under(root.path(), TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &target);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("weights.onnx"), b"installed bytes").unwrap();
        let ready = root.path().join("child-ready");
        let release = root.path().join("release-child");
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("inventory::tests::inventory_directory_lock_child")
            .arg("--nocapture")
            .env(CHILD_TARGET, &target)
            .env(CHILD_READY, &ready)
            .env(CHILD_RELEASE, &release)
            // The child must resolve the SAME sweep root as the parent (#204).
            .env_remove("DONTSPEAK_MODEL_DIR")
            .spawn()
            .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "child did not acquire the directory flight"
            );
            assert!(child.try_wait().unwrap().is_none(), "child exited early");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let removal_root = root.path().to_path_buf();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            let bytes = remove_at(&removal_root, "chatterbox").unwrap();
            done_tx.send(bytes).unwrap();
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "removal must not delete while another process owns the destination"
        );

        std::fs::write(&release, b"go").unwrap();
        assert!(child.wait().unwrap().success());
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the removal completes once the installer releases");
        remover.join().unwrap();
        assert!(!target.exists());
    }

    /// Child half of `removal_waits_for_a_cross_process_directory_installer`; inert unless
    /// the parent sets its environment.
    #[test]
    fn inventory_directory_lock_child() {
        let Some(target) = std::env::var_os("DS_MODEL_INVENTORY_CHILD_TARGET") else {
            return;
        };
        let target = PathBuf::from(target);
        let ready = PathBuf::from(std::env::var_os("DS_MODEL_INVENTORY_CHILD_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("DS_MODEL_INVENTORY_CHILD_RELEASE").unwrap());
        with_destination_flight(&target, |_| {
            std::fs::write(&ready, b"locked")?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !release.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent did not release the installer"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(())
        })
        .unwrap();
    }

    /// An install in flight for one of the asset's own files must finish before the removal
    /// deletes it — driven through the file-level httpmock seam, so no thread-local target
    /// seam and no ambient destination are involved.
    #[test]
    fn removal_does_not_interleave_with_an_in_flight_install() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const BODY: &[u8] = b"a freshly downloaded kokoro graph";
        let root = tempfile::tempdir().unwrap();
        let onnx = root.path().join(crate::spec::KOKORO_ONNX_FILE);
        assert_fixture_is_isolated(root.path(), &onnx);
        let voices = root.path().join(crate::spec::KOKORO_VOICES_FILE);
        std::fs::write(&voices, b"already installed voices").unwrap();

        let server = httpmock::MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(httpmock::Method::GET).path("/kokoro.onnx");
            then.status(200).body(BODY);
        });
        let spec = crate::spec::ModelSpec {
            file_name: crate::spec::KOKORO_ONNX_FILE.to_string(),
            url: server.url("/kokoro.onnx"),
            sha256: crate::hash::sha256_hex(BODY),
        };

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let install_root = root.path().to_path_buf();
        let installer = std::thread::spawn(move || {
            // `download_to_network` reports progress at least twice for a small body; only
            // the first call is the rendezvous, inside the flight and before the rename.
            let announced = AtomicBool::new(false);
            let progress = |_done: u64, _total: u64| {
                if !announced.swap(true, Ordering::SeqCst) {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect("main thread releases the install");
                }
            };
            crate::download::ensure_in_dir(&install_root, &spec, &progress).unwrap();
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the installer enters the flight");

        let removal_root = root.path().to_path_buf();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            removed_tx
                .send(remove_at(&removal_root, "kokoro").unwrap())
                .unwrap();
        });
        assert!(
            removed_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "removal must not interleave with an in-flight install"
        );

        release_tx.send(()).unwrap();
        installer.join().unwrap();
        let reclaimed = removed_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the removal completes once the install finishes");
        remover.join().unwrap();

        // The install's atomic rename lands BEFORE the delete, so the finalized file is gone.
        assert!(!onnx.exists(), "the removal deletes the finalized download");
        assert!(!voices.exists());
        assert!(reclaimed >= b"already installed voices".len() as u64);
        assert!(!root.path().join(".kokoro-v1.0-fp32.onnx.part").exists());
        assert!(
            !root
                .path()
                .join(".kokoro-v1.0-fp32.onnx.part.meta")
                .exists()
        );
        mock.assert_calls(1);
    }
}
