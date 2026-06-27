//! Libraries catalog — collects every third-party project the app DOWNLOADS at runtime
//! (the Kokoro + Parakeet models, the ONNX Runtime, and the optional CUDA/cuDNN GPU
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

/// The cross-platform libraries catalog for the UI's Libraries tab: a JSON array of
/// `{name, usage, homepage, license, license_url, files:[{name, url, size_bytes?}]}`,
/// in display order — system libraries (CUDA, cuDNN) → runtime (ONNX Runtime) → models
/// (Kokoro, Parakeet), lowest-level first. Built entirely from the [`crate::urls`]
/// registry, so it can never drift from what's actually fetched.
pub fn catalog() -> Value {
    let mut projects: Vec<Value> = Vec::new();

    // Build the runtime/system-library file lists up front so they can be emitted ahead
    // of the models. ONNX Runtime — the load-dynamic dist archive (platform-selected;
    // `None` on the platforms with no pinned dist), plus the GPU build wheel on Windows
    // x64 (split out of CUDA_WHEELS below, since onnxruntime-gpu is MIT, not NVIDIA).
    let mut onnx_files: Vec<Value> = Vec::new();
    if let Some(d) = crate::ort::onnxruntime_dist() {
        onnx_files.push(file_obj(
            url_basename(d.url),
            d.url,
            urls::ONNXRUNTIME_DIST_SIZE_BYTES,
        ));
    }

    // Optional CUDA / cuDNN GPU runtime (Windows + Linux x64). Split the pinned wheels by
    // license: onnxruntime-gpu → ONNX Runtime (MIT); cuDNN → its own SLA; everything else
    // → the CUDA Toolkit EULA. The wheels carry no size in the registry, so none is shown.
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
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

    // System libraries first (CUDA, then cuDNN), lowest-level before the runtime that uses them.
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
    {
        if !cuda_files.is_empty() {
            projects.push(project_obj(&urls::NVIDIA_CUDA, cuda_files));
        }
        if !cudnn_files.is_empty() {
            projects.push(project_obj(&urls::NVIDIA_CUDNN, cudnn_files));
        }
    }

    // Then the runtime.
    if !onnx_files.is_empty() {
        projects.push(project_obj(&urls::ONNX_RUNTIME, onnx_files));
    }

    // Finally the models — each file (and its license) comes straight from the registry profile.
    for p in [urls::KOKORO, urls::PARAKEET] {
        let files = p.files.iter().map(download_file).collect();
        projects.push(project_obj(&p, files));
    }

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
            assert!(
                p["files"].as_array().is_some_and(|f| !f.is_empty()),
                "project `{}` has no files",
                p["name"]
            );
        }
    }

    /// Every MODEL download in the registry (Kokoro + Parakeet, which are present on every
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
        for d in [
            urls::KOKORO_ONNX,
            urls::KOKORO_VOICES,
            urls::PARAKEET_ENCODER,
            urls::PARAKEET_DECODER,
            urls::PARAKEET_PREPROC,
            urls::PARAKEET_VOCAB,
        ] {
            assert!(
                names.iter().any(|n| n == d.file_name),
                "download `{}` is missing from the libraries catalog",
                d.file_name
            );
        }
    }

    /// On Windows/Linux x64 every pinned CUDA wheel must be bucketed into a project (no wheel
    /// left license-less), and the cuDNN wheel must land under the cuDNN SLA, not the CUDA EULA.
    #[cfg(all(any(target_os = "windows", target_os = "linux"), target_arch = "x86_64"))]
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
