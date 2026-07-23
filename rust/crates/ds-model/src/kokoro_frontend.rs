//! Downloaded native assets for Kokoro's multilingual text frontends.

use std::path::{Path, PathBuf};

use crate::archive::extract_wheel_subtree;
use crate::download::ensure_in_dir;
use crate::spec::ModelSpec;

pub(crate) const COMPLETE_MARKER: &str = ".complete";
pub(crate) const ESPEAK_DIR_NAME: &str = "espeakng-loader-0.2.4";

#[derive(Clone, Copy)]
pub(crate) struct FrontendDist {
    pub(crate) url: &'static str,
    pub(crate) archive_sha256: &'static str,
    pub(crate) size_bytes: u64,
}

pub(crate) fn espeak_dist() -> Option<FrontendDist> {
    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    ))]
    {
        let download = crate::urls::ESPEAKNG_LOADER;
        Some(FrontendDist {
            url: download.url,
            archive_sha256: download.sha256,
            size_bytes: download.size_bytes,
        })
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        None
    }
}

fn frontend_dir(name: &str) -> Option<PathBuf> {
    Some(ds_config::model_dir()?.join(name))
}

/// Root-relative form of [`espeak_root_dir`], for callers that own their model root.
pub(crate) fn espeak_dir_under(root: &Path) -> PathBuf {
    root.join(ESPEAK_DIR_NAME)
}

pub fn espeak_root_dir() -> Option<PathBuf> {
    frontend_dir(ESPEAK_DIR_NAME)
}

pub fn espeak_library_path() -> Option<PathBuf> {
    let name = if cfg!(target_os = "windows") {
        "espeak-ng.dll"
    } else if cfg!(target_os = "macos") {
        "libespeak-ng.dylib"
    } else {
        "libespeak-ng.so"
    };
    Some(espeak_root_dir()?.join(name))
}

fn marker_matches(dir: &Path, sha256: &str) -> bool {
    std::fs::read_to_string(dir.join(COMPLETE_MARKER))
        .map(|marker| marker.trim() == sha256)
        .unwrap_or(false)
}

fn espeak_payload_present(dir: &Path) -> bool {
    let library = if cfg!(target_os = "windows") {
        "espeak-ng.dll"
    } else if cfg!(target_os = "macos") {
        "libespeak-ng.dylib"
    } else {
        "libespeak-ng.so"
    };
    let data = dir.join("espeak-ng-data");
    dir.join(library).is_file()
        && [
            "phondata",
            "phonindex",
            "phontab",
            "es_dict",
            "fr_dict",
            "hi_dict",
            "it_dict",
            "pt_dict",
        ]
        .iter()
        .all(|file| data.join(file).is_file())
}

pub fn is_espeak_loader_present() -> bool {
    espeak_dist().is_some_and(|dist| {
        espeak_root_dir().is_some_and(|dir| {
            espeak_payload_present(&dir) && marker_matches(&dir, dist.archive_sha256)
        })
    })
}

fn extract_espeak(archive: &Path, dest: &Path) -> std::io::Result<()> {
    extract_wheel_subtree(archive, Path::new("espeakng_loader"), dest)
}

fn ensure_distribution(
    final_dir: &Path,
    dist: FrontendDist,
    payload_present: fn(&Path) -> bool,
    extract: fn(&Path, &Path) -> std::io::Result<()>,
    label: &str,
    progress: &dyn Fn(u64, u64),
) -> std::io::Result<PathBuf> {
    crate::download::with_destination_flight(final_dir, |parent| {
        if payload_present(final_dir) && marker_matches(final_dir, dist.archive_sha256) {
            return Ok(final_dir.to_path_buf());
        }

        let archive_dir = tempfile::tempdir_in(parent)?;
        let archive_spec = ModelSpec {
            file_name: "frontend.archive".to_string(),
            url: dist.url.to_string(),
            sha256: dist.archive_sha256.to_string(),
        };
        let archive = ensure_in_dir(archive_dir.path(), &archive_spec, progress)?;
        let staging = tempfile::tempdir_in(parent)?;
        let staged = staging.path().join("payload");
        std::fs::create_dir(&staged)?;
        extract(&archive, &staged)?;
        if !payload_present(&staged) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{label} archive is missing required files"),
            ));
        }
        if final_dir.exists() {
            std::fs::remove_dir_all(final_dir)?;
        }
        std::fs::rename(&staged, final_dir)?;
        std::fs::write(final_dir.join(COMPLETE_MARKER), dist.archive_sha256)?;
        Ok(final_dir.to_path_buf())
    })
}

pub fn ensure_espeak_loader_with_progress(progress: &dyn Fn(u64, u64)) -> std::io::Result<PathBuf> {
    let dist = espeak_dist().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no pinned espeakng-loader distribution for this platform",
        )
    })?;
    let dir = espeak_root_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve model_dir()")
    })?;
    ensure_distribution(
        &dir,
        dist,
        espeak_payload_present,
        extract_espeak,
        "espeakng-loader",
        progress,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_checks_require_the_complete_layout() {
        let root = tempfile::tempdir().unwrap();
        assert!(!espeak_payload_present(root.path()));
    }

    #[test]
    fn platform_distributions_are_checksum_pinned() {
        if let Some(dist) = espeak_dist() {
            assert_eq!(dist.archive_sha256.len(), 64);
            assert!(dist.url.contains("espeakng_loader-0.2.4"));
        }
    }
}
