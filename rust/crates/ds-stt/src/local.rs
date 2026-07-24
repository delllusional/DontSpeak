//! Local on-device STT selector. One type so the warm helper holds whichever
//! `stt_engine` was selected. Parakeet over `ort` and — on macOS — the same model through
//! MLX Audio. Both are selectable.

use std::path::PathBuf;

use crate::parakeet::ParakeetTranscriber;

/// MLX Audio shim present (`DONTSPEAK_MLX_DYLIB_PATH`)? Gates `mlx` so a
/// missing shim falls back to ONNX. Engine checks the SAME env
/// (`mlx_shim_available`); helper inherits it so status row stays honest.
#[cfg(target_os = "macos")]
fn shim_available() -> bool {
    std::env::var_os("DONTSPEAK_MLX_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).exists())
        .unwrap_or(false)
}

/// Active local transcriber. Same lazy-load surface as [`ParakeetTranscriber`].
pub enum LocalTranscriber {
    /// Parakeet transducer over `ort` (`cpu`/`cuda` provider). Boxed: avoids clippy
    /// `large_enum_variant` vs the smaller native arms.
    ParakeetOnnx(Box<ParakeetTranscriber>),
    /// macOS MLX Audio (`mlx` provider).
    #[cfg(target_os = "macos")]
    Mlx(crate::mlx::MlxTranscriber),
    /// macOS FluidAudio Core ML / ANE (`fluid` provider). Apple Silicon only — the Intel
    /// shim compiles `shim.swift` alone and exports no `ds_fluid_*` symbols (#211).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    Fluid(crate::fluid::FluidTranscriber),
    /// macOS `SFSpeechRecognizer` (`system` engine) — DIFFERENT engine, not a Parakeet
    /// runtime; selected by the `"system"` provider token.
    #[cfg(target_os = "macos")]
    System(crate::sysspeech::SystemTranscriber),
}

/// Which local backend a resolved provider selects, given shim availability. Separated from
/// construction (which is dlopen-free but per-arch) so the shim-vs-fall-through decision is
/// unit-testable without touching the ANE or a real dlopen.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Onnx,
    Mlx,
    /// Apple Silicon only — the Intel shim exports no `ds_fluid_*` (#211).
    #[cfg(target_arch = "aarch64")]
    Fluid,
    System,
}

/// Pure selection: `"system"` → System; `"fluid"` → Fluid on Apple Silicon with the shim;
/// `"mlx"` → MLX with the shim; anything else (incl. shim-absent `mlx`/`fluid`, and every
/// `fluid` on Intel) → ONNX. `shim` is the live `DONTSPEAK_MLX_DYLIB_PATH` probe.
#[cfg(target_os = "macos")]
fn select_backend(provider: &str, shim: bool) -> Backend {
    if provider.eq_ignore_ascii_case("system") {
        return Backend::System;
    }
    // Fluid is Apple-Silicon-only: on Intel the shim has no `ds_fluid_*`, so `fluid` must fall
    // through to ONNX rather than dlsym-fail (the #211 arch trap). Gated, not merely runtime.
    #[cfg(target_arch = "aarch64")]
    if provider.eq_ignore_ascii_case("fluid") && shim {
        return Backend::Fluid;
    }
    if provider.eq_ignore_ascii_case("mlx") && shim {
        return Backend::Mlx;
    }
    Backend::Onnx
}

impl LocalTranscriber {
    /// Backend by RESOLVED provider token. `"system"` → `SFSpeechRecognizer`;
    /// `"fluid"` → FluidAudio Core ML on Apple Silicon when the shim is present; `"mlx"` →
    /// native MLX when shim present; anything else (incl. `"cpu"`) → portable ONNX Parakeet.
    /// Shim-aware so the status row stays honest.
    pub fn for_provider(provider: &str, parakeet_dir: PathBuf) -> Self {
        #[cfg(target_os = "macos")]
        {
            match select_backend(provider, shim_available()) {
                Backend::System => {
                    return LocalTranscriber::System(crate::sysspeech::SystemTranscriber::new());
                }
                #[cfg(target_arch = "aarch64")]
                Backend::Fluid => {
                    return LocalTranscriber::Fluid(crate::fluid::FluidTranscriber::new());
                }
                Backend::Mlx => {
                    return LocalTranscriber::Mlx(crate::mlx::MlxTranscriber::new());
                }
                Backend::Onnx => {}
            }
        }
        let _ = provider;
        LocalTranscriber::ParakeetOnnx(Box::new(ParakeetTranscriber::for_provider(
            parakeet_dir,
            provider,
        )))
    }

    /// Realized runtime in the SAME token vocabulary Kokoro TTS reports via `PROVIDER`
    /// — STT status maps through the ONE shared `realized_ort_token` path (no drift).
    pub fn provider(&self) -> ds_config::RealizedProvider {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.provider(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(_) => ds_config::RealizedProvider::Mlx,
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(_) => ds_config::RealizedProvider::Fluid,
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => ds_config::RealizedProvider::System,
        }
    }

    pub fn preload(&mut self) -> Result<(), String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(c) => c.preload(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.preload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.preload(),
        }?;
        // Warm the graph: first real transcribe compiles ONNX / MLX / Core ML — throwaway pass
        // now so first dictation doesn't pay it. SKIP System: no graph, rejects synthetic.
        // Best-effort: warmup hiccup must not fail an otherwise-successful load.
        let skip_warmup = match self {
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(_) => true,
            _ => false,
        };
        if !skip_warmup {
            self.transcribe_pcm_16k(&warmup_audio())?;
        }
        Ok(())
    }

    pub fn unload(&mut self) -> bool {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.unload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(c) => c.unload(),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.unload(),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.unload(),
        }
    }

    pub fn transcribe_pcm_16k(&mut self, pcm: &[f32]) -> Result<String, String> {
        match self {
            LocalTranscriber::ParakeetOnnx(m) => m.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::Mlx(c) => c.transcribe_pcm_16k(pcm),
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            LocalTranscriber::Fluid(c) => c.transcribe_pcm_16k(pcm),
            #[cfg(target_os = "macos")]
            LocalTranscriber::System(s) => s.transcribe_pcm_16k(pcm),
        }
    }
}

/// ~0.5s @ 16 kHz quiet 440 Hz — warmup for [`LocalTranscriber::preload`].
/// Non-silence ensures the warmup executes a real forward pass. Transcript discarded.
fn warmup_audio() -> Vec<f32> {
    use std::f32::consts::PI;
    (0..8_000)
        .map(|i| 0.02 * (i as f32 * 2.0 * PI * 440.0 / 16_000.0).sin())
        .collect()
}

#[cfg(target_os = "macos")]
#[cfg(test)]
mod tests {
    use super::{Backend, select_backend};

    /// `fluid` resolves to the Core ML backend ONLY where the shim can export `ds_fluid_*` —
    /// Apple Silicon with the bundled dylib present. Everywhere else it falls through to ONNX.
    /// Pure selection, no dlopen: constructing a variant never touches the ANE.
    #[test]
    fn for_provider_selects_the_fluid_backend_only_with_the_shim() {
        #[cfg(target_arch = "aarch64")]
        {
            // Apple Silicon: shim present → Fluid; shim absent → ONNX (never dlsym-fail).
            assert_eq!(select_backend("fluid", true), Backend::Fluid);
            assert_eq!(select_backend("FLUID", true), Backend::Fluid);
            assert_eq!(select_backend("fluid", false), Backend::Onnx);
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            // Intel macOS: the shim is System-STT-only (shim.swift alone, no `ds_fluid_*`), so
            // `fluid` MUST fall through to ONNX even when a shim path is present — the #211
            // arch trap the aarch64 gate on the `fluid` arm exists to close.
            assert_eq!(select_backend("fluid", true), Backend::Onnx);
            assert_eq!(select_backend("fluid", false), Backend::Onnx);
        }
        // MLX still selects the native backend with the shim, both arches; without it, ONNX.
        assert_eq!(select_backend("mlx", true), Backend::Mlx);
        assert_eq!(select_backend("mlx", false), Backend::Onnx);
        // System and plain ONNX providers are shim-independent.
        assert_eq!(select_backend("system", false), Backend::System);
        assert_eq!(select_backend("cpu", true), Backend::Onnx);
    }
}
