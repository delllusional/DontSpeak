//! Host capability gates — one spelling of compound platform predicates.
//!
//! Compute rungs, download targets, and dispatchers used to restate the same fact in three
//! dialects (`os == "macos" && …`, `cfg!(…)`, `#[cfg]`), which is how #211 and #250 each
//! drifted a single arm. [`Os`]/[`Arch`] (not `&str`) are the currency: a typo is a compile
//! error, not silent `Other`. Tokens cross the boundary once via `from_token`.
//!
//! [`Os::this`]/[`Arch::this`] are the compile target (`std::env::consts`);
//! `gates_match_their_cfg_spelling` pins equality with the matching `cfg!`. Use `#[cfg]` only
//! when code must not compile off-platform; everything else takes the boolean.

use std::env::consts;

/// Shipped OS. [`Os::Other`] reaches gates on unshipped targets; no gate admits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Windows,
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

    pub fn this() -> Self {
        Self::from_token(consts::OS)
    }
}

/// Shipped CPU arch. [`Arch::Other`] is treated like [`Os::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// `aarch64`.
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

    pub fn this() -> Self {
        Self::from_token(consts::ARCH)
    }
}

/// Apple Silicon macOS: MLX, FluidAudio ANE, ORT Core ML EP. Neural Engine + native shim
/// symbols only here; Intel ORT Core ML EP registers then fails every `Session::run` (#250).
/// Plain-ONNX macOS (e.g. SepFormer) needs OS only — Intel Macs still get it.
pub fn apple_silicon(os: Os, arch: Arch) -> bool {
    os == Os::MacOs && arch == Arch::Arm64
}

/// x86_64 Windows/Linux only — hosts the bundled ORT CUDA wheels publish for.
pub fn cuda_host(os: Os, arch: Arch) -> bool {
    matches!(os, Os::Windows | Os::Linux) && arch == Arch::X64
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Literal matrix so a widened gate fails on every CI leg.
    #[test]
    fn platform_matrix() {
        for (os, arch, apple, cuda) in [
            (Os::MacOs, Arch::Arm64, true, false),
            (Os::MacOs, Arch::X64, false, false),
            (Os::Windows, Arch::X64, false, true),
            (Os::Windows, Arch::Arm64, false, false),
            (Os::Linux, Arch::X64, false, true),
            (Os::Linux, Arch::Arm64, false, false),
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

    /// Gates equal their `cfg!` spelling so attributes can cite them.
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
