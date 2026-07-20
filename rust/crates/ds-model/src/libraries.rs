//! Libraries catalog — collects every third-party project the app DOWNLOADS at runtime
//! (built-in TTS + Parakeet models, the ONNX Runtime, and the optional CUDA/cuDNN GPU
//! runtime) with its license, homepage, and the files it fetches.
//!
//! The license metadata lives WITH the files in [`crate::urls`] — each
//! [`crate::urls::Project`] references its own [`crate::urls::Download`]s — so a file
//! can't drift away from its license. This module only SHAPES that registry into the
//! JSON the platform UIs render, and is cross-platform by construction: it's built from
//! the same registry every platform downloads from. Each platform's `Libraries` tab reads
//! it through the `ds_libraries_json` C-ABI export.
//!
//! Scope is deliberately the DOWNLOADED assets only (the part that lives next to its
//! files and so can never go stale). Compiled-in libraries are per-platform and not in
//! this registry; if ever listed they belong to the platform UI layer, not here.

use serde_json::{Value, json};

use crate::download::url_basename;
use crate::urls::{self, Download, Project};

/// One file entry: name + source URL, with the known size when we have one (the live
/// `Content-Length` is authoritative during a fetch, so an absent/zero size here just
/// means "don't show a size", never a wrong one).
fn file_obj(name: &str, url: &str, size_bytes: u64) -> Value {
    let mut o = json!({ "name": name, "url": url });
    if size_bytes > 0 {
        o["size_bytes"] = json!(size_bytes);
    }
    o
}

fn download_file(d: &Download) -> Value {
    file_obj(d.file_name, d.url, d.size_bytes)
}

fn project_obj(p: &Project, files: Vec<Value>) -> Value {
    json!({
        "name": p.name,
        "usage": p.usage,
        "homepage": p.homepage,
        "license": p.license,
        "license_url": p.license_url,
        "files": files,
    })
}

/// Add published model-language coverage to a TTS project. Runtime acceptance stays in the
/// descriptor's separate `languages` field; the Libraries wire describes the downloaded model.
fn with_tts_languages(mut project: Value, model: ds_config::TtsModel) -> Value {
    let descriptor = model.descriptor();
    project["languages"] = json!(descriptor.model_languages);
    project["language_count"] = json!(descriptor.supported_language_count());
    project["automatic_language_detection"] = json!(descriptor.detects_language_automatically());
    if let Some(url) = descriptor.language_list_url() {
        project["language_list_url"] = json!(url);
    }
    project
}

/// One JSON entry for an MLX model set. The license lives with the files on
/// [`MlxRepo`](crate::mlx_repo::MlxRepo), so this can't drift
/// from what's downloaded. The set's files are a whole pinned HuggingFace revision (fetched
/// via the tree API), so the single "file" we list is that pinned revision — `name` is the
/// repo, `url` links the exact tree the download reads.
fn mlx_project_obj(r: &crate::mlx_repo::MlxRepo) -> Value {
    let homepage = format!("https://huggingface.co/{}", r.repo);
    let tree_url = format!("{homepage}/tree/{}", r.revision);
    json!({
        "name": r.display_name,
        "usage": r.usage,
        "homepage": homepage,
        "license": r.license,
        "license_url": r.license_url,
        // The pinned revision IS the unit we fetch; show it as the single source "file".
        "files": [{ "name": r.repo, "url": tree_url }],
    })
}

fn tts_project_obj(set: &crate::tts_assets::TtsOrtAssetSet) -> Value {
    with_tts_languages(
        json!({
            "name": set.display_name,
            "usage": "Built-in text-to-speech model",
            "homepage": set.homepage,
            "license": set.license,
            "license_url": set.license_url,
            "files": set.files.iter().map(download_file).collect::<Vec<_>>(),
        }),
        set.model,
    )
}

/// The libraries catalog for the UI's Libraries tab, FILTERED to the platform this build runs
/// on ([`urls::current_platform`]): a JSON array of `{name, usage, homepage, license,
/// license_url, files:[{name, url, size_bytes?}], languages?, language_count?,
/// automatic_language_detection?, language_list_url?}`, in display order — system libraries (CUDA,
/// cuDNN) → runtime (ONNX Runtime) → portable models (Kokoro, Parakeet) → MLX
/// sets, lowest-level first. The per-platform rule is DATA: each [`Project`] declares its
/// `platforms` and the MLX sets are [`Platform::APPLE_MLX`](urls::Platform::APPLE_MLX),
/// so the collector just filters — no scattered `#[cfg]` deciding what to show. (The CUDA
/// file *URLs* live in the
/// cfg-gated `CUDA_WHEELS`, so only their file-assembly stays cfg-gated; the applicability
/// decision is the data above.) Built entirely from the [`crate::urls`] / [`crate::mlx_repo`]
/// registries, so it can never drift from what's actually fetched.
pub fn catalog() -> Value {
    let plat = urls::current_platform();
    let mut projects: Vec<Value> = Vec::new();

    // ONNX Runtime — the load-dynamic dist archive (platform-selected; `None` where there's no
    // pinned dist, e.g. Intel macOS), plus the GPU build wheel on Windows x64 (split out of
    // CUDA_WHEELS below, since onnxruntime-gpu is MIT, not NVIDIA).
    let mut onnx_files: Vec<Value> = Vec::new();
    if let Some(d) = crate::ort::onnxruntime_dist() {
        onnx_files.push(file_obj(
            url_basename(d.url),
            d.url,
            urls::ONNXRUNTIME_DIST_SIZE_BYTES,
        ));
    }

    // Optional CUDA / cuDNN GPU runtime. The pinned wheel URLs only exist on Windows/Linux x64
    // (`CUDA_WHEELS` is cfg-gated), so the file-assembly is the one part that stays cfg-gated;
    // the applicability is the projects' `platforms`. Split the wheels by license: onnxruntime-gpu
    // → ONNX Runtime (MIT); cuDNN → its own SLA; everything else → the CUDA Toolkit EULA.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    let (cuda_files, cudnn_files) = {
        let mut cuda: Vec<Value> = Vec::new();
        let mut cudnn: Vec<Value> = Vec::new();
        for (u, _sha) in urls::CUDA_WHEELS {
            let base = url_basename(u);
            if base.starts_with("onnxruntime_gpu") {
                onnx_files.push(file_obj(base, u, 0));
            } else if base.starts_with("nvidia_cudnn") {
                cudnn.push(file_obj(base, u, 0));
            } else {
                cuda.push(file_obj(base, u, 0));
            }
        }
        (cuda, cudnn)
    };
    #[cfg(not(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    )))]
    let (cuda_files, cudnn_files): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());

    // CUDA/cuDNN then ORT, then models by function (TTS→STT→diarize); ONNX before MLX.
    if urls::NVIDIA_CUDA.runs_on(plat) && !cuda_files.is_empty() {
        projects.push(project_obj(&urls::NVIDIA_CUDA, cuda_files));
    }
    if urls::NVIDIA_CUDNN.runs_on(plat) && !cudnn_files.is_empty() {
        projects.push(project_obj(&urls::NVIDIA_CUDNN, cudnn_files));
    }

    if urls::ONNX_RUNTIME.runs_on(plat) && !onnx_files.is_empty() {
        projects.push(project_obj(&urls::ONNX_RUNTIME, onnx_files));
    }

    let apple = urls::Platform::APPLE_MLX.contains(&plat);
    let push_portable = |projects: &mut Vec<Value>, p: &Project| {
        if p.runs_on(plat) {
            let files = p.files.iter().map(download_file).collect();
            projects.push(project_obj(p, files));
        }
    };
    let push_mlx = |projects: &mut Vec<Value>, r: &crate::mlx_repo::MlxRepo| {
        if apple && !r.display_name.is_empty() {
            projects.push(mlx_project_obj(r));
        }
    };

    // Kokoro stays two [`Project`] entries: the ONNX set includes the two PeterReid G2P
    // graphs, and listing them under "Kokoro" would mis-attribute them.
    if urls::KOKORO.runs_on(plat) {
        projects.push(with_tts_languages(
            project_obj(
                &urls::KOKORO,
                urls::KOKORO.files.iter().map(download_file).collect(),
            ),
            ds_config::TtsModel::Kokoro,
        ));
    }
    push_portable(&mut projects, &urls::KOKORO_G2P);
    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    ))]
    push_portable(&mut projects, &urls::KOKORO_ESPEAK_FRONTEND);
    push_portable(&mut projects, &urls::KOKORO_JAPANESE_FRONTEND);
    projects.extend(
        crate::tts_assets::TTS_ORT_ASSETS
            .iter()
            .filter(|set| set.model != ds_config::TtsModel::Kokoro)
            .map(tts_project_obj),
    );
    push_portable(&mut projects, &urls::PARAKEET);
    for repo in crate::mlx_repo::all_mlx_repos() {
        push_mlx(&mut projects, repo);
    }
    push_portable(&mut projects, &urls::SEPFORMER_PROJECT);

    Value::Array(projects)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every project in the catalog must carry a full, non-empty license record and at
    /// least one file. This is the can't-drift guard: a profile added without a license,
    /// homepage, or license URL fails here.
    #[test]
    fn every_project_is_fully_licensed() {
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is a JSON array");
        assert!(!arr.is_empty(), "catalog is empty");
        for p in arr {
            for key in ["name", "usage", "homepage", "license", "license_url"] {
                assert!(
                    !p[key].as_str().unwrap_or("").is_empty(),
                    "project field `{key}` is empty in {p}"
                );
            }
            // A homepage equal to the license URL is the URL-heuristic fallback bug: the
            // attribution must always link a real project page.
            assert_ne!(
                p["homepage"], p["license_url"],
                "project `{}` uses its license URL as homepage",
                p["name"]
            );
            assert!(
                p["files"].as_array().is_some_and(|f| !f.is_empty()),
                "project `{}` has no files",
                p["name"]
            );
        }
    }

    /// Every model download in the registry (built-in TTS + Parakeet, present on every
    /// platform) must appear in the catalog. Add a model file without putting it under a
    /// licensed [`Project`] and this fails — the file/license sync guarantee the UI relies on.
    #[test]
    fn all_model_downloads_are_in_the_catalog() {
        let cat = catalog();
        let names: Vec<String> = cat
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|p| p["files"].as_array().unwrap().iter())
            .map(|f| f["name"].as_str().unwrap().to_string())
            .collect();
        let mut downloads: Vec<_> = crate::tts_assets::TTS_ORT_ASSETS
            .iter()
            .flat_map(|set| set.files.iter())
            .copied()
            .chain([
                urls::KOKORO_JAPANESE_DICTIONARY,
                urls::PARAKEET_ENCODER,
                urls::PARAKEET_DECODER,
                urls::PARAKEET_JOINER,
                urls::PARAKEET_TOKENS,
            ])
            .collect();
        #[cfg(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64")
        ))]
        downloads.push(urls::ESPEAKNG_LOADER);
        for d in downloads {
            assert!(
                names.iter().any(|n| n == d.file_name),
                "download `{}` is missing from the libraries catalog",
                d.file_name
            );
        }
    }

    #[test]
    fn tts_projects_expose_published_model_language_coverage() {
        let cat = catalog();
        let projects = cat.as_array().unwrap();
        for model in ds_config::TtsModel::ALL.iter().copied() {
            let descriptor = model.descriptor();
            let project = projects
                .iter()
                .find(|project| project["name"].as_str() == Some(descriptor.display_name))
                .unwrap_or_else(|| panic!("missing TTS project {}", descriptor.display_name));
            assert_eq!(project["languages"], json!(descriptor.model_languages));
            assert_eq!(
                project["language_count"],
                descriptor.supported_language_count()
            );
            assert_eq!(
                project["automatic_language_detection"],
                descriptor.detects_language_automatically()
            );
            match descriptor.language_list_url() {
                Some(url) => assert_eq!(project["language_list_url"], url),
                None => assert!(project.get("language_list_url").is_none()),
            }
        }
    }

    /// The per-platform applicability MATRIX — pure data, so it asserts every target at once
    /// (the can't-drift guarantee the data-driven catalog rests on). This is the Rust home of
    /// what the old macOS-only `LibraryCatalogTests` checked; the rule now lives in ONE place.
    #[test]
    fn platform_applicability_matrix_is_pinned() {
        use urls::Platform::*;

        // Portable model assets — every shipped target.
        for p in urls::Platform::ALL.iter().copied() {
            assert!(urls::KOKORO.runs_on(p), "Kokoro must apply on {p:?}");
            assert!(urls::PARAKEET.runs_on(p), "Parakeet must apply on {p:?}");
        }

        // ONNX Runtime — everywhere EXCEPT Intel macOS (no pinned dist there).
        assert!(urls::ONNX_RUNTIME.runs_on(WindowsX64));
        assert!(urls::ONNX_RUNTIME.runs_on(WindowsArm64));
        assert!(urls::ONNX_RUNTIME.runs_on(LinuxX64));
        assert!(urls::ONNX_RUNTIME.runs_on(MacArm64));
        assert!(
            !urls::ONNX_RUNTIME.runs_on(MacX64),
            "Intel macOS has no ONNX Runtime dist, so it must not list it"
        );

        // CUDA / cuDNN — x64 Windows + Linux only; never Windows-on-ARM or any Mac.
        for proj in [&urls::NVIDIA_CUDA, &urls::NVIDIA_CUDNN] {
            assert!(proj.runs_on(WindowsX64), "{} on Win x64", proj.name);
            assert!(proj.runs_on(LinuxX64), "{} on Linux x64", proj.name);
            for p in [WindowsArm64, MacArm64, MacX64] {
                assert!(!proj.runs_on(p), "{} must NOT apply on {p:?}", proj.name);
            }
        }
    }

    /// MLX sets are Apple-Silicon only and every downloaded set is cataloged.
    #[test]
    fn mlx_sets_are_apple_silicon_only_and_fully_licensed() {
        use crate::mlx_repo;

        assert_eq!(
            urls::Platform::APPLE_MLX,
            &[urls::Platform::MacArm64],
            "MLX Audio sets run only on Apple Silicon"
        );
        // Every listed MLX set carries a full license record (the can't-drift guard for
        // the macOS additions, independent of which platform the test runs on).
        for r in mlx_repo::all_mlx_repos() {
            for (field, val) in [
                ("usage", r.usage),
                ("license", r.license),
                ("license_url", r.license_url),
            ] {
                assert!(!val.is_empty(), "MLX set `{}` has empty {field}", r.name);
            }
        }
    }

    /// On THIS build's platform the catalog matches `current_platform()` exactly: it's
    /// non-empty, every entry's `platforms` includes us, and the MLX sets are present
    /// iff we're on Apple Silicon. Runs on whatever target `cargo test` is invoked on.
    #[test]
    fn catalog_matches_the_current_platform() {
        let plat = urls::current_platform();
        let cat = catalog();
        let arr = cat.as_array().expect("catalog is a JSON array");
        assert!(!arr.is_empty(), "catalog is empty on {plat:?}");

        let names: Vec<String> = arr
            .iter()
            .map(|p| p["name"].as_str().unwrap_or_default().to_string())
            .collect();

        // CUDA/cuDNN never show on a non-CUDA platform.
        if !urls::Platform::WITH_CUDA.contains(&plat) {
            assert!(
                !names
                    .iter()
                    .any(|n| n.contains("CUDA") || n.contains("cuDNN")),
                "no GPU libraries on {plat:?}, got {names:?}"
            );
        }
        // MLX sets show only on Apple Silicon.
        let has_mlx = names.iter().any(|n| n.contains("MLX"));
        assert_eq!(
            has_mlx,
            urls::Platform::APPLE_MLX.contains(&plat),
            "MLX presence must track Apple Silicon on {plat:?}, got {names:?}"
        );
    }

    /// On Windows/Linux x64 every pinned CUDA wheel must be bucketed into a project (no wheel
    /// left license-less), and the cuDNN wheel must land under the cuDNN SLA, not the CUDA EULA.
    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn all_cuda_wheels_are_bucketed_by_license() {
        let cat = catalog();
        let arr = cat.as_array().unwrap();
        let names_for = |license: &str| -> Vec<String> {
            arr.iter()
                .filter(|p| p["license"].as_str() == Some(license))
                .flat_map(|p| p["files"].as_array().unwrap().iter())
                .map(|f| f["name"].as_str().unwrap().to_string())
                .collect()
        };
        let mit = names_for("MIT");
        let eula = names_for("NVIDIA CUDA Toolkit EULA");
        let sla = names_for("NVIDIA cuDNN SLA");
        let all: Vec<String> = mit
            .iter()
            .chain(eula.iter())
            .chain(sla.iter())
            .cloned()
            .collect();
        for (u, _sha) in urls::CUDA_WHEELS {
            let base = url_basename(u);
            assert!(
                all.iter().any(|n| n == base),
                "CUDA wheel `{base}` is not in any licensed project"
            );
        }
        assert!(
            sla.iter().any(|n| n.starts_with("nvidia_cudnn")),
            "the cuDNN wheel must be listed under the cuDNN SLA project"
        );
        assert!(
            mit.iter().any(|n| n.starts_with("onnxruntime_gpu")),
            "onnxruntime-gpu must be listed under ONNX Runtime (MIT), not an NVIDIA license"
        );
    }
}
