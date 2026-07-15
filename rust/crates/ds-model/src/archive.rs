//! Archive extraction: pull the onnxruntime shared library out of Microsoft's
//! `.tgz` (macOS/Linux) / `.zip` (Windows), and flatten the CUDA wheels' DLLs.
//! Atomic temp + rename so a partial extract never lands.

use std::io::{Read, Write};
use std::path::Path;

const MAX_EXTRACTED_MEMBER_BYTES: u64 = 1024 * 1024 * 1024;
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
const MAX_EXTRACTED_TOTAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
const MAX_EXTRACTED_MEMBERS: u32 = 512;

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn add_extracted_bytes(total: &mut u64, copied: u64) -> std::io::Result<()> {
    add_extracted_bytes_with_limit(total, copied, MAX_EXTRACTED_TOTAL_BYTES)
}

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    target_arch = "x86_64"
))]
fn add_extracted_bytes_with_limit(total: &mut u64, copied: u64, limit: u64) -> std::io::Result<()> {
    *total = total.checked_add(copied).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "archive extraction size overflow",
        )
    })?;
    if *total > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "archive exceeds total extraction size limit",
        ));
    }
    Ok(())
}

fn copy_bounded(mut src: impl Read, mut dest: impl Write) -> std::io::Result<u64> {
    copy_bounded_with_limit(&mut src, &mut dest, MAX_EXTRACTED_MEMBER_BYTES)
}

fn copy_bounded_with_limit(
    mut src: impl Read,
    mut dest: impl Write,
    limit: u64,
) -> std::io::Result<u64> {
    let copied = std::io::copy(&mut src.by_ref().take(limit.saturating_add(1)), &mut dest)?;
    if copied > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "archive member exceeds extraction size limit",
        ));
    }
    Ok(copied)
}

/// Extract the onnxruntime shared library from the downloaded archive onto
/// `dest`. Per-platform because Microsoft ships a `.tgz` on macOS and a `.zip`
/// on Windows.
#[cfg(target_os = "windows")]
pub(crate) fn extract_runtime_member(zip_path: &Path, dest: &Path) -> std::io::Result<()> {
    // The win-x64 .zip holds lib/onnxruntime.dll (the runtime we want) AND
    // lib/onnxruntime_providers_shared.dll — match the base name exactly so we
    // pull only onnxruntime.dll. Atomic temp + rename onto `dest`.
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let dir = dest
        .parent()
        .ok_or_else(|| std::io::Error::other("dest has no parent"))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(std::io::Error::other)?;
        if !entry.is_file() {
            continue;
        }
        let is_runtime = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.eq_ignore_ascii_case("onnxruntime.dll"))
            .unwrap_or(false);
        if is_runtime {
            let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
            copy_bounded(&mut entry, &mut tmp)?;
            tmp.flush()?;
            tmp.persist(dest).map_err(|e| e.error)?;
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no onnxruntime.dll member in the onnxruntime .zip",
    ))
}

/// Non-Windows: the archive is a gzip'd tar; pull the dylib/.so member.
#[cfg(not(target_os = "windows"))]
pub(crate) fn extract_runtime_member(tgz_path: &Path, dest: &Path) -> std::io::Result<()> {
    extract_dylib_member(tgz_path, dest)
}

/// Extract the FIRST `*.dylib`/`*.so`/`*.dll` member of a gzip'd tar onto
/// `dest` (atomic temp + rename). Mirrors `ort-sys`'s own dylib-copy heuristic
/// (it scans for entries whose path contains `.dll`/`.so`/`.dylib`).
/// `#[allow(dead_code)]`: on Windows the zip path above is used instead, but the
/// parity test still exercises this function.
#[allow(dead_code)]
pub(crate) fn extract_dylib_member(tgz_path: &Path, dest: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(tgz_path)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);
    let dir = dest
        .parent()
        .ok_or_else(|| std::io::Error::other("dest has no parent"))?;
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Own the path first so the mutable `entry` borrow below (io::copy) is OK.
        let path = entry.path()?.into_owned();
        // Pick the real shared library: a regular FILE whose name ends in the
        // platform ext. Exclude debug bundles (a .dSYM dir path also contains
        // ".dylib") and the unversioned symlink (not a regular file). E.g. the
        // Microsoft archive holds lib/libonnxruntime.1.22.0.dylib (real),
        // lib/libonnxruntime.dylib (symlink), and a .dSYM bundle — only the first
        // must be extracted.
        let is_regular = entry.header().entry_type().is_file();
        // Match the CORE runtime only. macOS ships lib/libonnxruntime.1.27.0.dylib; Linux ships
        // lib/libonnxruntime.so.1.27.0 (a versioned soname — note it does NOT end in ".so", and
        // the bare libonnxruntime.so / .so.1 are SYMLINKS excluded by `is_regular`). Anchoring on
        // the "libonnxruntime." prefix (DOT, not the "_" in libonnxruntime_providers_shared.so)
        // excludes the provider shim that also ships in the linux tgz's lib/.
        let name_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| {
                n.starts_with("libonnxruntime.")
                    && (n.ends_with(".dylib")
                        || n.ends_with(".dll")
                        || n.ends_with(".so")
                        || n.contains(".so."))
            })
            .unwrap_or(false);
        let in_debug_bundle = path.to_string_lossy().contains(".dSYM");
        if is_regular && name_matches && !in_debug_bundle {
            let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
            copy_bounded(&mut entry, &mut tmp)?;
            tmp.flush()?;
            tmp.persist(dest).map_err(|e| e.error)?;
            return Ok(());
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no libonnxruntime dylib member in the onnxruntime .tgz",
    ))
}

/// Extract EVERY `*.dll` member of a wheel (zip) into `dir`, flattened (atomic per
/// file). The nvidia wheels hold their DLLs under `nvidia/<lib>/bin/`; the
/// onnxruntime wheel under `onnxruntime/capi/` — flattening collects exactly the
/// runtime set.
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
pub(crate) fn extract_all_dlls(zip_path: &Path, dir: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let mut count = 0u32;
    let mut extracted_bytes = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(std::io::Error::other)?;
        if !entry.is_file() {
            continue;
        }
        let Some(base) = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !base.to_ascii_lowercase().ends_with(".dll") {
            continue;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        if count >= MAX_EXTRACTED_MEMBERS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "archive contains too many DLL members",
            ));
        }
        let copied = copy_bounded(&mut entry, &mut tmp)?;
        add_extracted_bytes(&mut extracted_bytes, copied)?;
        tmp.flush()?;
        tmp.persist(dir.join(&base)).map_err(|e| e.error)?;
        count += 1;
    }
    if count == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no .dll members in wheel",
        ));
    }
    Ok(())
}

/// Linux analogue of [`extract_all_dlls`]: extract EVERY shared object (`*.so` / versioned
/// `*.so.N`) from a wheel (zip) into `dir`, flattened. The nvidia wheels hold their libs under
/// `nvidia/<lib>/lib/lib*.so.NN`; the onnxruntime_gpu wheel under `onnxruntime/capi/lib*.so` —
/// flattening into one dir collects the runtime set so the CUDA provider and its deps are all
/// siblings (resolved via the RTLD_GLOBAL preload in `ort::ensure_ort_dylib_gpu`).
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) fn extract_all_sos(zip_path: &Path, dir: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let mut count = 0u32;
    let mut extracted_bytes = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(std::io::Error::other)?;
        if !entry.is_file() {
            continue;
        }
        let Some(base) = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // A shared object: ends in ".so" or carries a versioned soname (".so.12", ".so.9.5.1").
        if !(base.ends_with(".so") || base.contains(".so.")) {
            continue;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        if count >= MAX_EXTRACTED_MEMBERS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "archive contains too many shared-object members",
            ));
        }
        let copied = copy_bounded(&mut entry, &mut tmp)?;
        add_extracted_bytes(&mut extracted_bytes, copied)?;
        tmp.flush()?;
        tmp.persist(dir.join(&base)).map_err(|e| e.error)?;
        count += 1;
    }
    if count == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no .so members in wheel",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tgz(path: &Path, members: &[(&str, &[u8])]) {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let file = std::fs::File::create(path).unwrap();
        let encoder = GzEncoder::new(file, Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, payload) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, &payload[..])
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();
    }

    /// Build a tiny gzip'd tar with a fake `libonnxruntime.dylib` member and
    /// assert `extract_dylib_member` pulls exactly that member — no network, no
    /// real ORT.
    #[test]
    fn extract_dylib_member_pulls_the_dylib() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let payload = b"FAKE-ONNXRUNTIME-DYLIB-BYTES";
        // Put a misleading `.source.txt` member before the dylib. A substring match for
        // `.so` would extract it instead of the shared library.
        let mut tar_buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_buf);
            let mut readme = tar::Header::new_gnu();
            readme.set_size(5);
            readme.set_mode(0o644);
            readme.set_cksum();
            builder
                .append_data(&mut readme, "onnxruntime-1.22.0/README", &b"hello"[..])
                .unwrap();
            let decoy_payload = b"NOT-A-SHARED-LIBRARY";
            let mut decoy = tar::Header::new_gnu();
            decoy.set_size(decoy_payload.len() as u64);
            decoy.set_mode(0o644);
            decoy.set_cksum();
            builder
                .append_data(
                    &mut decoy,
                    "onnxruntime-1.22.0/lib/libonnxruntime.source.txt",
                    &decoy_payload[..],
                )
                .unwrap();
            let mut dylib = tar::Header::new_gnu();
            dylib.set_size(payload.len() as u64);
            dylib.set_mode(0o755);
            dylib.set_cksum();
            builder
                .append_data(
                    &mut dylib,
                    "onnxruntime-1.22.0/lib/libonnxruntime.1.22.0.dylib",
                    &payload[..],
                )
                .unwrap();
            builder.finish().unwrap();
        }
        // gzip it.
        let mut gz = Vec::new();
        {
            let mut enc = GzEncoder::new(&mut gz, Compression::fast());
            enc.write_all(&tar_buf).unwrap();
            enc.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let tgz_path = dir.path().join("ort.tgz");
        std::fs::write(&tgz_path, &gz).unwrap();
        let dest = dir.path().join("libonnxruntime.dylib");
        extract_dylib_member(&tgz_path, &dest).expect("extracts the dylib member");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    #[test]
    fn copy_bounded_rejects_a_member_over_the_limit() {
        let mut output = Vec::new();
        let error = copy_bounded_with_limit(&b"12345"[..], &mut output, 4).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(output, b"12345");
    }

    #[cfg(all(
        any(target_os = "windows", target_os = "linux"),
        target_arch = "x86_64"
    ))]
    #[test]
    fn extracted_total_rejects_limit_and_integer_overflow() {
        let mut total = 3;
        let error = add_extracted_bytes_with_limit(&mut total, 2, 4).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let mut total = u64::MAX;
        let error = add_extracted_bytes_with_limit(&mut total, 1, u64::MAX).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn provider_only_archive_is_rejected_without_touching_destination() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("provider-only.tgz");
        write_tgz(
            &archive,
            &[("lib/libonnxruntime_providers_shared.so", b"provider")],
        );
        let dest = dir.path().join("libonnxruntime.so");
        std::fs::write(&dest, b"existing").unwrap();

        let error = extract_dylib_member(&archive, &dest).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&dest).unwrap(), b"existing");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_runtime_zip_selects_core_dll_not_provider_shim() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("runtime.zip");
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive
            .start_file("lib/onnxruntime_providers_shared.dll", options)
            .unwrap();
        archive.write_all(b"provider").unwrap();
        archive.start_file("lib/onnxruntime.dll", options).unwrap();
        archive.write_all(b"runtime").unwrap();
        archive.finish().unwrap();

        let dest = dir.path().join("onnxruntime.dll");
        extract_runtime_member(&archive_path, &dest).unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"runtime");
    }
}
