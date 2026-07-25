//! On-disk model-cache inventory: costs and removal.
//!
//! Rows derive from existing registries ([`crate::tts_assets`], [`crate::mlx_repo`],
//! [`crate::coreml_repo`], [`crate::spec`], [`crate::ort`]) — no second taxonomy. API takes
//! [`ModelRoots`] only (no ambient roots, even in `remove_at` flights) so tests never touch
//! real caches.
//!
//! `installed` = cheap presence (files + markers). Load-time checksums still gate the engine.
//! Sizes are logical (`symlink_metadata().len()`), no follow — same walk as the orphan sweep.
//!
//! Diarization rows stay unlisted while the feature is hidden (#77).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ds_config::{TtsModel, VoiceConfig};
use serde_json::{Value, json};

use crate::download::with_destination_flight_in;
use crate::hf_repo::{HfRepo, ModelRoots};
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

    /// Frontend/runtime shared across models; removable only when unreferenced
    /// ([`shared_asset_referenced`], #220).
    pub fn is_shared(self) -> bool {
        matches!(self, AssetKind::Frontend | AssetKind::Runtime)
    }
}

/// On-disk flavor, keyed by [`DownloadTarget`] wire token.
#[derive(Debug, Clone)]
pub struct Variant {
    pub target: DownloadTarget,
    pub installed: bool,
    pub bytes: u64,
}

/// Inventory row: asset id + host-supported variants.
#[derive(Debug, Clone)]
pub struct Asset {
    pub id: &'static str,
    pub kind: AssetKind,
    pub model: Option<TtsModel>,
    pub variants: Vec<Variant>,
}

impl Asset {
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
    targets: &'static [DownloadTarget],
}

/// Wire order: TTS models, STT, then shared frontend/runtimes.
static ROWS: &[Row] = &[
    Row {
        id: "kokoro",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Kokoro),
        targets: &[
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroMlx,
            DownloadTarget::KokoroFluid,
        ],
    },
    Row {
        id: "chatterbox",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Chatterbox),
        targets: &[
            DownloadTarget::ChatterboxModel,
            DownloadTarget::ChatterboxMlx,
        ],
    },
    Row {
        id: "qwen",
        kind: AssetKind::Tts,
        model: Some(TtsModel::Qwen),
        targets: &[DownloadTarget::QwenModel, DownloadTarget::QwenMlx],
    },
    Row {
        id: "omnivoice",
        kind: AssetKind::Tts,
        model: Some(TtsModel::OmniVoice),
        targets: &[DownloadTarget::OmniVoiceModel, DownloadTarget::OmniVoiceMlx],
    },
    Row {
        id: ds_config::STT_MODEL_TOKEN,
        kind: AssetKind::Stt,
        model: None,
        targets: &[
            DownloadTarget::ParakeetModel,
            DownloadTarget::ParakeetMlx,
            DownloadTarget::ParakeetFluid,
        ],
    },
    Row {
        id: ds_config::KOKORO_FRONTEND_ASSET_TOKEN,
        kind: AssetKind::Frontend,
        model: None,
        targets: &[DownloadTarget::KokoroFrontend],
    },
    Row {
        id: ds_config::ONNXRUNTIME_ASSET_TOKEN,
        kind: AssetKind::Runtime,
        model: None,
        targets: &[DownloadTarget::Onnxruntime],
    },
    Row {
        id: ds_config::CUDA_ASSET_TOKEN,
        kind: AssetKind::Runtime,
        model: None,
        targets: &[DownloadTarget::Cuda],
    },
];

fn row(id: &str) -> Option<&'static Row> {
    ROWS.iter().find(|row| row.id == id)
}

/// Targets for `id`, or `None` if unlisted. Removal gates are dynamic — see
/// [`shared_asset_referenced`] and engine-side `dontspeakd::models::refusal`.
pub fn asset_targets(id: &str) -> Option<&'static [DownloadTarget]> {
    row(id).map(|row| row.targets)
}

/// Flat ONNX weights for `model` minus shared frontend files (G2P stays on `kokoro_frontend`).
/// Derived from the argument, never hardcoded to Kokoro.
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

fn hf_dirs(roots: &ModelRoots, repos: &[&'static HfRepo]) -> Vec<PathBuf> {
    repos.iter().map(|repo| roots.dir_for(repo)).collect()
}

/// Paths one target owns under `roots` (files and whole dirs).
pub fn owned_paths_under(roots: &ModelRoots, target: DownloadTarget) -> Vec<PathBuf> {
    let root = roots.model.as_path();
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
        | DownloadTarget::OmniVoiceMlx => hf_dirs(
            roots,
            crate::mlx_repo::tts_mlx_set(target.tts_model().expect("mlx tts target has a model")),
        ),
        DownloadTarget::ParakeetModel => PARAKEET_ONNX_FILES
            .iter()
            .map(|name| root.join(name))
            .collect(),
        DownloadTarget::ParakeetMlx => hf_dirs(roots, &crate::mlx_repo::PARAKEET_MLX_SET),
        // Spans model + FluidAudio roots — removal reclaims both.
        DownloadTarget::KokoroFluid => hf_dirs(roots, &crate::coreml_repo::KOKORO_COREML_SET),
        DownloadTarget::ParakeetFluid => hf_dirs(roots, &crate::coreml_repo::PARAKEET_COREML_SET),
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
        // Diarization hidden (#77); `Models` is an installer group.
        DownloadTarget::DiarizationMlx
        | DownloadTarget::DiarizationFluid
        | DownloadTarget::SepformerModel
        | DownloadTarget::Models => Vec::new(),
    }
}

/// Recursive logical size. Missing ⇒ 0; symlinks never followed.
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

/// Cheap set presence (marker + files). Checksums: [`crate::hf_repo::is_hf_set_present`].
fn hf_set_installed(roots: &ModelRoots, repos: &[&'static HfRepo]) -> bool {
    repos.iter().all(|repo| {
        let dir = roots.dir_for(repo);
        crate::hf_repo::ready_marker_matches(&dir, repo)
            && repo.files.iter().all(|file| dir.join(file.path).is_file())
    })
}

fn onnx_tts_installed(root: &Path, model: TtsModel) -> bool {
    let dir = crate::tts_assets::tts_model_dir_under(root, model);
    crate::tts_assets::tts_ort_asset_set(model)
        .files_for(false)
        .all(|file| dir.join(file.file_name).is_file())
}

fn variant_installed(roots: &ModelRoots, target: DownloadTarget) -> bool {
    let root = roots.model.as_path();
    match target {
        DownloadTarget::KokoroModel => onnx_tts_installed(root, TtsModel::Kokoro),
        DownloadTarget::ChatterboxModel => onnx_tts_installed(root, TtsModel::Chatterbox),
        DownloadTarget::QwenModel => onnx_tts_installed(root, TtsModel::Qwen),
        DownloadTarget::OmniVoiceModel => onnx_tts_installed(root, TtsModel::OmniVoice),
        DownloadTarget::KokoroMlx
        | DownloadTarget::ChatterboxMlx
        | DownloadTarget::QwenMlx
        | DownloadTarget::OmniVoiceMlx => hf_set_installed(
            roots,
            crate::mlx_repo::tts_mlx_set(target.tts_model().expect("mlx tts target has a model")),
        ),
        DownloadTarget::ParakeetModel => PARAKEET_ONNX_FILES
            .iter()
            .all(|name| root.join(name).is_file()),
        DownloadTarget::ParakeetMlx => hf_set_installed(roots, &crate::mlx_repo::PARAKEET_MLX_SET),
        // Files only — voice pack is a usability gate, not inventory (#220).
        DownloadTarget::KokoroFluid => {
            hf_set_installed(roots, &crate::coreml_repo::KOKORO_COREML_SET)
        }
        DownloadTarget::ParakeetFluid => {
            hf_set_installed(roots, &crate::coreml_repo::PARAKEET_COREML_SET)
        }
        DownloadTarget::KokoroFrontend => {
            KOKORO_G2P_FILES
                .iter()
                .all(|name| root.join(name).is_file())
                && crate::kokoro_frontend::espeak_dir_under(root)
                    .join(crate::kokoro_frontend::COMPLETE_MARKER)
                    .is_file()
        }
        DownloadTarget::Onnxruntime => root.join(crate::ort::onnxruntime_dylib_file()).is_file(),
        DownloadTarget::Cuda => crate::ort::is_cuda_runtime_present_under(root),
        DownloadTarget::DiarizationMlx
        | DownloadTarget::DiarizationFluid
        | DownloadTarget::SepformerModel
        | DownloadTarget::Models => false,
    }
}

fn variant_bytes(roots: &ModelRoots, target: DownloadTarget) -> u64 {
    owned_paths_under(roots, target)
        .iter()
        .fold(0u64, |sum, path| sum.saturating_add(dir_size_at(path)))
}

/// Read-only walk of `roots` (creates nothing).
pub fn scan_at(roots: &ModelRoots) -> Vec<Asset> {
    ROWS.iter()
        .filter_map(|row| {
            let variants: Vec<Variant> = row
                .targets
                .iter()
                .copied()
                .filter(|target| target.is_supported_on_this_host())
                .map(|target| Variant {
                    target,
                    installed: variant_installed(roots, target),
                    bytes: variant_bytes(roots, target),
                })
                .collect();
            if variants.is_empty() {
                return None;
            }
            Some(Asset {
                id: row.id,
                kind: row.kind,
                model: row.model,
                variants,
            })
        })
        .collect()
}

/// Would the engine re-fetch this id? Coarse per model (both variants) — live flavor
/// depends on the app-hosted shim this process cannot see.
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

/// Targets that load the shared ORT dylib (ONNX sets + native Kokoro G2P). Not `Cuda`
/// (ships its own ORT under `cuda/`).
const ORT_CONSUMER_TARGETS: [DownloadTarget; 7] = [
    DownloadTarget::KokoroModel,
    DownloadTarget::ChatterboxModel,
    DownloadTarget::QwenModel,
    DownloadTarget::OmniVoiceModel,
    DownloadTarget::ParakeetModel,
    DownloadTarget::KokoroMlx,
    DownloadTarget::KokoroFluid,
];

fn any_installed(roots: &ModelRoots, targets: &[DownloadTarget]) -> bool {
    targets
        .iter()
        .copied()
        .filter(|target| target.is_supported_on_this_host())
        .any(|target| variant_installed(roots, target))
}

/// Speaker-lock would pull SepFormer (and thus ORT). Mirrors `compute_needs` selection,
/// not presence.
fn sepformer_selected(cfg: &VoiceConfig) -> bool {
    DownloadTarget::SepformerModel.is_supported_on_this_host()
        && cfg.speaker_lock
        && cfg.is_diarization_on()
}

/// Built-in engine resolves to CUDA? Shared spelling for boot prefetch, warm-child reload,
/// and [`shared_asset_referenced`]. Pure config; callers own the driver probe.
pub fn cuda_runtime_wanted(cfg: &VoiceConfig) -> bool {
    (cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
        && cfg.resolved_tts_provider() == ds_config::Provider::OrtCuda)
        || (cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
            && cfg.resolved_stt_provider() == ds_config::Provider::OrtCuda)
}

pub fn is_shared_asset(id: &str) -> bool {
    row(id).is_some_and(|row| row.kind.is_shared())
}

/// Shared asset still needed: something installed loads it, or selection would re-fetch.
/// Model rows and unknown ids → `false`. Undescribed shared row → referenced (fail closed).
pub fn shared_asset_referenced(roots: &ModelRoots, cfg: &VoiceConfig, id: &str) -> bool {
    let root = roots.model.as_path();
    let Some(row) = row(id) else { return false };
    if !row.kind.is_shared() {
        return false;
    }
    match row.targets {
        [DownloadTarget::KokoroFrontend] => {
            any_installed(
                roots,
                &[
                    DownloadTarget::KokoroModel,
                    DownloadTarget::KokoroMlx,
                    DownloadTarget::KokoroFluid,
                ],
            ) || (cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
                && cfg.tts_model == TtsModel::Kokoro)
        }
        [DownloadTarget::Onnxruntime] => {
            any_installed(roots, &ORT_CONSUMER_TARGETS)
                // No diarization row while hidden (#77) — probe the file directly.
                || root.join(crate::spec::SEPFORMER_FILE).is_file()
                || cfg.resolved_tts() == Some(ds_config::TtsEngine::BuiltIn)
                || cfg.resolved_stt() == Some(ds_config::SttEngine::BuiltIn)
                || sepformer_selected(cfg)
        }
        // Driverless host uses CPU EP and never prefetches CUDA — reclaimable there.
        [DownloadTarget::Cuda] => cuda_runtime_wanted(cfg) && crate::ort::cuda_driver_available(),
        // Fail closed; `every_shared_row_has_its_own_reference_arm` catches missing arms.
        _ => true,
    }
}

/// Delete every path `id` owns under `roots`; return reclaimed bytes.
///
/// Two local gates (listed id; shared ⇒ unreferenced) so IPC callers that skip engine
/// refusal cannot break an install. Active-model / host / in-flight stay engine-side.
///
/// Idempotent outcome, not side-effect-free: each path's flight materializes parent, sweep
/// root, gate, and lock sidecars (same footprint as a download). Partial failure surfaces;
/// re-run recovers (operates on paths present, not `installed`).
pub fn remove_at(roots: &ModelRoots, cfg: &VoiceConfig, id: &str) -> std::io::Result<u64> {
    let invalid = |message: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, message);
    let targets =
        asset_targets(id).ok_or_else(|| invalid(format!("`{id}` is not a removable asset")))?;
    if shared_asset_referenced(roots, cfg, id) {
        return Err(invalid(format!("`{id}` is still referenced")));
    }
    let mut paths: Vec<PathBuf> = targets
        .iter()
        .flat_map(|target| owned_paths_under(roots, *target))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    let mut reclaimed: u64 = 0;
    for path in &paths {
        // Skip absent paths outside our cache — do not materialize third-party roots.
        if !path.exists() && !path.starts_with(&roots.model) {
            continue;
        }
        let bytes = with_destination_flight_in(Some(roots), path, |_| remove_locked(path))?;
        reclaimed = reclaimed.saturating_add(bytes);
    }
    log::info!(
        target: "model",
        "removed asset `{id}`: reclaimed {reclaimed} bytes under {}",
        roots.model.display()
    );
    Ok(reclaimed)
}

/// Measure then delete inside the flight (reported = reclaimed).
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

/// `models` tool payload. `removed` only after a successful removal.
///
/// `removable`: not active, not shared-referenced, not downloading. `reason` is the durable
/// cause only (`active` / `shared`); live download state is `status`.
pub fn inventory_json(
    roots: &ModelRoots,
    cfg: &VoiceConfig,
    active_downloads: &[DownloadTarget],
    removed: Option<(&str, u64)>,
) -> Value {
    let assets: Vec<Value> = scan_at(roots)
        .into_iter()
        .map(|asset| {
            let shared = asset.kind.is_shared();
            let active = asset_in_use(cfg, asset.id);
            let referenced = shared && shared_asset_referenced(roots, cfg, asset.id);
            // Shared rows block while any fetch runs (coarse; over-block costs one retry).
            let downloading = if shared {
                !active_downloads.is_empty()
            } else {
                asset
                    .variants
                    .iter()
                    .any(|variant| active_downloads.contains(&variant.target))
            };
            let reason = if active {
                Some("active")
            } else if referenced {
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
                "removable": !active && !referenced && !downloading,
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
        "model_dir": roots.model.display().to_string(),
        "total_bytes": dir_size_at(&roots.model),
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

    fn roots_under(dir: &Path) -> ModelRoots {
        ModelRoots::under(dir)
    }

    /// Fail if ambient `DONTSPEAK_MODEL_DIR` would steal the flight gate (#204).
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
        let roots = roots_under(Path::new("/roots"));
        let root = roots.model.as_path();
        assert_eq!(
            flat_weight_files(TtsModel::Kokoro),
            vec![
                crate::spec::KOKORO_ONNX_FILE,
                crate::spec::KOKORO_VOICES_FILE
            ]
        );
        assert_eq!(
            owned_paths_under(&roots, DownloadTarget::KokoroModel),
            vec![
                root.join(crate::spec::KOKORO_ONNX_FILE),
                root.join(crate::spec::KOKORO_VOICES_FILE),
            ]
        );
        assert_eq!(
            owned_paths_under(&roots, DownloadTarget::ChatterboxModel),
            vec![root.join("chatterbox-multilingual")]
        );
        assert_eq!(
            owned_paths_under(&roots, DownloadTarget::ChatterboxMlx),
            vec![
                root.join("mlx")
                    .join(crate::mlx_repo::CHATTERBOX_MLX_DIR_NAME),
                root.join("mlx")
                    .join(crate::mlx_repo::CHATTERBOX_S3_MLX_DIR_NAME),
            ]
        );
        assert_eq!(
            owned_paths_under(&roots, DownloadTarget::ParakeetMlx),
            vec![
                root.join("mlx")
                    .join(crate::mlx_repo::PARAKEET_MLX_DIR_NAME)
            ]
        );
        assert_eq!(
            owned_paths_under(&roots, DownloadTarget::ParakeetModel),
            vec![
                root.join(crate::spec::PARAKEET_ENCODER_FILE),
                root.join(crate::spec::PARAKEET_DECODER_FILE),
                root.join(crate::spec::PARAKEET_JOINER_FILE),
                root.join(crate::spec::PARAKEET_TOKENS_FILE),
            ]
        );

        let frontend = owned_paths_under(&roots, DownloadTarget::KokoroFrontend);
        assert!(frontend.contains(&root.join(crate::spec::KOKORO_G2P_ENCODER_FILE)));
        assert!(frontend.contains(&root.join(crate::spec::KOKORO_G2P_DECODER_FILE)));
        assert!(frontend.contains(&crate::kokoro_frontend::espeak_dir_under(root)));
        let kokoro = owned_paths_under(&roots, DownloadTarget::KokoroModel);
        for path in &frontend {
            assert!(!kokoro.contains(path), "{path:?} must not be a kokoro path");
        }
        assert!(
            owned_paths_under(&roots, DownloadTarget::Onnxruntime)
                .contains(&root.join(crate::ort::onnxruntime_dylib_file()))
        );
        assert!(owned_paths_under(&roots, DownloadTarget::DiarizationMlx).is_empty());
        assert!(owned_paths_under(&roots, DownloadTarget::DiarizationFluid).is_empty());

        // Fluid: two repos (one outside model root). Voice pack stays on the ONNX variant.
        let fluid = owned_paths_under(&roots, DownloadTarget::KokoroFluid);
        assert_eq!(
            fluid,
            vec![
                roots
                    .model
                    .join("coreml")
                    .join(crate::coreml_repo::KOKORO_COREML_DIR_NAME),
                roots
                    .fluid
                    .join(crate::coreml_repo::KOKORO_G2P_COREML_DIR_NAME),
            ]
        );
        for path in &kokoro {
            assert!(
                !fluid.contains(path),
                "{path:?} belongs to the ONNX variant"
            );
        }
    }

    /// `flat_weight_files` follows its argument (not hardcoded to Kokoro).
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

    /// Directory + flat + MLX fixtures.
    fn seed(roots: &ModelRoots) {
        let root = roots.model.as_path();
        let chatterbox = crate::tts_assets::tts_model_dir_under(root, TtsModel::Chatterbox);
        for file in crate::tts_assets::tts_ort_asset_set(TtsModel::Chatterbox).files_for(false) {
            write(&chatterbox.join(file.file_name), b"chatterbox");
        }
        for name in PARAKEET_ONNX_FILES {
            write(&root.join(name), b"parakeet");
        }
        let kokoro_mlx = roots.dir_for(&crate::mlx_repo::KOKORO_MLX);
        for file in crate::mlx_repo::KOKORO_MLX.files {
            write(&kokoro_mlx.join(file.path), b"k");
        }
        write(
            &kokoro_mlx.join(".ds-ready"),
            crate::mlx_repo::KOKORO_MLX.revision.as_bytes(),
        );
    }

    /// Full Kokoro ONNX set (incl. G2P — presence matches `tts_model_files_present`).
    fn seed_kokoro_onnx(root: &Path) {
        let dir = crate::tts_assets::tts_model_dir_under(root, TtsModel::Kokoro);
        for file in crate::tts_assets::tts_ort_asset_set(TtsModel::Kokoro).files_for(false) {
            write(&dir.join(file.file_name), b"kokoro");
        }
    }

    /// Fluid Kokoro set with markers; no shared voice pack (files alone install).
    fn seed_kokoro_fluid(roots: &ModelRoots) {
        for repo in crate::coreml_repo::KOKORO_COREML_SET {
            let dir = roots.dir_for(repo);
            for file in repo.files {
                write(&dir.join(file.path), b"coreml");
            }
            write(&dir.join(".ds-ready"), repo.revision.as_bytes());
        }
    }

    /// No engine resolves; only on-disk state can reference shared assets.
    fn nothing_selected() -> VoiceConfig {
        VoiceConfig {
            tts_engine: Some(Vec::new()),
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        }
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
        let roots = roots_under(root.path());
        seed(&roots);
        let before = entries(&roots.model);
        let assets = scan_at(&roots);
        assert_eq!(entries(&roots.model), before, "scan_at must create nothing");
        assert!(!roots.fluid.exists(), "nor any root it does not read");

        let chatterbox = asset(&assets, "chatterbox");
        let onnx = variant(chatterbox, DownloadTarget::ChatterboxModel).unwrap();
        assert!(onnx.installed);
        assert!(onnx.bytes > 0);
        let parakeet = asset(&assets, "parakeet");
        let parakeet_onnx = variant(parakeet, DownloadTarget::ParakeetModel).unwrap();
        assert!(parakeet_onnx.installed);
        assert_eq!(parakeet_onnx.bytes, 8 * PARAKEET_ONNX_FILES.len() as u64);

        let kokoro = asset(&assets, "kokoro");
        match variant(kokoro, DownloadTarget::KokoroMlx) {
            Some(mlx) => {
                assert!(
                    mlx.installed,
                    "a seeded set at the pinned revision is ready"
                );
                let dir = roots.dir_for(&crate::mlx_repo::KOKORO_MLX);
                std::fs::write(dir.join(".ds-ready"), "0000000").unwrap();
                let stale = scan_at(&roots);
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

    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn cuda_inventory_requires_current_marker_and_runtime_files() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let dir = crate::ort::cuda_runtime_dir_under(&roots.model);
        std::fs::create_dir_all(&dir).unwrap();
        let installed = || {
            let assets = scan_at(&roots);
            variant(asset(&assets, "cuda"), DownloadTarget::Cuda)
                .unwrap()
                .installed
        };

        assert!(!installed(), "an empty cuda directory is not installed");

        #[cfg(target_os = "windows")]
        for name in [
            "onnxruntime.dll",
            "onnxruntime_providers_cuda.dll",
            "cudnn64_9.dll",
        ] {
            write(&dir.join(name), b"runtime");
        }
        #[cfg(target_os = "linux")]
        for name in [
            "libonnxruntime.so.1.26.0",
            "libonnxruntime_providers_cuda.so",
            "libcudnn.so.9",
        ] {
            write(&dir.join(name), b"runtime");
        }

        let marker = crate::ort::cuda_version_marker(&dir);
        write(&marker, b"");
        assert!(!installed(), "an empty fingerprint is not installed");

        write(&marker, b"stale");
        assert!(!installed(), "a stale fingerprint is not installed");

        write(&marker, crate::ort::cuda_version_fingerprint().as_bytes());
        assert!(installed(), "the current complete runtime is installed");

        #[cfg(target_os = "windows")]
        std::fs::remove_file(dir.join("onnxruntime_providers_cuda.dll")).unwrap();
        #[cfg(target_os = "linux")]
        std::fs::remove_file(dir.join("libonnxruntime_providers_cuda.so")).unwrap();
        assert!(
            !installed(),
            "a current marker cannot hide a partial runtime"
        );
    }

    /// Fluid-only Kokoro installs the row; remove reclaims both roots (incl. FluidAudio cache).
    #[test]
    fn a_kokoro_row_holding_only_the_fluid_variant_is_installed_and_fully_reclaimed() {
        if !DownloadTarget::KokoroFluid.is_supported_on_this_host() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        seed_kokoro_fluid(&roots);
        let coreml = roots.dir_for(&crate::coreml_repo::KOKORO_COREML);
        let g2p = roots.dir_for(&crate::coreml_repo::KOKORO_G2P_COREML);
        assert_fixture_is_isolated(root.path(), &coreml);
        assert_fixture_is_isolated(root.path(), &g2p);

        let assets = scan_at(&roots);
        let kokoro = asset(&assets, "kokoro");
        assert!(
            kokoro.installed(),
            "the Fluid variant alone installs the row"
        );
        assert!(
            variant(kokoro, DownloadTarget::KokoroFluid)
                .unwrap()
                .installed
        );
        assert!(
            !variant(kokoro, DownloadTarget::KokoroModel)
                .unwrap()
                .installed
        );
        assert!(kokoro.bytes() > 0);

        let reclaimed = remove_at(&roots, &nothing_selected(), "kokoro").unwrap();
        assert!(reclaimed > 0);
        assert!(!coreml.exists());
        assert!(!g2p.exists(), "the third-party cache is reclaimed too");
    }

    /// Fluid Kokoro holds ORT + frontend even with nothing selected (#220).
    #[test]
    fn kokoro_fluid_keeps_the_onnx_runtime_referenced() {
        assert_eq!(ORT_CONSUMER_TARGETS.len(), 7);
        assert!(ORT_CONSUMER_TARGETS.contains(&DownloadTarget::KokoroFluid));
        if !DownloadTarget::KokoroFluid.is_supported_on_this_host() {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let cfg = nothing_selected();
        assert!(!shared_asset_referenced(&roots, &cfg, "onnxruntime"));

        seed_kokoro_fluid(&roots);
        assert!(shared_asset_referenced(&roots, &cfg, "onnxruntime"));
        assert!(shared_asset_referenced(&roots, &cfg, "kokoro_frontend"));
        for id in ["onnxruntime", "kokoro_frontend"] {
            let err = remove_at(&roots, &cfg, id).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{id}");
        }
    }

    #[test]
    fn a_half_seeded_set_is_not_installed_but_still_reports_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let dir = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Qwen);
        let first = crate::tts_assets::tts_ort_asset_set(TtsModel::Qwen).files[0];
        write(&dir.join(first.file_name), b"partial download");
        let assets = scan_at(&roots);
        let qwen = variant(asset(&assets, "qwen"), DownloadTarget::QwenModel).unwrap();
        assert!(!qwen.installed);
        assert_eq!(qwen.bytes, 16);
    }

    #[test]
    fn scan_of_a_missing_root_is_all_zero_and_leaves_it_missing() {
        let parent = tempfile::tempdir().unwrap();
        let roots = roots_under(&parent.path().join("never-created"));
        let assets = scan_at(&roots);
        assert!(!assets.is_empty());
        for asset in &assets {
            assert!(!asset.installed(), "{}", asset.id);
            assert_eq!(asset.bytes(), 0, "{}", asset.id);
        }
        assert!(
            !roots.model.exists(),
            "scan_at must not create the model root"
        );
        assert!(!roots.fluid.exists(), "nor any other root");
    }

    #[test]
    fn asset_targets_cover_every_removable_token_and_nothing_else() {
        for id in ds_config::REMOVABLE_ASSET_TOKENS {
            let targets = asset_targets(id).unwrap_or_else(|| panic!("{id} has a row"));
            assert!(!targets.is_empty(), "{id}");
        }
        for id in ["sepformer", "bogus", ""] {
            assert!(asset_targets(id).is_none(), "{id} must not have a row");
        }
        for row in ROWS {
            assert!(
                ds_config::REMOVABLE_ASSET_TOKENS.contains(&row.id),
                "{} is a row the remove enum does not advertise",
                row.id
            );
            assert_eq!(
                row.kind.is_shared(),
                ds_config::SHARED_ASSET_TOKENS.contains(&row.id),
                "{}",
                row.id
            );
            if row.kind.is_shared() {
                assert_eq!(row.targets.len(), 1, "{}", row.id);
                assert_eq!(row.id, row.targets[0].as_str(), "{}", row.id);
            }
        }
    }

    /// Empty disk + nothing selected: only the fail-closed default can answer `true`.
    #[test]
    fn every_shared_row_has_its_own_reference_arm() {
        let empty = tempfile::tempdir().unwrap();
        let roots = roots_under(empty.path());
        let cfg = nothing_selected();
        for row in ROWS.iter().filter(|row| row.kind.is_shared()) {
            assert!(
                !shared_asset_referenced(&roots, &cfg, row.id),
                "`{}` has no arm in shared_asset_referenced",
                row.id
            );
        }
    }

    #[test]
    fn remove_deletes_only_what_the_asset_owns() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        seed(&roots);
        let chatterbox = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        let qwen = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Qwen);
        write(&qwen.join("keep.onnx"), b"another model");
        let dylib = roots.model.join(crate::ort::onnxruntime_dylib_file());
        write(&dylib, b"shared runtime");
        let expected = dir_size_at(&chatterbox);

        assert_eq!(
            remove_at(&roots, &nothing_selected(), "chatterbox").unwrap(),
            expected
        );
        assert!(!chatterbox.exists());
        assert!(qwen.join("keep.onnx").is_file(), "a sibling model survives");
        assert!(dylib.is_file(), "the shared runtime is never removed");
        assert!(
            roots.model.is_dir(),
            "the model root itself is never removed"
        );
        assert!(
            roots
                .model
                .join(crate::spec::PARAKEET_ENCODER_FILE)
                .is_file(),
            "a flat sibling asset survives"
        );
    }

    /// Absent model: 0 bytes reclaimed; only flight lock scaffolding appears.
    #[test]
    fn removing_an_absent_model_reclaims_nothing_and_creates_only_lock_scaffolding() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let chatterbox = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        assert_eq!(
            remove_at(&roots, &nothing_selected(), "chatterbox").unwrap(),
            0
        );

        assert!(!chatterbox.exists(), "no model directory is materialized");
        let mut expected = vec![
            ".chatterbox-multilingual.lock".to_string(),
            ".orphan-sweep.gate".to_string(),
            "mlx".to_string(),
        ];
        expected.sort();
        assert_eq!(entries(&roots.model), expected);
        let mlx = ds_config::mlx_dir_under(&roots.model);
        assert_eq!(entries(&mlx), vec!["mlx-audio".to_string()]);
        // Rooted removal: nested destinations share the model-root gate (no per-parent gate).
        assert_eq!(
            entries(&mlx.join("mlx-audio")),
            vec![
                ".mlx-community_S3TokenizerV2.lock".to_string(),
                ".mlx-community_chatterbox-8bit.lock".to_string(),
            ]
        );
    }

    #[test]
    fn remove_rejects_an_id_it_does_not_own() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        for id in ["bogus", ""] {
            let err = remove_at(&roots, &nothing_selected(), id).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{id}");
        }
        assert!(!roots.model.exists(), "a refusal locks nothing");
    }

    #[test]
    fn shared_references_follow_what_is_installed() {
        let cfg = nothing_selected();
        let empty = tempfile::tempdir().unwrap();
        let empty_roots = roots_under(empty.path());
        for id in ds_config::SHARED_ASSET_TOKENS {
            assert!(
                !shared_asset_referenced(&empty_roots, &cfg, id),
                "{id} on an empty root"
            );
        }

        let chatterbox = tempfile::tempdir().unwrap();
        let chatterbox_roots = roots_under(chatterbox.path());
        let dir =
            crate::tts_assets::tts_model_dir_under(&chatterbox_roots.model, TtsModel::Chatterbox);
        for file in crate::tts_assets::tts_ort_asset_set(TtsModel::Chatterbox).files_for(false) {
            write(&dir.join(file.file_name), b"chatterbox");
        }
        assert!(shared_asset_referenced(
            &chatterbox_roots,
            &cfg,
            "onnxruntime"
        ));
        assert!(!shared_asset_referenced(
            &chatterbox_roots,
            &cfg,
            "kokoro_frontend"
        ));

        let kokoro = tempfile::tempdir().unwrap();
        let kokoro_roots = roots_under(kokoro.path());
        seed_kokoro_onnx(&kokoro_roots.model);
        assert!(shared_asset_referenced(&kokoro_roots, &cfg, "onnxruntime"));
        assert!(shared_asset_referenced(
            &kokoro_roots,
            &cfg,
            "kokoro_frontend"
        ));

        let sepformer = tempfile::tempdir().unwrap();
        let sepformer_roots = roots_under(sepformer.path());
        write(
            &sepformer_roots.model.join(crate::spec::SEPFORMER_FILE),
            b"separator",
        );
        assert!(shared_asset_referenced(
            &sepformer_roots,
            &cfg,
            "onnxruntime"
        ));
        assert!(!shared_asset_referenced(
            &sepformer_roots,
            &cfg,
            "kokoro_frontend"
        ));

        if DownloadTarget::KokoroMlx.is_supported_on_this_host() {
            let mlx = tempfile::tempdir().unwrap();
            let mlx_roots = roots_under(mlx.path());
            let dir = mlx_roots.dir_for(&crate::mlx_repo::KOKORO_MLX);
            for file in crate::mlx_repo::KOKORO_MLX.files {
                write(&dir.join(file.path), b"k");
            }
            write(
                &dir.join(".ds-ready"),
                crate::mlx_repo::KOKORO_MLX.revision.as_bytes(),
            );
            assert!(shared_asset_referenced(&mlx_roots, &cfg, "onnxruntime"));
            assert!(shared_asset_referenced(&mlx_roots, &cfg, "kokoro_frontend"));
        }
    }

    #[test]
    fn shared_references_follow_the_selection() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let built_in_tts = |model| VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: model,
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        };

        let kokoro = built_in_tts(TtsModel::Kokoro);
        assert!(shared_asset_referenced(&roots, &kokoro, "onnxruntime"));
        assert!(shared_asset_referenced(&roots, &kokoro, "kokoro_frontend"));

        let chatterbox = built_in_tts(TtsModel::Chatterbox);
        assert!(shared_asset_referenced(&roots, &chatterbox, "onnxruntime"));
        assert!(!shared_asset_referenced(
            &roots,
            &chatterbox,
            "kokoro_frontend"
        ));

        let stt_only = VoiceConfig {
            tts_engine: Some(Vec::new()),
            stt_engine: Some(vec![ds_config::SttEngine::BuiltIn]),
            ..VoiceConfig::default()
        };
        assert!(shared_asset_referenced(&roots, &stt_only, "onnxruntime"));

        let off = nothing_selected();
        for id in ds_config::SHARED_ASSET_TOKENS {
            assert!(!shared_asset_referenced(&roots, &off, id), "{id}");
        }

        let diarizing = VoiceConfig {
            speaker_lock: true,
            diarizer: vec![ds_config::DiarizerProvider::Mlx],
            ..nothing_selected()
        };
        assert_eq!(
            shared_asset_referenced(&roots, &diarizing, "onnxruntime"),
            DownloadTarget::SepformerModel.is_supported_on_this_host()
        );
    }

    #[test]
    fn cuda_reference_follows_the_resolved_provider() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let has_row = DownloadTarget::Cuda.is_supported_on_this_host();
        // Driverless host uses CPU EP and never prefetches — reclaimable despite ladder.
        let held = has_row && crate::ort::cuda_driver_available();
        let built_in_tts = |provider| VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            stt_engine: Some(Vec::new()),
            provider: vec![provider],
            ..VoiceConfig::default()
        };

        let cpu = built_in_tts(ds_config::Provider::OrtCpu);
        assert!(!shared_asset_referenced(&roots, &cpu, "cuda"));

        let cuda = built_in_tts(ds_config::Provider::OrtCuda);
        assert_eq!(cuda_runtime_wanted(&cuda), has_row);
        assert_eq!(shared_asset_referenced(&roots, &cuda, "cuda"), held);

        let stt_cuda = VoiceConfig {
            tts_engine: Some(Vec::new()),
            stt_engine: Some(vec![ds_config::SttEngine::BuiltIn]),
            provider: vec![ds_config::Provider::OrtCuda],
            ..VoiceConfig::default()
        };
        assert_eq!(shared_asset_referenced(&roots, &stt_cuda, "cuda"), held);

        let off = VoiceConfig {
            provider: vec![ds_config::Provider::OrtCuda],
            ..nothing_selected()
        };
        assert!(!shared_asset_referenced(&roots, &off, "cuda"));
    }

    #[test]
    fn an_unreferenced_shared_asset_is_removed() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let dylib = roots.model.join(crate::ort::onnxruntime_dylib_file());
        assert_fixture_is_isolated(root.path(), &dylib);
        let paths = crate::ort::onnxruntime_paths_under(&roots.model);
        for path in &paths {
            write(path, b"managed runtime");
        }
        let expected: u64 = paths.iter().map(|path| dir_size_at(path)).sum();

        assert_eq!(
            remove_at(&roots, &nothing_selected(), "onnxruntime").unwrap(),
            expected
        );
        for path in &paths {
            assert!(!path.exists(), "{path:?}");
        }
    }

    #[test]
    fn remove_at_refuses_a_referenced_shared_asset() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let dir = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Chatterbox);
        for file in crate::tts_assets::tts_ort_asset_set(TtsModel::Chatterbox).files_for(false) {
            write(&dir.join(file.file_name), b"chatterbox");
        }
        let dylib = roots.model.join(crate::ort::onnxruntime_dylib_file());
        write(&dylib, b"managed runtime");

        let err = remove_at(&roots, &nothing_selected(), "onnxruntime").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(dylib.is_file(), "a refusal deletes nothing");
    }

    /// Never remove what `compute_needs` would immediately re-fetch.
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
        let roots = roots_under(root.path());
        seed(&roots);
        // ONNX Kokoro so frontend is referenced on every host (seed's MLX is Apple-only).
        seed_kokoro_onnx(&roots.model);
        // Explicit STT off — default ladder is host-dependent and would flip `parakeet`.
        let cfg = VoiceConfig {
            tts_engine: Some(vec![ds_config::TtsEngine::BuiltIn]),
            tts_model: TtsModel::Chatterbox,
            stt_engine: Some(Vec::new()),
            ..VoiceConfig::default()
        };
        let payload = inventory_json(&roots, &cfg, &[DownloadTarget::QwenModel], None);
        assert_eq!(payload["model_dir"], roots.model.display().to_string());
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

        // In-flight Qwen blocks every shared row; `reason` still splits by host/driver.
        for id in ds_config::SHARED_ASSET_TOKENS {
            let Some(shared) = assets.iter().find(|asset| asset["id"] == *id) else {
                assert_eq!(
                    *id, "cuda",
                    "only the CUDA row is host-gated off x86_64 Windows/Linux"
                );
                continue;
            };
            let referenced =
                *id != ds_config::CUDA_ASSET_TOKEN || crate::ort::cuda_driver_available();
            assert_eq!(shared["removable"], false, "{id}");
            assert_eq!(
                shared["reason"],
                if referenced {
                    json!("shared")
                } else {
                    Value::Null
                },
                "{id}"
            );
            assert_eq!(shared["active"], false, "{id}");
        }
        let ids: Vec<&str> = assets
            .iter()
            .map(|asset| asset["id"].as_str().unwrap())
            .collect();
        let expected: Vec<&str> = ds_config::REMOVABLE_ASSET_TOKENS
            .iter()
            .copied()
            .filter(|id| {
                asset_targets(id)
                    .unwrap()
                    .iter()
                    .any(|target| target.is_supported_on_this_host())
            })
            .collect();
        assert_eq!(ids, expected);

        let empty = tempfile::tempdir().unwrap();
        let free = inventory_json(&roots_under(empty.path()), &nothing_selected(), &[], None);
        for shared in
            free["assets"].as_array().unwrap().iter().filter(|asset| {
                ds_config::SHARED_ASSET_TOKENS.contains(&asset["id"].as_str().unwrap())
            })
        {
            assert_eq!(shared["removable"], true, "{}", shared["id"]);
            assert_eq!(shared["reason"], Value::Null, "{}", shared["id"]);
        }
    }

    #[test]
    fn a_removal_payload_reports_the_reclaimed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        seed(&roots);
        let chatterbox = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Chatterbox);
        assert_fixture_is_isolated(root.path(), &chatterbox);
        let bytes = remove_at(&roots, &nothing_selected(), "chatterbox").unwrap();
        assert!(bytes > 0);
        let payload = inventory_json(
            &roots,
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

    /// Cross-process directory flight blocks removal until release.
    #[test]
    fn removal_waits_for_a_cross_process_directory_installer() {
        const CHILD_TARGET: &str = "DS_MODEL_INVENTORY_CHILD_TARGET";
        const CHILD_READY: &str = "DS_MODEL_INVENTORY_CHILD_READY";
        const CHILD_RELEASE: &str = "DS_MODEL_INVENTORY_CHILD_RELEASE";

        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let target = crate::tts_assets::tts_model_dir_under(&roots.model, TtsModel::Chatterbox);
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
            .env_remove("DONTSPEAK_MODEL_DIR") // same sweep root as parent (#204)
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

        let removal_roots = roots.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            let bytes = remove_at(&removal_roots, &nothing_selected(), "chatterbox").unwrap();
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

    /// Child half of the cross-process flight test; inert without parent env.
    #[test]
    fn inventory_directory_lock_child() {
        let Some(target) = std::env::var_os("DS_MODEL_INVENTORY_CHILD_TARGET") else {
            return;
        };
        let target = PathBuf::from(target);
        let ready = PathBuf::from(std::env::var_os("DS_MODEL_INVENTORY_CHILD_READY").unwrap());
        let release = PathBuf::from(std::env::var_os("DS_MODEL_INVENTORY_CHILD_RELEASE").unwrap());
        with_destination_flight_in(None, &target, |_| {
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

    /// In-flight file install finishes before removal deletes it (httpmock seam).
    #[test]
    fn removal_does_not_interleave_with_an_in_flight_install() {
        use std::sync::atomic::{AtomicBool, Ordering};

        const BODY: &[u8] = b"a freshly downloaded kokoro graph";
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        let onnx = roots.model.join(crate::spec::KOKORO_ONNX_FILE);
        assert_fixture_is_isolated(root.path(), &onnx);
        let voices = roots.model.join(crate::spec::KOKORO_VOICES_FILE);
        write(&voices, b"already installed voices");

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
        let install_root = roots.model.clone();
        let installer = std::thread::spawn(move || {
            // First progress tick is the in-flight rendezvous (before rename).
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

        let removal_roots = roots.clone();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remover = std::thread::spawn(move || {
            removed_tx
                .send(remove_at(&removal_roots, &nothing_selected(), "kokoro").unwrap())
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

        assert!(!onnx.exists(), "the removal deletes the finalized download");
        assert!(!voices.exists());
        assert!(reclaimed >= b"already installed voices".len() as u64);
        assert!(!roots.model.join(".kokoro-v1.0-fp32.onnx.part").exists());
        assert!(
            !roots
                .model
                .join(".kokoro-v1.0-fp32.onnx.part.meta")
                .exists()
        );
        mock.assert_calls(1);
    }

    /// Do not materialize an unused FluidAudio cache on remove (#212).
    #[test]
    fn removing_kokoro_creates_nothing_in_a_fluid_cache_that_does_not_exist() {
        let root = tempfile::tempdir().unwrap();
        let roots = roots_under(root.path());
        assert_fixture_is_isolated(
            root.path(),
            &roots.model.join(crate::spec::KOKORO_ONNX_FILE),
        );
        assert!(!roots.fluid.exists());

        assert_eq!(remove_at(&roots, &nothing_selected(), "kokoro").unwrap(), 0);

        assert!(
            !roots.fluid.exists(),
            "a removal must not create a third-party cache this host never used"
        );
    }
}
