//! Canonical download / installer-prefetch targets.
//!
//! ONE enum every dispatcher matches (`prefetch_items`, `run_prefetch`, engine download
//! manager) so wire tokens can't typo-fallthrough. Parse only at the process boundary
//! (CLI); in-process APIs take the enum. Naming: `<brand>_<flavor>` for models,
//! bare nouns for runtimes/groups. Platform gate: [`is_supported_on_this_host`](DownloadTarget::is_supported_on_this_host).

/// Download/prefetch target. [`as_str`](DownloadTarget::as_str) is the stable wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadTarget {
    /// Shared ORT dylib. Wire `"onnxruntime"` (not bare `"onnx"` — that means a model flavor).
    Onnxruntime,
    /// Full portable Kokoro + ORT. Wire `"kokoro_model"` (not brand `"kokoro"`).
    KokoroModel,
    /// Voices + OOV G2P + ORT (shared by both Kokoro backends). Wire `"kokoro_frontend"`.
    KokoroFrontend,
    /// Apple-native Kokoro Core ML set ([`crate::coreml_repo::KOKORO_COREML_SET`]). macOS.
    KokoroCoreml,
    /// Full Parakeet streaming set + ORT. Wire `"parakeet_model"`.
    ParakeetModel,
    /// Apple-native Parakeet Core ML set ([`crate::coreml_repo::PARAKEET_COREML_SET`]). macOS.
    ParakeetCoreml,
    /// SepFormer speaker-lock + ORT. macOS. Wire `"sepformer_model"`.
    SepformerModel,
    /// CUDA EP wheels (~1.4 GB). x86_64 Windows/Linux only.
    Cuda,
    /// Speaker-diarization Core ML (macOS). Wire `"diarization_coreml"`.
    DiarizationCoreml,
    /// Installer group: Kokoro + Parakeet ONNX.
    Models,
    /// Legacy Windows .NET Desktop Runtime token (no-op; wire compat only).
    Dotnet,
    /// Legacy Windows App Runtime token (no-op; wire compat only).
    Winapp,
}

impl DownloadTarget {
    /// The stable wire token for this target (what the IPC / CLI / installer pass).
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadTarget::Onnxruntime => "onnxruntime",
            DownloadTarget::KokoroModel => "kokoro_model",
            DownloadTarget::KokoroFrontend => "kokoro_frontend",
            DownloadTarget::KokoroCoreml => "kokoro_coreml",
            DownloadTarget::ParakeetModel => "parakeet_model",
            DownloadTarget::ParakeetCoreml => "parakeet_coreml",
            DownloadTarget::SepformerModel => "sepformer_model",
            DownloadTarget::Cuda => "cuda",
            DownloadTarget::DiarizationCoreml => "diarization_coreml",
            DownloadTarget::Models => "models",
            DownloadTarget::Dotnet => "dotnet",
            DownloadTarget::Winapp => "winapp",
        }
    }

    /// Parse a wire token into a target. Each target has exactly ONE canonical token (no
    /// legacy aliases). Returns `None` for an unknown token.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "onnxruntime" => DownloadTarget::Onnxruntime,
            "kokoro_model" => DownloadTarget::KokoroModel,
            "kokoro_frontend" => DownloadTarget::KokoroFrontend,
            "kokoro_coreml" => DownloadTarget::KokoroCoreml,
            "parakeet_model" => DownloadTarget::ParakeetModel,
            "parakeet_coreml" => DownloadTarget::ParakeetCoreml,
            "sepformer_model" => DownloadTarget::SepformerModel,
            "cuda" => DownloadTarget::Cuda,
            "diarization_coreml" => DownloadTarget::DiarizationCoreml,
            "models" => DownloadTarget::Models,
            "dotnet" => DownloadTarget::Dotnet,
            "winapp" => DownloadTarget::Winapp,
            _ => return None,
        })
    }

    /// Can this target actually be fetched on the RUNNING platform? The ONE spelling of
    /// every target's platform gate — the `#[cfg]`-gated fetch arms in each dispatcher
    /// (the engine's `start_download`, [`crate::spec::prefetch_items`], ds-helper's
    /// `run_prefetch`) must mirror this matrix:
    ///
    /// * the Core ML sets (`kokoro_coreml` / `parakeet_coreml` / `diarization_coreml`)
    ///   exist only where the ANE shim runs — macOS;
    /// * `sepformer_model` is macOS-only too (the speaker-lock is macOS code), though it
    ///   is plain ONNX, not Core ML;
    /// * `cuda` (the ONNX CUDA EP wheels) exists only on x86_64 Windows/Linux;
    /// * legacy `dotnet` / `winapp` tokens are not fetchable on any host;
    /// * everything else (the onnxruntime dylib and the ONNX model sets) is universal.
    pub fn is_supported_on_this_host(self) -> bool {
        match self {
            DownloadTarget::KokoroCoreml
            | DownloadTarget::ParakeetCoreml
            | DownloadTarget::DiarizationCoreml
            | DownloadTarget::SepformerModel => cfg!(target_os = "macos"),
            DownloadTarget::Cuda => cfg!(all(
                any(target_os = "windows", target_os = "linux"),
                target_arch = "x86_64"
            )),
            DownloadTarget::Dotnet | DownloadTarget::Winapp => false,
            DownloadTarget::Onnxruntime
            | DownloadTarget::KokoroModel
            | DownloadTarget::KokoroFrontend
            | DownloadTarget::ParakeetModel
            | DownloadTarget::Models => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_as_str_and_parse() {
        for t in [
            DownloadTarget::Onnxruntime,
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroFrontend,
            DownloadTarget::KokoroCoreml,
            DownloadTarget::ParakeetModel,
            DownloadTarget::ParakeetCoreml,
            DownloadTarget::SepformerModel,
            DownloadTarget::Cuda,
            DownloadTarget::DiarizationCoreml,
            DownloadTarget::Models,
            DownloadTarget::Dotnet,
            DownloadTarget::Winapp,
        ] {
            assert_eq!(DownloadTarget::parse(t.as_str()), Some(t), "{:?}", t);
        }
    }

    #[test]
    fn every_target_has_one_canonical_token_no_legacy_aliases() {
        assert_eq!(DownloadTarget::KokoroModel.as_str(), "kokoro_model");
        assert_eq!(DownloadTarget::KokoroFrontend.as_str(), "kokoro_frontend");
        assert_eq!(DownloadTarget::ParakeetModel.as_str(), "parakeet_model");
        assert_eq!(DownloadTarget::Onnxruntime.as_str(), "onnxruntime");
        assert_eq!(
            DownloadTarget::DiarizationCoreml.as_str(),
            "diarization_coreml"
        );
        // The pre-rename bare brand tokens are NOT accepted (single canonical name, no aliases).
        assert_eq!(DownloadTarget::parse("kokoro"), None);
        assert_eq!(DownloadTarget::parse("parakeet"), None);
        assert_eq!(DownloadTarget::parse("kokoro_voices"), None);
        // Same for the pre-rename runtime/diarization tokens: "onnx" meant the RUNTIME here
        // but a MODEL flavor everywhere else, and "diarization" lacked its `_coreml` flavor
        // suffix — both renamed, neither kept as an alias.
        assert_eq!(DownloadTarget::parse("onnx"), None);
        assert_eq!(DownloadTarget::parse("diarization"), None);
        // The legacy "all" combined-fetch target is GONE: the engine sequences per-model
        // targets (each row shows its own %), and the installer prefetch's no-arg default
        // falls through run_prefetch's unknown-token arm to the same models+cuda behavior.
        assert_eq!(DownloadTarget::parse("all"), None);
    }

    #[test]
    fn unknown_token_is_none() {
        assert_eq!(DownloadTarget::parse("bogus"), None);
        assert_eq!(DownloadTarget::parse(""), None);
    }

    // The platform-support matrix, pinned as LITERAL per-OS expectations (not re-evaluated
    // `cfg!` expressions, which would be tautological) — a drifted gate shows up on that
    // platform's CI leg rather than silently mis-routing a dispatcher.
    #[test]
    fn platform_support_matrix() {
        use DownloadTarget::*;
        // Universal targets: fetchable everywhere.
        for t in [
            Onnxruntime,
            KokoroModel,
            KokoroFrontend,
            ParakeetModel,
            Models,
        ] {
            assert!(t.is_supported_on_this_host(), "{t:?} must be universal");
        }
        // The macOS-only set: the ANE Core ML sets plus the (plain-ONNX) speaker-lock separator.
        let mac_only = [
            KokoroCoreml,
            ParakeetCoreml,
            DiarizationCoreml,
            SepformerModel,
        ];
        #[cfg(target_os = "macos")]
        {
            for t in mac_only {
                assert!(t.is_supported_on_this_host(), "{t:?} is a macOS target");
            }
            for t in [Cuda, Dotnet, Winapp] {
                assert!(
                    !t.is_supported_on_this_host(),
                    "{t:?} is not a macOS target"
                );
            }
        }
        #[cfg(target_os = "windows")]
        {
            for t in mac_only {
                assert!(!t.is_supported_on_this_host(), "{t:?} is macOS-only");
            }
            assert!(!Dotnet.is_supported_on_this_host());
            assert!(!Winapp.is_supported_on_this_host());
            assert_eq!(
                Cuda.is_supported_on_this_host(),
                cfg!(target_arch = "x86_64")
            );
        }
        #[cfg(target_os = "linux")]
        {
            for t in [
                mac_only[0],
                mac_only[1],
                mac_only[2],
                mac_only[3],
                Dotnet,
                Winapp,
            ] {
                assert!(
                    !t.is_supported_on_this_host(),
                    "{t:?} is not a Linux target"
                );
            }
            assert_eq!(
                Cuda.is_supported_on_this_host(),
                cfg!(target_arch = "x86_64")
            );
        }
    }
}
