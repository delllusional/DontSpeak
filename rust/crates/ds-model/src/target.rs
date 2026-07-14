//! The canonical set of download / installer-prefetch targets.
//!
//! Historically each dispatcher — the installer prefetch list ([`crate::spec::prefetch_items`]),
//! the ds-helper `run_prefetch`, and the engine's background download manager — re-spelled the
//! same bare `&str` tokens ("onnx", "kokoro", "parakeet", …) in its own `match`, with no shared
//! definition. A typo or a renamed token would silently fall through to a default arm. This enum
//! is the ONE definition every dispatcher matches on, so the wire tokens live in a single place
//! and an exhaustive `match` forces every target to be considered. String conversion happens
//! ONLY at the true process boundary — the ds-helper CLI (`--prefetch` / `--print-manifest` /
//! `--install-prefetched`) parses its argv token once; every in-process API (the engine's
//! `start_download`, [`crate::spec::prefetch_items`], the status rows) takes the enum, and
//! [`as_str`](DownloadTarget::as_str) is emitted only into logs/CLI output.
//!
//! Naming convention: per-model targets are `<brand>_<flavor>` (`kokoro_model` = the ONNX
//! flavor, `kokoro_coreml` = the apple-native Core ML flavor, …); shared runtimes and
//! installer groups are bare nouns (`onnxruntime`, `cuda`, `models`, …). Platform support
//! lives in ONE predicate, [`is_supported_on_this_host`](DownloadTarget::is_supported_on_this_host),
//! which every dispatcher's `cfg`-gated fetch arms must mirror.

/// A single download / prefetch target, plus retained legacy installer tokens. The
/// [`as_str`](DownloadTarget::as_str) token is the stable wire form passed across the
/// IPC / CLI / installer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DownloadTarget {
    /// The shared onnxruntime dylib — the base ORT runtime every ONNX model runs on.
    /// Wire token `"onnxruntime"` (not the bare format name "onnx", which elsewhere
    /// means a MODEL flavor — e.g. the `kokoro_model`/`parakeet_model` ONNX sets).
    Onnxruntime,
    /// The full portable Kokoro set: synth graph, voices, shared OOV G2P graphs, and ORT.
    /// Wire token `"kokoro_model"` — the `_model` suffix disambiguates this download target
    /// from the engine brand "kokoro".
    KokoroModel,
    /// Assets the Apple ANE / Core ML path shares with the portable frontend: voices, OOV G2P
    /// graphs, and ORT. The stable wire token remains `kokoro_voices` for compatibility.
    KokoroVoices,
    /// The apple-native Kokoro Core ML sets — the runtime ANE chain PLUS the G2P/lexicon
    /// sub-models ([`crate::coreml_repo::KOKORO_COREML_SET`]). macOS-only (ANE shim); fetched
    /// into the dirs the shim loads from offline, like [`DiarizationCoreml`](Self::DiarizationCoreml).
    KokoroCoreml,
    /// The FULL Parakeet streaming-STT asset set (encoder + decoder + joiner + tokens, plus
    /// the shared onnxruntime dylib on supported platforms). Wire token `"parakeet_model"` —
    /// the `_model` suffix disambiguates this download target from the engine brand "parakeet".
    ParakeetModel,
    /// The apple-native Parakeet Core ML sets — the streaming EOU set PLUS the offline
    /// sliding-window fallback ([`crate::coreml_repo::PARAKEET_COREML_SET`]). macOS-only.
    ParakeetCoreml,
    /// The SepFormer speech-separation model (`sepformer_int8.onnx`, ~29 MB) for the
    /// dictation speaker-lock, plus the shared onnxruntime dylib it runs on. macOS-only —
    /// the speaker-lock path that consumes it is macOS code. Wire token
    /// `"sepformer_model"`, the `_model` suffix like its ONNX siblings.
    SepformerModel,
    /// The shared GPU runtime (~1.4 GB CUDA EP wheels) — drives BOTH engines (x86_64
    /// Windows/Linux only).
    Cuda,
    /// The speaker-diarization Core ML models (the macOS ANE-shim path; fetched into the
    /// dir the shim loads from offline). The `_coreml` suffix marks the apple-native
    /// flavor, like its [`KokoroCoreml`](Self::KokoroCoreml)/[`ParakeetCoreml`](Self::ParakeetCoreml)
    /// siblings (there is no ONNX diarization flavor).
    DiarizationCoreml,
    /// Both ONNX models (Kokoro + Parakeet) — the installer's "models" component group.
    Models,
    /// Legacy Windows .NET Desktop Runtime wire token. The self-contained package no
    /// longer fetches this mutable prerequisite; retained only for wire compatibility.
    Dotnet,
    /// Legacy Windows App Runtime wire token. The self-contained package no longer
    /// fetches this mutable prerequisite; retained only for wire compatibility.
    Winapp,
}

impl DownloadTarget {
    /// The stable wire token for this target (what the IPC / CLI / installer pass).
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadTarget::Onnxruntime => "onnxruntime",
            DownloadTarget::KokoroModel => "kokoro_model",
            DownloadTarget::KokoroVoices => "kokoro_voices",
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
            "kokoro_voices" => DownloadTarget::KokoroVoices,
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
            | DownloadTarget::KokoroVoices
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
            DownloadTarget::KokoroVoices,
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
        assert_eq!(DownloadTarget::ParakeetModel.as_str(), "parakeet_model");
        assert_eq!(DownloadTarget::Onnxruntime.as_str(), "onnxruntime");
        assert_eq!(
            DownloadTarget::DiarizationCoreml.as_str(),
            "diarization_coreml"
        );
        // The pre-rename bare brand tokens are NOT accepted (single canonical name, no aliases).
        assert_eq!(DownloadTarget::parse("kokoro"), None);
        assert_eq!(DownloadTarget::parse("parakeet"), None);
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
            KokoroVoices,
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
