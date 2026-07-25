//! Host capability gates — the ONE spelling of the COMPOUND platform predicates.
//!
//! Two facts decide every compute rung, download target, and dispatcher arm, and each used
//! to be re-asked per call site in a different dialect: `os == "macos" && arch == "aarch64"`
//! in the config ladder, `cfg!(all(target_os = "macos", target_arch = "aarch64"))` in the
//! download matrix, `#[cfg(...)]` at the fetch sites. Three dialects of one fact is how #211
//! (Fluid resolving on Intel) and #250 (Core ML resolving on Intel) each drifted a single arm
//! without the others noticing.
//!
//! [`Os`] and [`Arch`] are the currency, not `&str`: a mistyped `"macoss"` in a matrix table
//! is a silent `Other` that keeps the test green, whereas a mistyped variant does not
//! compile. Strings cross into enums exactly once, at [`Os::from_token`]/[`Arch::from_token`].
//!
//! [`Os::this`] and [`Arch::this`] read `std::env::consts`, which reports the COMPILE target,
//! so `apple_silicon(Os::this(), Arch::this())` and `cfg!(all(target_os = "macos",
//! target_arch = "aarch64"))` are the same answer; the `gates_match_their_cfg_spelling` test
//! pins that, so a `#[cfg]` attribute elsewhere can cite a gate here instead of restating
//! its terms.
//!
//! Use `#[cfg]` only where the guarded code must not COMPILE off-platform (it names a
//! symbol that exists nowhere else); everything else takes the boolean.

use std::env::consts;

/// An OS DontSpeak ships for. [`Os::Other`] covers every target that reaches a gate without
/// a published build, and no gate ever admits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// `macos`.
    MacOs,
    /// `windows`.
    Windows,
    /// `linux`.
    Linux,
    Other,
}

impl Os {
    /// Map a [`std::env::consts::OS`] token.
    pub fn from_token(token: &str) -> Self {
        match token {
            "macos" => Os::MacOs,
            "windows" => Os::Windows,
            "linux" => Os::Linux,
            _ => Os::Other,
        }
    }

    /// The OS this binary was built for.
    pub fn this() -> Self {
        Self::from_token(consts::OS)
    }
}

/// A CPU architecture DontSpeak ships for. [`Arch::Other`] is treated like [`Os::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// `aarch64` — Apple Silicon, Windows on ARM, arm64 Linux.
    Arm64,
    /// `x86_64`.
    X64,
    Other,
}

impl Arch {
    /// Map a [`std::env::consts::ARCH`] token.
    pub fn from_token(token: &str) -> Self {
        match token {
            "aarch64" => Arch::Arm64,
            "x86_64" => Arch::X64,
            _ => Arch::Other,
        }
    }

    /// The architecture this binary was built for.
    pub fn this() -> Self {
        Self::from_token(consts::ARCH)
    }
}

/// Apple Silicon macOS: MLX, FluidAudio's Core ML/ANE chains, and ONNX Runtime's Core ML
/// execution provider. The Neural Engine ships only here, the native shim exports its
/// `ds_fluid_*`/MLX symbols only here, and on Intel the ORT Core ML EP registers and then
/// fails every `Session::run` — a run-path failure no load-time fallback catches (#250).
/// The arch half is the whole point: plain-ONNX macOS code such as the SepFormer
/// speaker-lock needs the OS but not the Neural Engine, and Intel Macs still get it.
pub fn apple_silicon(os: Os, arch: Arch) -> bool {
    os == Os::MacOs && arch == Arch::Arm64
}

/// x86_64 Windows/Linux: the only hosts the bundled ONNX Runtime CUDA wheels publish for.
/// arm64 Windows/Linux resolve no CUDA rung because no runtime exists to download.
pub fn cuda_host(os: Os, arch: Arch) -> bool {
    matches!(os, Os::Windows | Os::Linux) && arch == Arch::X64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The matrix as literal variants, so a widened gate fails on every platform's CI leg
    /// rather than only on the host that regressed.
    #[test]
    fn platform_matrix() {
        for (os, arch, apple, cuda) in [
            (Os::MacOs, Arch::Arm64, true, false),
            (Os::MacOs, Arch::X64, false, false),
            (Os::Windows, Arch::X64, false, true),
            (Os::Windows, Arch::Arm64, false, false),
            (Os::Linux, Arch::X64, false, true),
            (Os::Linux, Arch::Arm64, false, false),
            // Unshipped targets reach gates through `from_token`; none may admit them.
            (Os::Other, Arch::X64, false, false),
            (Os::MacOs, Arch::Other, false, false),
            (Os::Other, Arch::Other, false, false),
        ] {
            assert_eq!(
                apple_silicon(os, arch),
                apple,
                "apple_silicon {os:?}/{arch:?}"
            );
            assert_eq!(cuda_host(os, arch), cuda, "cuda_host {os:?}/{arch:?}");
        }
    }

    #[test]
    fn tokens_map_to_variants_and_unknowns_fall_to_other() {
        assert_eq!(Os::from_token("macos"), Os::MacOs);
        assert_eq!(Os::from_token("windows"), Os::Windows);
        assert_eq!(Os::from_token("linux"), Os::Linux);
        assert_eq!(Os::from_token("freebsd"), Os::Other);
        assert_eq!(Arch::from_token("aarch64"), Arch::Arm64);
        assert_eq!(Arch::from_token("x86_64"), Arch::X64);
        assert_eq!(Arch::from_token("riscv64"), Arch::Other);
    }

    /// The equivalence that lets a `#[cfg]` attribute cite a gate rather than restate it.
    #[test]
    fn gates_match_their_cfg_spelling() {
        assert_eq!(
            apple_silicon(Os::this(), Arch::this()),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
        assert_eq!(
            cuda_host(Os::this(), Arch::this()),
            cfg!(all(
                any(target_os = "windows", target_os = "linux"),
                target_arch = "x86_64"
            ))
        );
    }
}
