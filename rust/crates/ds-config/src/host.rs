//! Host capability gates — the ONE spelling of "can this backend or asset exist here".
//!
//! Three questions decide every compute rung, download target, and dispatcher arm, and each
//! used to be re-asked per call site in a different dialect: `os == "macos" && arch ==
//! "aarch64"` in the config ladder, `cfg!(all(target_os = "macos", target_arch = "aarch64"))`
//! in the download matrix, `#[cfg(...)]` at the fetch sites. Three dialects of one fact is
//! how #211 (Fluid resolving on Intel) and #250 (Core ML resolving on Intel) each drifted a
//! single arm without the others noticing.
//!
//! Each gate comes in two forms: a pure `(os, arch)` predicate that cross-platform matrix
//! tests can drive with literals, and an `is_*` wrapper that answers for the running host.
//! `std::env::consts` reports the COMPILE target, so `is_apple_silicon()` and
//! `cfg!(all(target_os = "macos", target_arch = "aarch64"))` are the same answer — the
//! `gates_match_their_cfg_spelling` test pins that, so a `#[cfg]` attribute elsewhere can
//! cite a gate here instead of restating its terms.
//!
//! Use `#[cfg]` only where the guarded code must not COMPILE off-platform (it names a
//! symbol that exists nowhere else); everything else takes the boolean.

use std::env::consts;

/// Apple Silicon macOS: MLX, FluidAudio's Core ML/ANE chains, and ONNX Runtime's Core ML
/// execution provider. The Neural Engine ships only here, the native shim exports its
/// `ds_fluid_*`/MLX symbols only here, and on Intel the ORT Core ML EP registers and then
/// fails every `Session::run` — a run-path failure no load-time fallback catches (#250).
pub fn apple_silicon(os: &str, arch: &str) -> bool {
    os == "macos" && arch == "aarch64"
}

/// [`apple_silicon`] for the running host.
pub fn is_apple_silicon() -> bool {
    apple_silicon(consts::OS, consts::ARCH)
}

/// macOS on any arch: plain-ONNX macOS code such as the SepFormer speaker-lock, which needs
/// the OS but not the Neural Engine. The split from [`apple_silicon`] is the whole point —
/// it is what Intel Macs still get.
pub fn macos(os: &str) -> bool {
    os == "macos"
}

/// [`macos`] for the running host.
pub fn is_macos() -> bool {
    macos(consts::OS)
}

/// x86_64 Windows/Linux: the only hosts the bundled ONNX Runtime CUDA wheels publish for.
/// aarch64 Windows/Linux resolve no CUDA rung because no runtime exists to download.
pub fn cuda_host(os: &str, arch: &str) -> bool {
    matches!(os, "windows" | "linux") && arch == "x86_64"
}

/// [`cuda_host`] for the running host.
pub fn is_cuda_host() -> bool {
    cuda_host(consts::OS, consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix as LITERALS, so a widened gate fails on every platform's CI leg rather
    /// than only on the host that regressed.
    #[test]
    fn platform_matrix() {
        for (os, arch, apple, mac, cuda) in [
            ("macos", "aarch64", true, true, false),
            ("macos", "x86_64", false, true, false),
            ("windows", "x86_64", false, false, true),
            ("windows", "aarch64", false, false, false),
            ("linux", "x86_64", false, false, true),
            ("linux", "aarch64", false, false, false),
        ] {
            assert_eq!(apple_silicon(os, arch), apple, "apple_silicon {os}/{arch}");
            assert_eq!(macos(os), mac, "macos {os}/{arch}");
            assert_eq!(cuda_host(os, arch), cuda, "cuda_host {os}/{arch}");
        }
    }

    /// The equivalence that lets a `#[cfg]` attribute cite a gate rather than restate it.
    #[test]
    fn gates_match_their_cfg_spelling() {
        assert_eq!(
            is_apple_silicon(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        assert_eq!(is_macos(), cfg!(target_os = "macos"));
        assert_eq!(
            is_cuda_host(),
            cfg!(all(
                any(target_os = "windows", target_os = "linux"),
                target_arch = "x86_64"
            ))
        );
    }
}
