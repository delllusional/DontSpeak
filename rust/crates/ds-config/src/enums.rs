//! Engine-selection token enums and fail-open / strict / serialize-as-token plumbing.
//!
//! Declared FIRST in `lib.rs` so `fail_open_de!`, `serialize_as_str!`, `strict_de!` are
//! textually in scope (`#[macro_use]`).

use ds_client::WiredAgent;
use serde::{Deserialize, Deserializer};

use crate::host::{Arch, Os};

// Scalar engine enums: fail-open `de_*` (typo/absent → default). Set/ladder fields are
// Vecs with their own deserializers; element enums here are the building blocks.

/// STT backend token. Enum Default is `BuiltIn`; out-of-box walks `stt_engine_ladder`.
/// Off: `stt_engine = Some(vec![])`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttEngine {
    /// Local Parakeet (`built_in`). Via [`Provider`]; factory → ClaudeCode if model missing.
    #[default]
    BuiltIn,
    /// OS on-device STT (macOS). Inert when unavailable (no silent fall to claude_code).
    System,
    /// CC dictation: read `voice:pushToTalk` only (`claude_code`).
    ClaudeCode,
}

impl SttEngine {
    /// All variants, canonical-token order — single source for catalog parity tests.
    pub const ALL: &'static [SttEngine] =
        &[SttEngine::BuiltIn, SttEngine::System, SttEngine::ClaudeCode];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "built_in" => Some(SttEngine::BuiltIn),
            "system" => Some(SttEngine::System),
            "claude_code" => Some(SttEngine::ClaudeCode),
            _ => None,
        }
    }

    /// Canonical token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            SttEngine::ClaudeCode => "claude_code",
            SttEngine::BuiltIn => "built_in",
            SttEngine::System => "system",
        }
    }

    /// Usable on this build/platform? Ladder predicate ([`crate::VoiceConfig::resolved_stt`]).
    /// Static only — runtime model/auth still apply. `built_in` needs MLX or ONNX; `system`
    /// macOS-only; `claude_code` always.
    pub fn is_stt_usable(self) -> bool {
        self.stt_usable_on(Os::this(), Arch::this())
    }

    /// Pure `(os, arch)` form of [`is_stt_usable`](SttEngine::is_stt_usable).
    pub(crate) fn stt_usable_on(self, os: Os, arch: Arch) -> bool {
        match self {
            SttEngine::BuiltIn => built_in_usable_on(os, arch),
            SttEngine::System => system_stt_buildable_on(os, arch),
            SttEngine::ClaudeCode => true,
        }
    }
}

/// TTS backend. `built_in` hosts the model selected by [`crate::TtsModel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtsEngine {
    #[default]
    BuiltIn,
    System,
}

impl TtsEngine {
    /// All variants, canonical-token order — catalog parity single source.
    pub const ALL: &'static [TtsEngine] = &[TtsEngine::BuiltIn, TtsEngine::System];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "built_in" => Some(TtsEngine::BuiltIn),
            "system" => Some(TtsEngine::System),
            _ => None,
        }
    }

    /// Config token (`built_in` / `system`).
    pub fn as_str(self) -> &'static str {
        match self {
            TtsEngine::BuiltIn => "built_in",
            TtsEngine::System => "system",
        }
    }

    /// Usable on this build/platform? ([`crate::VoiceConfig::resolved_tts`]).
    /// `built_in` needs MLX or ONNX; `system` macOS+Windows only.
    pub fn is_tts_usable(self) -> bool {
        self.tts_usable_on(Os::this(), Arch::this())
    }

    /// Pure `(os, arch)` form of [`is_tts_usable`](TtsEngine::is_tts_usable).
    pub(crate) fn tts_usable_on(self, os: Os, arch: Arch) -> bool {
        match self {
            TtsEngine::BuiltIn => built_in_usable_on(os, arch),
            TtsEngine::System => system_tts_buildable_on(os),
        }
    }
}

/// Voice input mode. Default `RecordSubmit` (Caps PTT). `Always` = hands-free loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenMode {
    #[default]
    RecordSubmit,
    Always,
}

impl ListenMode {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "record_submit" => Some(ListenMode::RecordSubmit),
            "always" => Some(ListenMode::Always),
            _ => None,
        }
    }

    /// Canonical token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            ListenMode::RecordSubmit => "record_submit",
            ListenMode::Always => "always",
        }
    }
}

/// Diarization runtime rung. `diarizer: Vec` is also on/off: empty = off; first
/// usable rung wins ([`crate::VoiceConfig::resolved_diarizer`]). Apple-Silicon-only today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiarizerProvider {
    /// MLX Audio on Apple Silicon (macOS only).
    #[default]
    Mlx,
    /// FluidAudio's Core ML / ANE diarization — pyannote segmentation + WeSpeaker (macOS
    /// Apple Silicon). Second rung beside MLX; both need the Neural Engine, so both share the
    /// Apple-Silicon gate.
    Fluid,
}

impl DiarizerProvider {
    /// All variants — catalog parity single source.
    pub const ALL: &'static [DiarizerProvider] = &[DiarizerProvider::Mlx, DiarizerProvider::Fluid];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mlx" => Some(DiarizerProvider::Mlx),
            "fluid" => Some(DiarizerProvider::Fluid),
            _ => None,
        }
    }

    /// Canonical token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            DiarizerProvider::Mlx => "mlx",
            DiarizerProvider::Fluid => "fluid",
        }
    }

    /// Platform usable? The ONE spelling of the diarizer's platform gate:
    /// `ds_stt::diarize::ensure_backend` defers to it rather than keeping a second `cfg!`, so
    /// the rung config calls usable is exactly the rung that maps to a backend.
    pub fn is_diarizer_usable(self) -> bool {
        self.diarizer_usable_on(Os::this(), Arch::this())
    }

    /// Pure `(os, arch)` form of [`is_diarizer_usable`](DiarizerProvider::is_diarizer_usable) —
    /// the shape `Provider` already has (`stt_usable_on`/`tts_usable_on`), so the cross-platform
    /// matrix can pin `fluid` off Apple Silicon without a second `cfg!` (the drift #211 caught).
    /// MLX Sortformer and FluidAudio's Core ML chains alike run only on the Neural Engine.
    pub(crate) fn diarizer_usable_on(self, os: Os, arch: Arch) -> bool {
        match self {
            DiarizerProvider::Mlx | DiarizerProvider::Fluid => crate::host::apple_silicon(os, arch),
        }
    }
}

/// Shared compute rung (TTS+STT). Ladder default MLX→CUDA→CPU. `Ort*` = ORT EPs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provider {
    /// ORT CPU EP — always available.
    #[default]
    OrtCpu,
    /// ORT CUDA (NVIDIA). May download GPU runtime; STT int8 may fall back per-op.
    OrtCuda,
    /// ORT Core ML execution provider (Apple-Silicon TTS only).
    OrtCoreMl,
    /// Native MLX Metal/CPU backend. Both engines on Apple Silicon.
    Mlx,
    /// FluidAudio's native Core ML / ANE chains. Distinct from [`Provider::OrtCoreMl`]:
    /// that is ONNX Runtime's Core ML execution provider over our own ONNX graphs, this
    /// loads FluidAudio's pinned `.mlmodelc` bundles through its Swift API. Kokoro TTS,
    /// Parakeet STT, and diarization only — see `TtsModelDescriptor::providers`.
    Fluid,
}

impl Provider {
    /// All variants — catalog parity single source.
    pub const ALL: &'static [Provider] = &[
        Provider::Mlx,
        Provider::Fluid,
        Provider::OrtCuda,
        Provider::OrtCoreMl,
        Provider::OrtCpu,
    ];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(Provider::OrtCpu),
            "cuda" => Some(Provider::OrtCuda),
            "coreml" => Some(Provider::OrtCoreMl),
            "mlx" => Some(Provider::Mlx),
            "fluid" => Some(Provider::Fluid),
            _ => None,
        }
    }

    /// Canonical token (warm child `DONTSPEAK_PROVIDER` env).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::OrtCpu => "cpu",
            Provider::OrtCuda => "cuda",
            Provider::OrtCoreMl => "coreml",
            Provider::Mlx => "mlx",
            Provider::Fluid => "fluid",
        }
    }

    /// STT usable on this platform? See `resolved_stt_provider`.
    pub(crate) fn is_stt_usable(self) -> bool {
        self.stt_usable_on(Os::this(), Arch::this())
    }

    /// Pure `(os, arch)` form of STT usability. The two ladders differ on ONE rung — ORT's
    /// Core ML EP is wired for TTS graphs only — so deriving STT from TTS keeps the four
    /// shared rungs from drifting apart the way a second copy of the match would.
    pub(crate) fn stt_usable_on(self, os: Os, arch: Arch) -> bool {
        self != Provider::OrtCoreMl && self.tts_usable_on(os, arch)
    }

    /// TTS usable on this platform?
    pub(crate) fn is_tts_usable(self) -> bool {
        self.tts_usable_on(Os::this(), Arch::this())
    }

    /// Pure `(os, arch)` form of TTS usability, and the base
    /// [`stt_usable_on`](Provider::stt_usable_on) narrows. MLX's Metal kernels, FluidAudio's
    /// ANE chains, and ORT's Core ML EP all die off Apple Silicon — the last of them only at
    /// `Session::run` (#250).
    pub(crate) fn tts_usable_on(self, os: Os, arch: Arch) -> bool {
        match self {
            Provider::OrtCpu => true,
            Provider::Mlx | Provider::Fluid | Provider::OrtCoreMl => {
                crate::host::apple_silicon(os, arch)
            }
            Provider::OrtCuda => crate::host::cuda_host(os, arch),
        }
    }
}

/// Preference token asks for NVIDIA GPU? Shared TTS/STT load paths.
pub fn provider_pref_wants_gpu(pref: &str) -> bool {
    pref.eq_ignore_ascii_case("cuda") || pref.eq_ignore_ascii_case("auto")
}

/// Warm-child loaded backend (`PROVIDER`/`STT_PROVIDER`). Distinct from config [`Provider`];
/// UPPERCASE. Stringify once at IPC; status maps via `to_provider` (typos = compile errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealizedProvider {
    Cuda,
    /// Universal fallback.
    Cpu,
    /// ONNX Runtime Core ML execution provider.
    CoreMl,
    /// MLX Audio on Apple Silicon.
    Mlx,
    /// FluidAudio Core ML / ANE on Apple Silicon.
    Fluid,
    /// System STT (no ort).
    System,
}

impl RealizedProvider {
    /// UPPERCASE wire token (round-trips through `parse`).
    pub fn as_str(self) -> &'static str {
        match self {
            RealizedProvider::Cuda => "CUDA",
            RealizedProvider::Cpu => "CPU",
            RealizedProvider::CoreMl => "CoreML",
            RealizedProvider::Mlx => "MLX",
            RealizedProvider::Fluid => "Fluid",
            RealizedProvider::System => "System",
        }
    }

    /// Wire token → self; unknown → Cpu (fail closed on GPU claim).
    pub fn parse(s: &str) -> Self {
        match s {
            "CUDA" => RealizedProvider::Cuda,
            "CoreML" => RealizedProvider::CoreMl,
            "MLX" => RealizedProvider::Mlx,
            "Fluid" => RealizedProvider::Fluid,
            "System" => RealizedProvider::System,
            _ => RealizedProvider::Cpu,
        }
    }

    /// Map to config [`Provider`] (`System` → OrtCpu for status).
    pub fn to_provider(self) -> Provider {
        match self {
            RealizedProvider::Cuda => Provider::OrtCuda,
            RealizedProvider::CoreMl => Provider::OrtCoreMl,
            RealizedProvider::Mlx => Provider::Mlx,
            RealizedProvider::Fluid => Provider::Fluid,
            RealizedProvider::Cpu | RealizedProvider::System => Provider::OrtCpu,
        }
    }
}

impl std::fmt::Display for RealizedProvider {
    /// Writes the wire token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Menu-bar color/breathe set (`tray`). At most one form per state; animated wins.
/// Default `["stt", "tts_animated"]`; `[]` = never color. Engine passes through only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayKind {
    Stt,         // mic static
    Tts,         // voice static
    SttAnimated, // mic + breathe
    TtsAnimated, // voice + breathe
}

impl TrayKind {
    /// All variants — catalog parity single source.
    pub const ALL: &'static [TrayKind] = &[
        TrayKind::Stt,
        TrayKind::Tts,
        TrayKind::SttAnimated,
        TrayKind::TtsAnimated,
    ];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "stt" => Some(TrayKind::Stt),
            "tts" => Some(TrayKind::Tts),
            "stt_animated" => Some(TrayKind::SttAnimated),
            "tts_animated" => Some(TrayKind::TtsAnimated),
            _ => None,
        }
    }

    /// Canonical token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            TrayKind::Stt => "stt",
            TrayKind::Tts => "tts",
            TrayKind::SttAnimated => "stt_animated",
            TrayKind::TtsAnimated => "tts_animated",
        }
    }

    /// Mic/recording state (vs voice).
    pub(crate) fn is_stt(self) -> bool {
        matches!(self, TrayKind::Stt | TrayKind::SttAnimated)
    }
    /// `_animated` form.
    pub(crate) fn animated(self) -> bool {
        matches!(self, TrayKind::SttAnimated | TrayKind::TtsAnimated)
    }
}

/// At most one token per state (animated wins); stt then tts. `[]` stays empty.
pub fn normalize_tray(kinds: Vec<TrayKind>) -> Vec<TrayKind> {
    let mut stt: Option<bool> = None; // Some(animated?)
    let mut tts: Option<bool> = None;
    for k in kinds {
        if k.is_stt() {
            stt = Some(stt.unwrap_or(false) || k.animated());
        } else {
            tts = Some(tts.unwrap_or(false) || k.animated());
        }
    }
    let mut out = Vec::new();
    if let Some(a) = stt {
        out.push(if a {
            TrayKind::SttAnimated
        } else {
            TrayKind::Stt
        });
    }
    if let Some(a) = tts {
        out.push(if a {
            TrayKind::TtsAnimated
        } else {
            TrayKind::Tts
        });
    }
    out
}

/// Whose pending speech a submit cancels (`clear_on_input` set). Any submit triggers permanent
/// cancel. `current` = submitting session; `other` = everything else (incl. untagged MCP).
/// Empty = never cancel. See [`crate::VoiceConfig::clear_on_input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSpeechScope {
    Current,
    Other,
}

impl CancelSpeechScope {
    /// All variants — catalog parity single source.
    pub const ALL: &'static [CancelSpeechScope] =
        &[CancelSpeechScope::Current, CancelSpeechScope::Other];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "current" => Some(CancelSpeechScope::Current),
            "other" => Some(CancelSpeechScope::Other),
            _ => None,
        }
    }

    /// Canonical token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            CancelSpeechScope::Current => "current",
            CancelSpeechScope::Other => "other",
        }
    }
}

/// Narration set (`narrate`). `Digests` = blockquotes + injects spec; `Shorts` = short
/// no-blockquote replies. Empty = off. `Digests` gates the injected provide spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NarrateKind {
    Digests,
    Shorts,
}

impl NarrateKind {
    /// All variants — catalog parity single source.
    pub const ALL: &'static [NarrateKind] = &[NarrateKind::Digests, NarrateKind::Shorts];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "digests" => Some(NarrateKind::Digests),
            "shorts" => Some(NarrateKind::Shorts),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NarrateKind::Digests => "digests",
            NarrateKind::Shorts => "shorts",
        }
    }
}

// `WiredAgent` lives in `ds-client` (cycle avoidance); re-exported from `lib.rs`.

/// Parse enum-token array in order (drop unknown/dup). `None` = non-array (field fallback).
macro_rules! fail_open_vec {
    ($value:expr, $ty:ty, $parse:expr) => {{
        match $value {
            toml::Value::Array(items) => {
                let mut out = Vec::<$ty>::new();
                for item in items {
                    if let Some(value) = item.as_str().and_then($parse)
                        && !out.contains(&value)
                    {
                        out.push(value);
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }};
}

/// Fail-open `exclude_clients`: array → known clients (deduped); non-array/`None` → wire all;
/// `Some([])` = none. Pinned by `exclude_clients_drops_unwired_tokens`.
pub(crate) fn de_exclude_clients<'de, D>(d: D) -> Result<Option<Vec<WiredAgent>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(fail_open_vec!(&v, WiredAgent, WiredAgent::parse))
}

// Macros: fail-open / serialize-as-token / strict (textually scoped — see module doc).

macro_rules! fail_open_de {
    ($fn_name:ident, $ty:ty) => {
        /// Fail-open: unknown/wrong type → `Default` (hand-edited config must not brick).
        pub(crate) fn $fn_name<'de, D>(d: D) -> Result<$ty, D::Error>
        where
            D: Deserializer<'de>,
        {
            let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
            Ok(v.as_str().and_then(<$ty>::parse).unwrap_or_default())
        }
    };
}

fail_open_de!(de_listen_mode, ListenMode);
/// Default when `clear_on_input` absent: cancel current session only. Explicit `[]` = never.
pub(crate) fn default_clear_on_input() -> Vec<CancelSpeechScope> {
    vec![CancelSpeechScope::Current]
}
/// Fail-open `clear_on_input`: array keeps known tokens; empty = never cancel; non-array →
/// [`default_clear_on_input`] (not empty — that would invert the default).
pub(crate) fn de_clear_on_input<'de, D>(d: D) -> Result<Vec<CancelSpeechScope>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(
        fail_open_vec!(&v, CancelSpeechScope, CancelSpeechScope::parse)
            .unwrap_or_else(default_clear_on_input),
    )
}

/// Serialize as `as_str()` token (round-trip with fail-open `parse`).
macro_rules! serialize_as_str {
    ($ty:ty) => {
        impl serde::Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }
    };
}

serialize_as_str!(SttEngine);
serialize_as_str!(TtsEngine);
serialize_as_str!(ListenMode);
serialize_as_str!(Provider);
serialize_as_str!(TrayKind);
serialize_as_str!(CancelSpeechScope);
serialize_as_str!(DiarizerProvider);
serialize_as_str!(NarrateKind);
// `WiredAgent` Serialize lives in `ds-client` (no macro there).

/// Strict Deserialize: unknown → error (`set_config` only; VoiceConfig uses fail-open `de_*`).
macro_rules! strict_de {
    ($ty:ty, $valid:literal) => {
        impl<'de> serde::Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                <$ty>::parse(&s)
                    .ok_or_else(|| serde::de::Error::custom(concat!("must be one of: ", $valid)))
            }
        }
    };
}

strict_de!(SttEngine, "built_in|system|claude_code");
strict_de!(TtsEngine, "built_in|system");
strict_de!(ListenMode, "record_submit|always");
strict_de!(Provider, "cpu|cuda|coreml|mlx|fluid");
strict_de!(TrayKind, "stt|tts|stt_animated|tts_animated");
strict_de!(CancelSpeechScope, "current|other");
strict_de!(DiarizerProvider, "mlx|fluid");
strict_de!(NarrateKind, "digests|shorts");

/// Default narrate: shorts + digests. Empty array opts out.
pub(crate) fn default_narrate() -> Vec<NarrateKind> {
    vec![NarrateKind::Shorts, NarrateKind::Digests]
}

/// Fail-open `narrate`: array of known tokens; empty = off; non-array → [`default_narrate`].
pub(crate) fn de_narrate<'de, D>(d: D) -> Result<Vec<NarrateKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, NarrateKind, NarrateKind::parse).unwrap_or_else(default_narrate))
}

/// Default tray: stt static + tts_animated.
pub(crate) fn default_tray() -> Vec<TrayKind> {
    vec![TrayKind::Stt, TrayKind::TtsAnimated]
}

/// Fail-open `tray`: array of known tokens (then normalize); empty = never color;
/// non-array → default.
pub(crate) fn de_tray<'de, D>(d: D) -> Result<Vec<TrayKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    let parsed = fail_open_vec!(&v, TrayKind, TrayKind::parse).unwrap_or_else(default_tray);
    Ok(normalize_tray(parsed))
}

/// Default provider ladder: MLX → CUDA → CPU.
pub fn default_provider() -> Vec<Provider> {
    vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu]
}

/// Fail-open `provider` ladder. Empty/unknown/non-array → [`default_provider`] (always a backend).
pub(crate) fn de_provider<'de, D>(d: D) -> Result<Vec<Provider>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, Provider, Provider::parse)
        .filter(|providers| !providers.is_empty())
        .unwrap_or_else(default_provider))
}

/// Default diarizer ladder: empty = off (opt-in; no separate enable flag).
pub(crate) fn default_diarizer() -> Vec<DiarizerProvider> {
    Vec::new()
}

/// Fail-open `diarizer`: empty/non-array = off.
pub(crate) fn de_diarizer<'de, D>(d: D) -> Result<Vec<DiarizerProvider>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, DiarizerProvider, DiarizerProvider::parse).unwrap_or_default())
}

// Engine LADDER (config-file Vec; empty = off; first usable rung wins) + PREFERENCE
// (`Option<Vec>` tri-state: None=ladder, Some([])=off, Some([e])=force, no auto-sub).
// See VoiceConfig::resolved_tts / resolved_stt. Pure (os, arch) helpers for cross-platform tests.
/// Every target has an ONNX Runtime it can fetch — Intel macOS on Microsoft's last x86_64
/// build, the rest on the current pin (see ds-model's `onnxruntime_dist`).
fn built_in_usable_on(_os: Os, _arch: Arch) -> bool {
    true
}
/// System STT is macOS-any-arch (static); runtime probe handles auth/locale.
fn system_stt_buildable_on(os: Os, arch: Arch) -> bool {
    let _ = arch;
    os == Os::MacOs
}
/// System TTS (`say`/SAPI) on macOS+Windows only.
fn system_tts_buildable_on(os: Os) -> bool {
    matches!(os, Os::MacOs | Os::Windows)
}

/// Default TTS ladder: built_in → system. `[]` = off.
pub(crate) fn default_tts_engine_ladder() -> Vec<TtsEngine> {
    vec![TtsEngine::BuiltIn, TtsEngine::System]
}

/// Default STT ladder: system → built_in → claude_code. `system` macOS-only usable;
/// `claude_code` always last. `[]` = off (Caps still silences).
pub(crate) fn default_stt_engine_ladder() -> Vec<SttEngine> {
    vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode]
}

/// Fail-open `tts_engine_ladder`: arrays only (empty = off); scalar/wrong type → default ladder.
pub(crate) fn de_tts_engine_ladder<'de, D>(d: D) -> Result<Vec<TtsEngine>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(parse_tts_ladder(&v))
}

/// Fail-open `stt_engine_ladder` — same rules as [`de_tts_engine_ladder`].
pub(crate) fn de_stt_engine_ladder<'de, D>(d: D) -> Result<Vec<SttEngine>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(parse_stt_ladder(&v))
}

pub(crate) fn parse_tts_ladder(v: &toml::Value) -> Vec<TtsEngine> {
    fail_open_vec!(v, TtsEngine, TtsEngine::parse).unwrap_or_else(default_tts_engine_ladder)
}

pub(crate) fn parse_stt_ladder(v: &toml::Value) -> Vec<SttEngine> {
    fail_open_vec!(v, SttEngine, SttEngine::parse).unwrap_or_else(default_stt_engine_ladder)
}

/// Fail-open `tts_engine` preference: engine scalar = force; `"off"`/`[]` = off;
/// else `None` (use ladder).
pub(crate) fn de_tts_engine_pref<'de, D>(d: D) -> Result<Option<Vec<TtsEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(match v {
        toml::Value::String(s) if s.trim().eq_ignore_ascii_case("off") => Some(Vec::new()),
        toml::Value::String(s) => TtsEngine::parse(&s).map(|e| vec![e]),
        toml::Value::Array(items) if items.is_empty() => Some(Vec::new()),
        _ => None,
    })
}

/// Fail-open `stt_engine` preference — see [`de_tts_engine_pref`].
pub(crate) fn de_stt_engine_pref<'de, D>(d: D) -> Result<Option<Vec<SttEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(match v {
        toml::Value::String(s) if s.trim().eq_ignore_ascii_case("off") => Some(Vec::new()),
        toml::Value::String(s) => SttEngine::parse(&s).map(|e| vec![e]),
        toml::Value::Array(items) if items.is_empty() => Some(Vec::new()),
        _ => None,
    })
}

/// Serialize `tts_engine` preference: single token or `[]`. (`None` skipped by serde attr.)
pub(crate) fn se_tts_engine_pref<S: serde::Serializer>(
    v: &Option<Vec<TtsEngine>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(rungs) if rungs.is_empty() => s.collect_seq(std::iter::empty::<&str>()),
        Some(rungs) => {
            debug_assert_eq!(rungs.len(), 1, "engine preference must have one rung");
            s.serialize_str(rungs[0].as_str())
        }
        None => s.serialize_none(),
    }
}

/// Serialize `stt_engine` preference — see [`se_tts_engine_pref`].
pub(crate) fn se_stt_engine_pref<S: serde::Serializer>(
    v: &Option<Vec<SttEngine>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(rungs) if rungs.is_empty() => s.collect_seq(std::iter::empty::<&str>()),
        Some(rungs) => {
            debug_assert_eq!(rungs.len(), 1, "engine preference must have one rung");
            s.serialize_str(rungs[0].as_str())
        }
        None => s.serialize_none(),
    }
}

/// Strict JSON for `set_config` `tts_engine`: engine token or `"off"`; wrong shape/token errors.
/// Absent → `None`. (No `Off` enum variant.)
pub fn de_opt_pref_tts_engine<'de, D>(d: D) -> Result<Option<Vec<TtsEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    let Some(v) = Option::<serde_json::Value>::deserialize(d)? else {
        return Ok(None);
    };
    match &v {
        serde_json::Value::String(s) if s == "off" => Ok(Some(Vec::new())),
        serde_json::Value::String(s) => TtsEngine::parse(s)
            .map(|e| Some(vec![e]))
            .ok_or_else(|| D::Error::custom("must be one of: built_in|system|off")),
        _ => Err(D::Error::custom("must be a string: built_in|system|off")),
    }
}

/// Strict JSON for `set_config` `stt_engine` — see [`de_opt_pref_tts_engine`].
pub fn de_opt_pref_stt_engine<'de, D>(d: D) -> Result<Option<Vec<SttEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error as _;
    let Some(v) = Option::<serde_json::Value>::deserialize(d)? else {
        return Ok(None);
    };
    match &v {
        serde_json::Value::String(s) if s == "off" => Ok(Some(Vec::new())),
        serde_json::Value::String(s) => SttEngine::parse(s)
            .map(|e| Some(vec![e]))
            .ok_or_else(|| D::Error::custom("must be one of: built_in|system|claude_code|off")),
        _ => Err(D::Error::custom(
            "must be a string: built_in|system|claude_code|off",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DRIFT GUARD for the realized-EP token vocabulary: every `RealizedProvider` round-trips
    /// through `as_str()`/`parse()`, `parse()` degrades unknown tokens to CPU (never a false GPU
    /// claim), and `to_provider()` maps onto the config ladder as expected. This is the single
    /// source of truth the TTS + STT producers and the status consumer all share, so this test
    /// failing is the ONLY way the shared vocabulary can change — it can't drift silently per site.
    #[test]
    fn realized_provider_vocabulary_is_stable() {
        use RealizedProvider::*;
        // Exhaustive round-trip (the match forces every variant to be considered).
        for rp in [Cuda, Cpu, CoreMl, Mlx, Fluid, System] {
            assert_eq!(
                RealizedProvider::parse(rp.as_str()),
                rp,
                "round-trip {rp:?}"
            );
        }
        // Pinned wire tokens (a rename here is a deliberate, visible change).
        assert_eq!(Cuda.as_str(), "CUDA");
        assert_eq!(Cpu.as_str(), "CPU");
        assert_eq!(CoreMl.as_str(), "CoreML");
        assert_eq!(Mlx.as_str(), "MLX");
        assert_eq!(Fluid.as_str(), "Fluid");
        assert_eq!(System.as_str(), "System");
        assert_eq!(Provider::Mlx.as_str(), "mlx");
        assert_eq!(Provider::Fluid.as_str(), "fluid");
        assert_eq!(DiarizerProvider::Mlx.as_str(), "mlx");
        assert_eq!(DiarizerProvider::Fluid.as_str(), "fluid");
        assert_eq!(
            DiarizerProvider::parse("fluid"),
            Some(DiarizerProvider::Fluid)
        );
        assert_eq!(Provider::parse("coreml"), Some(Provider::OrtCoreMl));
        assert_eq!(Provider::parse("fluid"), Some(Provider::Fluid));
        // Unknown / casing drift → CPU, never a spurious accelerated-provider claim.
        // `fluid`/`FLUID` are the CONFIG token's casing, not the realized wire token —
        // they must not be mistaken for a realized FluidAudio backend.
        assert_eq!(RealizedProvider::parse("cuda"), Cpu);
        assert_eq!(RealizedProvider::parse("Cuda"), Cpu);
        assert_eq!(RealizedProvider::parse("fluid"), Cpu);
        assert_eq!(RealizedProvider::parse("FLUID"), Cpu);
        assert_eq!(RealizedProvider::parse(""), Cpu);
        assert_eq!(RealizedProvider::parse("bogus"), Cpu);
        // Mapping to the config Provider vocabulary (System has no ort rung → CPU).
        assert_eq!(Cuda.to_provider(), Provider::OrtCuda);
        assert_eq!(CoreMl.to_provider(), Provider::OrtCoreMl);
        assert_eq!(Mlx.to_provider(), Provider::Mlx);
        assert_eq!(Fluid.to_provider(), Provider::Fluid);
        assert_eq!(Cpu.to_provider(), Provider::OrtCpu);
        assert_eq!(System.to_provider(), Provider::OrtCpu);
    }

    #[test]
    fn provider_pref_wants_gpu_tokens() {
        assert!(provider_pref_wants_gpu("cuda"));
        assert!(provider_pref_wants_gpu("CUDA"));
        assert!(provider_pref_wants_gpu("auto"));
        assert!(!provider_pref_wants_gpu("cpu"));
        assert!(!provider_pref_wants_gpu("mlx"));
        assert!(!provider_pref_wants_gpu(""));
    }

    fn arr(toks: &[&str]) -> toml::Value {
        toml::Value::Array(
            toks.iter()
                .map(|s| toml::Value::String(s.to_string()))
                .collect(),
        )
    }
    fn s(tok: &str) -> toml::Value {
        toml::Value::String(tok.to_string())
    }

    /// Intel macOS stopped being a runtime special case once ds-model pinned it a dist.
    #[test]
    fn engine_usability_is_the_static_matrix_on_every_target() {
        assert!(TtsEngine::BuiltIn.tts_usable_on(Os::MacOs, Arch::X64));
        assert!(SttEngine::BuiltIn.stt_usable_on(Os::MacOs, Arch::X64));

        let (os, arch) = (Os::this(), Arch::this());
        for e in TtsEngine::ALL.iter().copied() {
            assert_eq!(e.is_tts_usable(), e.tts_usable_on(os, arch), "{e:?}");
        }
        for e in SttEngine::ALL.iter().copied() {
            assert_eq!(e.is_stt_usable(), e.stt_usable_on(os, arch), "{e:?}");
        }
    }

    #[test]
    fn engine_selection_per_platform() {
        // The FULL cross-platform engine-selection truth table — runnable on any host. For each
        // target, walk the DEFAULT ladders with the pure `_on` predicates and assert what
        // resolves. This is the whole point of the ladder: Windows / Linux run the built-in
        // (Parakeet) engines for both TTS and STT; macOS (any arch) prefers the OS recognizer
        // for STT (`system` is macOS-only-usable and now sits first in the STT ladder) and
        // Kokoro for TTS.
        let resolve_tts = |os: Os, arch: Arch| -> Option<TtsEngine> {
            default_tts_engine_ladder()
                .into_iter()
                .find(|e| e.tts_usable_on(os, arch))
        };
        let resolve_stt = |os: Os, arch: Arch| -> Option<SttEngine> {
            default_stt_engine_ladder()
                .into_iter()
                .find(|e| e.stt_usable_on(os, arch))
        };
        let cases = [
            (
                Os::MacOs,
                Arch::Arm64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::System),
            ),
            // Intel macOS reaches built-in TTS now that it has a pinned ONNX Runtime; STT
            // still prefers the system recognizer, same as Apple silicon.
            (
                Os::MacOs,
                Arch::X64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::System),
            ),
            (
                Os::Windows,
                Arch::X64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::BuiltIn),
            ),
            (
                Os::Windows,
                Arch::Arm64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::BuiltIn),
            ),
            (
                Os::Linux,
                Arch::X64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::BuiltIn),
            ),
            (
                Os::Linux,
                Arch::Arm64,
                Some(TtsEngine::BuiltIn),
                Some(SttEngine::BuiltIn),
            ),
        ];
        for (os, arch, want_tts, want_stt) in cases {
            assert_eq!(resolve_tts(os, arch), want_tts, "TTS on {os:?}/{arch:?}");
            assert_eq!(resolve_stt(os, arch), want_stt, "STT on {os:?}/{arch:?}");
        }
        // Invariant across every target: claude_code is always a usable STT rung (so the
        // default STT ladder never dead-ends).
        for (os, arch) in [
            (Os::MacOs, Arch::X64),
            (Os::Windows, Arch::X64),
            (Os::Linux, Arch::Arm64),
        ] {
            assert!(SttEngine::ClaudeCode.stt_usable_on(os, arch));
        }
        // System STT is macOS any-arch (legacy SFSpeechRecognizer covers Intel/pre-26);
        // System TTS is macOS + Windows only.
        assert!(SttEngine::System.stt_usable_on(Os::MacOs, Arch::Arm64));
        assert!(SttEngine::System.stt_usable_on(Os::MacOs, Arch::X64));
        assert!(!SttEngine::System.stt_usable_on(Os::Windows, Arch::X64));
        assert!(!SttEngine::System.stt_usable_on(Os::Linux, Arch::Arm64));
        assert!(!TtsEngine::System.tts_usable_on(Os::Linux, Arch::X64));
    }

    #[test]
    fn provider_usability_matches_across_stt_and_tts() {
        // ONE per-provider usability matrix, asserted for BOTH the STT and TTS ladders, so the
        // rules can't silently diverge. Columns: expected (is_stt_usable, is_tts_usable)
        // at that (os, arch).
        use Provider::*;
        let cases = [
            // MLX — Apple Silicon only, identically for STT and TTS.
            (Mlx, Os::MacOs, Arch::Arm64, true, true),
            (Mlx, Os::MacOs, Arch::X64, false, false),
            (Mlx, Os::Windows, Arch::X64, false, false),
            (Mlx, Os::Linux, Arch::Arm64, false, false),
            // CPU — the universal rung, every platform, both axes.
            (OrtCpu, Os::MacOs, Arch::X64, true, true),
            (OrtCpu, Os::MacOs, Arch::Arm64, true, true),
            (OrtCpu, Os::Windows, Arch::X64, true, true),
            (OrtCpu, Os::Linux, Arch::Arm64, true, true),
            // CUDA — x86_64 Windows/Linux GPU EP only, both axes; never ARM64 or macOS.
            (OrtCuda, Os::Windows, Arch::X64, true, true),
            (OrtCuda, Os::Linux, Arch::X64, true, true),
            (OrtCuda, Os::Windows, Arch::Arm64, false, false),
            (OrtCuda, Os::Linux, Arch::Arm64, false, false),
            (OrtCuda, Os::MacOs, Arch::Arm64, false, false),
            // Core ML is an Apple-Silicon TTS-only ONNX Runtime provider. On Intel Macs the EP
            // registers and then fails every run, so x86_64 must skip it (issue #250).
            (OrtCoreMl, Os::MacOs, Arch::Arm64, false, true),
            (OrtCoreMl, Os::MacOs, Arch::X64, false, false),
            (OrtCoreMl, Os::Windows, Arch::X64, false, false),
            // FluidAudio — Apple Silicon only, like MLX, identically for STT and TTS.
            (Fluid, Os::MacOs, Arch::Arm64, true, true),
            (Fluid, Os::MacOs, Arch::X64, false, false),
            (Fluid, Os::Windows, Arch::X64, false, false),
            (Fluid, Os::Linux, Arch::Arm64, false, false),
        ];
        for (p, os, arch, want_stt, want_tts) in cases {
            assert_eq!(
                p.stt_usable_on(os, arch),
                want_stt,
                "stt {p:?} {os:?}/{arch:?}"
            );
            assert_eq!(
                p.tts_usable_on(os, arch),
                want_tts,
                "tts {p:?} {os:?}/{arch:?}"
            );
        }
    }

    /// The diarizer's platform gate, as the full cross-platform matrix (runnable on any host)
    /// plus host agreement — §10's `ds-config` matrix test. Both rungs are Apple-Silicon macOS
    /// only, so a `diarizer=["fluid"]` config skips the rung everywhere else, exactly like MLX.
    #[test]
    fn diarizer_usability_matches_the_provider_matrix() {
        use DiarizerProvider::*;
        let cases = [
            (Mlx, Os::MacOs, Arch::Arm64, true),
            (Mlx, Os::MacOs, Arch::X64, false),
            (Mlx, Os::Windows, Arch::X64, false),
            (Mlx, Os::Linux, Arch::Arm64, false),
            (Fluid, Os::MacOs, Arch::Arm64, true),
            (Fluid, Os::MacOs, Arch::X64, false),
            (Fluid, Os::Windows, Arch::X64, false),
            (Fluid, Os::Linux, Arch::Arm64, false),
        ];
        for (p, os, arch, want) in cases {
            assert_eq!(
                p.diarizer_usable_on(os, arch),
                want,
                "{p:?} {os:?}/{arch:?}"
            );
        }
        // Host agreement: the pub `is_diarizer_usable` (which the live diarizer walks) must
        // match the pure form for THIS (os, arch), so the two spellings never drift.
        let (os, arch) = (Os::this(), Arch::this());
        for p in DiarizerProvider::ALL.iter().copied() {
            assert_eq!(
                p.is_diarizer_usable(),
                p.diarizer_usable_on(os, arch),
                "{p:?}"
            );
        }
    }

    /// The `fluid` rung is per-MODEL, not just per-platform: FluidAudio ships a Core ML
    /// Kokoro and nothing else, so a `fluid`-first ladder must resolve Kokoro to Fluid and
    /// fall the other three through to the next usable rung — the descriptor filter in
    /// `resolved_tts_provider` doing the work, with no model-specific branch anywhere.
    #[test]
    fn fluid_is_skipped_when_the_model_does_not_support_it() {
        use crate::TtsModel;
        let ladder = [Provider::Fluid, Provider::Mlx, Provider::OrtCpu];
        let resolve = |model: TtsModel, os: Os, arch: Arch| {
            let descriptor = model.descriptor();
            ladder
                .iter()
                .copied()
                .find(|p| p.tts_usable_on(os, arch) && descriptor.supports_provider(*p))
                .unwrap_or(Provider::OrtCpu)
        };
        assert_eq!(
            resolve(TtsModel::Kokoro, Os::MacOs, Arch::Arm64),
            Provider::Fluid
        );
        for model in [TtsModel::Chatterbox, TtsModel::Qwen, TtsModel::OmniVoice] {
            assert_eq!(
                resolve(model, Os::MacOs, Arch::Arm64),
                Provider::Mlx,
                "{} has no FluidAudio export and must fall through",
                model.as_str()
            );
        }
        // Intel macOS has no ANE, so even Kokoro skips the rung.
        for model in TtsModel::ALL.iter().copied() {
            assert_eq!(resolve(model, Os::MacOs, Arch::X64), Provider::OrtCpu);
            assert_eq!(resolve(model, Os::Windows, Arch::X64), Provider::OrtCpu);
        }
    }

    /// Pins the deliberate decision NOT to put `fluid` in the out-of-box ladder: it covers
    /// one of four TTS models, so leading with it would make the default backend
    /// model-dependent, and it would add its Core ML sets to every clean install.
    /// Selecting it is an explicit `set_config provider=["fluid", ...]`.
    #[test]
    fn default_provider_ladder_does_not_include_fluid() {
        assert_eq!(
            default_provider(),
            vec![Provider::Mlx, Provider::OrtCuda, Provider::OrtCpu]
        );
        assert!(!default_provider().contains(&Provider::Fluid));
        // It is still a parseable, selectable token — just not a default one.
        assert!(Provider::ALL.contains(&Provider::Fluid));
    }

    #[test]
    fn provider_ladder_falls_back_per_platform() {
        // A cuda-FIRST ladder (`["cuda", "cpu"]`): CUDA leads, with `cpu` as the
        // universal fallback. CUDA is a real rung on Windows AND Linux (x86_64 ONNX-runtime GPU
        // EP); on macOS the resolver must SKIP it and land on the next usable rung — proving an
        // `cuda` first item degrades gracefully instead of dead-ending. This is the
        // cross-platform analogue of the live `model_status` check (which showed `cpu` on macOS).
        let ladder = [Provider::OrtCuda, Provider::OrtCpu];
        // Pin x86_64 here; the full provider matrix above covers the ARM64 CUDA gate.
        let resolve_tts = |os: Os| {
            ladder
                .iter()
                .copied()
                .find(|p| p.tts_usable_on(os, Arch::X64))
        };
        let resolve_stt = |os: Os| {
            ladder
                .iter()
                .copied()
                .find(|p| p.stt_usable_on(os, Arch::X64))
        };
        // Windows + Linux: CUDA is usable → it WINS (no fallback).
        for os in [Os::Windows, Os::Linux] {
            assert_eq!(resolve_tts(os), Some(Provider::OrtCuda), "tts {os:?}");
            assert_eq!(resolve_stt(os), Some(Provider::OrtCuda), "stt {os:?}");
        }
        // macOS: CUDA is NOT usable → fall back to the next rung, `cpu`.
        assert_eq!(resolve_tts(Os::MacOs), Some(Provider::OrtCpu));
        assert_eq!(resolve_stt(Os::MacOs), Some(Provider::OrtCpu));
        // A lone `["cuda"]` ladder dead-ends on macOS — the RESOLVER (`resolved_*_provider`)
        // is what supplies the `OrtCpu` default there; the raw `find` returns None, confirming
        // CUDA itself is genuinely unusable (not silently treated as usable).
        let cuda_only = [Provider::OrtCuda];
        assert_eq!(
            cuda_only
                .iter()
                .copied()
                .find(|p| p.tts_usable_on(Os::MacOs, Arch::Arm64)),
            None
        );
        // (The MLX Apple-Silicon rule and full per-provider matrix live in
        // `provider_usability_matches_across_stt_and_tts` — one table, both axes.)
        // Host agreement: the cfg-gated host fns must match the pure `_on` for THIS (os, arch),
        // so the two never drift (the live engine walks the host fns).
        let (os, arch) = (Os::this(), Arch::this());
        for p in Provider::ALL.iter().copied() {
            assert_eq!(p.is_stt_usable(), p.stt_usable_on(os, arch), "stt {p:?}");
            assert_eq!(p.is_tts_usable(), p.tts_usable_on(os, arch), "tts {p:?}");
        }
    }

    #[test]
    fn tts_ladder_parsing() {
        // Array: known tokens, in order, deduped.
        assert_eq!(
            parse_tts_ladder(&arr(&["system", "built_in", "system"])),
            vec![TtsEngine::System, TtsEngine::BuiltIn]
        );
        // An empty array yields EMPTY (= off).
        assert!(parse_tts_ladder(&arr(&[])).is_empty());
        // Unknown tokens drop; an explicit all-unknown array stays EMPTY (NOT the default —
        // the user gave an array, so we honor its emptiness).
        assert_eq!(
            parse_tts_ladder(&arr(&["festival", "built_in"])),
            vec![TtsEngine::BuiltIn]
        );
        assert!(parse_tts_ladder(&arr(&["festival"])).is_empty());
        // ARRAYS ONLY: a bare scalar string is NO LONGER a one-rung shorthand — any scalar
        // (known token or unknown) degrades to the default ladder. `[]` is the only
        // way to disable.
        assert_eq!(parse_tts_ladder(&s("system")), default_tts_engine_ladder());
        assert_eq!(
            parse_tts_ladder(&s("festival")),
            default_tts_engine_ladder()
        );
        // A non-string/array scalar (bool/int) → the default ladder.
        assert_eq!(
            parse_tts_ladder(&toml::Value::Integer(3)),
            default_tts_engine_ladder()
        );
        assert_eq!(
            parse_tts_ladder(&toml::Value::Boolean(true)),
            default_tts_engine_ladder()
        );
    }

    #[test]
    fn stt_ladder_parsing() {
        // Array drops dupes, keeps order; claude_code is a normal rung here.
        assert_eq!(
            parse_stt_ladder(&arr(&["claude_code", "built_in", "claude_code"])),
            vec![SttEngine::ClaudeCode, SttEngine::BuiltIn]
        );
        // ARRAYS ONLY: a bare scalar string (known token or unknown) is NO LONGER a
        // one-rung shorthand — it degrades to the default ladder. `[]` is the only disable.
        assert_eq!(
            parse_stt_ladder(&s("claude_code")),
            default_stt_engine_ladder()
        );
        assert_eq!(
            parse_stt_ladder(&s("deepgram")),
            default_stt_engine_ladder()
        );
        assert_eq!(
            parse_stt_ladder(&toml::Value::Boolean(false)),
            default_stt_engine_ladder()
        );
    }
}
