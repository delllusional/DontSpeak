//! The canonical set of download / installer-prefetch targets.
//!
//! Historically each dispatcher — the installer prefetch list ([`crate::spec::prefetch_items`]),
//! the ds-helper `run_prefetch`, and the daemon's background download manager — re-spelled the
//! same bare `&str` tokens ("onnx", "kokoro", "parakeet", …) in its own `match`, with no shared
//! definition. A typo or a renamed token would silently fall through to a default arm. This enum
//! is the ONE definition every dispatcher parses to and matches on, so the wire tokens live in a
//! single place and an exhaustive `match` forces every target to be considered.

use std::str::FromStr;

/// A single download / prefetch target: a model, a shared runtime, or a Windows
/// installer prerequisite. The [`as_str`](DownloadTarget::as_str) token is the stable
/// wire form passed across the IPC / CLI / installer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadTarget {
    /// The shared onnxruntime dylib — the base ORT runtime every ONNX model runs on.
    Onnx,
    /// The FULL native-Kokoro asset set: the ~310 MB `kokoro-v1.0.onnx` model PLUS the
    /// ~28 MB voices pack (and, on supported platforms, the shared onnxruntime dylib).
    /// Wire token `"kokoro_model"` — renamed from the legacy `"kokoro"` to disambiguate
    /// the download target from the engine brand "kokoro"; the old `"kokoro"` token is
    /// still accepted by [`parse`](DownloadTarget::parse) for back-compat.
    KokoroModel,
    /// The ~28 MB Kokoro voices pack ONLY (`voices-v1.0.bin`), for the Apple ANE / Core ML
    /// path which self-manages the model chain but ships only `af_heart` and so still needs
    /// the shared voices npz that sources every other voice.
    KokoroVoices,
    /// The FULL Parakeet streaming-STT asset set (encoder + decoder + joiner + tokens, plus
    /// the shared onnxruntime dylib on supported platforms).
    Parakeet,
    /// The shared GPU runtime (~1.4 GB CUDA EP wheels) — drives BOTH engines (x86_64
    /// Windows/Linux only).
    Cuda,
    /// The speaker-diarization Core ML models (the macOS ANE-shim path; fetched into the
    /// dir the shim loads from offline).
    Diarization,
    /// "Everything": both ONNX models (Kokoro + Parakeet). The daemon's combined fetch and
    /// the installer/helper default.
    All,
    /// Both ONNX models (Kokoro + Parakeet) — the installer's "models" component group.
    Models,
    /// The Windows .NET Desktop Runtime installer prerequisite (URL only; the installer
    /// runs it itself).
    Dotnet,
    /// The Windows App Runtime installer prerequisite (URL only; the installer runs it
    /// itself).
    Winapp,
}

impl DownloadTarget {
    /// The stable wire token for this target (what the IPC / CLI / installer pass).
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadTarget::Onnx => "onnx",
            DownloadTarget::KokoroModel => "kokoro_model",
            DownloadTarget::KokoroVoices => "kokoro_voices",
            DownloadTarget::Parakeet => "parakeet",
            DownloadTarget::Cuda => "cuda",
            DownloadTarget::Diarization => "diarization",
            DownloadTarget::All => "all",
            DownloadTarget::Models => "models",
            DownloadTarget::Dotnet => "dotnet",
            DownloadTarget::Winapp => "winapp",
        }
    }

    /// Parse a wire token into a target. Also accepts the legacy alias `"kokoro"` for
    /// [`DownloadTarget::KokoroModel`] — back-compat for any external CLI caller (and the
    /// Windows installer's `--install-prefetched`/`--print-manifest kokoro` steps) that
    /// predates the `kokoro` → `kokoro_model` rename. Returns `None` for an unknown token.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "onnx" => DownloadTarget::Onnx,
            "kokoro_model" | "kokoro" => DownloadTarget::KokoroModel,
            "kokoro_voices" => DownloadTarget::KokoroVoices,
            "parakeet" => DownloadTarget::Parakeet,
            "cuda" => DownloadTarget::Cuda,
            "diarization" => DownloadTarget::Diarization,
            "all" => DownloadTarget::All,
            "models" => DownloadTarget::Models,
            "dotnet" => DownloadTarget::Dotnet,
            "winapp" => DownloadTarget::Winapp,
            _ => return None,
        })
    }
}

impl FromStr for DownloadTarget {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DownloadTarget::parse(s).ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_as_str_and_parse() {
        for t in [
            DownloadTarget::Onnx,
            DownloadTarget::KokoroModel,
            DownloadTarget::KokoroVoices,
            DownloadTarget::Parakeet,
            DownloadTarget::Cuda,
            DownloadTarget::Diarization,
            DownloadTarget::All,
            DownloadTarget::Models,
            DownloadTarget::Dotnet,
            DownloadTarget::Winapp,
        ] {
            assert_eq!(DownloadTarget::parse(t.as_str()), Some(t), "{:?}", t);
            assert_eq!(t.as_str().parse::<DownloadTarget>(), Ok(t), "{:?}", t);
        }
    }

    #[test]
    fn legacy_kokoro_alias_maps_to_kokoro_model() {
        assert_eq!(DownloadTarget::parse("kokoro"), Some(DownloadTarget::KokoroModel));
        // The canonical token is the renamed one.
        assert_eq!(DownloadTarget::KokoroModel.as_str(), "kokoro_model");
    }

    #[test]
    fn unknown_token_is_none() {
        assert_eq!(DownloadTarget::parse("bogus"), None);
        assert_eq!(DownloadTarget::parse(""), None);
    }
}
