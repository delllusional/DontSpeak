//! Phase-2 engine-selection token enums + their fail-open / strict / serialize
//! plumbing.
//!
//! This module is declared FIRST in `lib.rs` so the declarative macros it defines
//! (`fail_open_de!`, `serialize_as_str!`, `strict_de!`) are textually in scope for
//! everything that follows. The macros are kept private to the crate (textual scope
//! via `#[macro_use]` on the `mod enums;` declaration).

use ds_client::ClientSource;
use serde::{Deserialize, Deserializer};

// ─────────────────────────────────────────────────────────────────────────────
// Phase-2 engine selection enums (§A, Config Schema)
//
// The scalar engine enums (`SttEngine`, `TtsEngine`, `ListenMode`) are `#[default]`-tagged
// and parsed through a fail-open `de_*` deserializer so an absent OR typo'd value degrades
// to the default rather than erroring the whole config block — preserving the "unset ==
// today" invariant for hand-edited config files. The set/ladder fields (`narrate`,
// `tray_indicator`, `provider`, `diarizer_provider`, `input_clears`) are `Vec`s with
// their own fail-open Vec deserializers (`de_narrate` &c.); the element enums here (incl.
// `CancelSpeechScope`) are their building blocks.
// ─────────────────────────────────────────────────────────────────────────────

/// STT backend selection. Default `BuiltIn` — DontSpeak's own on-device Parakeet model
/// (each backend, incl. the `claude_code` delegation, is documented on its variant below).
/// "Off" is structural now, not a token: the `stt_engine` preference is `Option<Vec<SttEngine>>`,
/// with `Some(vec![])` meaning explicit off — there is no `SttEngine::Off` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SttEngine {
    /// DontSpeak's BUILT-IN local STT: a cache-aware streaming FastConformer transducer. The
    /// RUNTIME (cross-platform ONNX via `ort`, or macOS FluidAudio Core ML / ANE) is
    /// selected by the shared [`Provider`] — exactly as it drives Kokoro TTS. The factory
    /// degrades it to ClaudeCode when the model is unavailable. Token `built_in`. The DEFAULT
    /// (on-device dictation).
    #[default]
    BuiltIn,
    /// Local on-device STT via the OS recognizer: macOS `SFSpeechRecognizer` (en-US,
    /// `requiresOnDeviceRecognition`), run through the warm helper like Parakeet.
    /// Availability is gated on the recognizer being authorized + on-device-capable; the
    /// engine returns the INERT `SystemStt` (never claude_code) when unavailable, so
    /// selecting it never silently degrades. Deferred on Windows/Linux (unavailable).
    System,
    /// Delegate to Claude Code's built-in voice dictation: DontSpeak READS Claude Code's
    /// `keybindings.json` for the key bound to `voice:pushToTalk` (default `Space`) and
    /// synthesizes it — never writing Claude Code's config. Claude Code does the (cloud)
    /// transcription. Token `claude_code`. (Last — the only non-on-device, non-symmetric
    /// option; `off`/`built_in`/`system` mirror `tts_engine`.)
    ClaudeCode,
}

impl SttEngine {
    /// All variants in canonical-token order — the single source the tool-catalog parity
    /// test checks schema `enum` arrays against. Ordered to mirror `tts_engine`
    /// (built_in · system), with `claude_code` appended (STT-only). All settable via
    /// set_config; `system` is additionally availability-gated at the MCP layer.
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

    /// The canonical snake_case token this engine serializes to. EXACTLY one of
    /// the values `parse()` accepts, so a `write_settings` → `load` round-trip is
    /// identity. Single source of truth shared by the writer and the GUI's
    /// segmented-index mapping.
    pub fn as_str(self) -> &'static str {
        match self {
            SttEngine::ClaudeCode => "claude_code",
            SttEngine::BuiltIn => "built_in",
            SttEngine::System => "system",
        }
    }

    /// Whether this engine can serve STT on the CURRENT build/platform — the predicate the
    /// `stt_engine` preference ladder is walked with (see [`crate::VoiceConfig::resolved_stt`]).
    /// A STATIC preference (like `Provider::is_stt_usable`): runtime model/authorization gating
    /// still applies downstream, but a rung that can NEVER run in this build is skipped so the
    /// ladder falls through to the next. `built_in` (Parakeet) needs the ONNX/Core-ML stack,
    /// absent on x86_64 macOS; `system` (Apple's on-device recognizer) is macOS-only;
    /// `claude_code` always works (it delegates to Claude Code, no native deps). `off` never
    /// appears in a resolved ladder.
    pub fn is_stt_usable(self) -> bool {
        // Intel macOS: built-in (Parakeet) ONNX STT is a RUNTIME capability (an onnxruntime dylib —
        // Homebrew keg / ORT_DYLIB_PATH), invisible to the static `(os, arch)` matrix. Absent ⇒ this
        // rung is skipped and the ladder falls through to `claude_code`.
        if matches!(self, SttEngine::BuiltIn)
            && cfg!(all(target_os = "macos", target_arch = "x86_64"))
        {
            return intel_mac_builtin_ort_available();
        }
        self.stt_usable_on(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// [`is_stt_usable`](SttEngine::is_stt_usable) as a pure function of the target `(os, arch)` —
    /// the single source the resolver and the cross-platform tests both walk.
    pub(crate) fn stt_usable_on(self, os: &str, arch: &str) -> bool {
        match self {
            SttEngine::BuiltIn => built_in_usable_on(os, arch),
            SttEngine::System => system_stt_buildable_on(os, arch),
            SttEngine::ClaudeCode => true,
        }
    }
}

/// TTS backend selection. Default is the native in-process neural engine.
///
/// Its config TOKEN is `built_in` — the speech-OUT mirror of [`SttEngine::BuiltIn`]
/// (system × built_in for both STT and TTS). `kokoro` stays the MODEL / voice-family
/// brand name (voice listings, the `model_status` row, the GUI label), exactly as
/// `parakeet` is the BuiltIn STT engine's model name. So the *setting* is `built_in`
/// while the voices remain "Kokoro". The variant keeps the brand name (`Kokoro`) since
/// most code reads it as the model identity; only `as_str` maps it to the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TtsEngine {
    #[default]
    Kokoro,
    System,
}

impl TtsEngine {
    /// All variants (canonical-token order); single source for the catalog parity test.
    pub const ALL: &'static [TtsEngine] = &[TtsEngine::Kokoro, TtsEngine::System];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "built_in" => Some(TtsEngine::Kokoro),
            "system" => Some(TtsEngine::System),
            _ => None,
        }
    }

    /// The canonical lowercase config TOKEN this engine serializes to (round-trips
    /// through `parse()`). The neural engine is `built_in`; "kokoro" is its MODEL/brand
    /// name, surfaced separately (voice listings, `model_status`) — not this token.
    pub fn as_str(self) -> &'static str {
        match self {
            TtsEngine::Kokoro => "built_in",
            TtsEngine::System => "system",
        }
    }

    /// Whether this engine can serve TTS on the CURRENT build/platform — the predicate the
    /// `tts_engine`/`tts_engine_ladder` preference is walked with (see
    /// [`crate::VoiceConfig::resolved_tts`]). A STATIC preference (like
    /// `Provider::is_tts_usable`). `built_in` (Kokoro) needs the ONNX/Core-ML stack, absent
    /// on x86_64 macOS; `system` (macOS `say` / Windows SAPI) is available on macOS + Windows
    /// (no system synth wired on Linux).
    pub fn is_tts_usable(self) -> bool {
        // Intel macOS: built-in (Kokoro) ONNX TTS is a RUNTIME capability (an onnxruntime dylib —
        // Homebrew keg / ORT_DYLIB_PATH), invisible to the static `(os, arch)` matrix. Absent ⇒ this
        // rung is skipped and the ladder falls through to `system` (`say`).
        if matches!(self, TtsEngine::Kokoro)
            && cfg!(all(target_os = "macos", target_arch = "x86_64"))
        {
            return intel_mac_builtin_ort_available();
        }
        self.tts_usable_on(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// [`is_tts_usable`](TtsEngine::is_tts_usable) as a pure function of the target `(os, arch)` —
    /// the single source the resolver and the cross-platform tests both walk.
    pub(crate) fn tts_usable_on(self, os: &str, arch: &str) -> bool {
        match self {
            TtsEngine::Kokoro => built_in_usable_on(os, arch),
            TtsEngine::System => system_tts_buildable_on(os),
        }
    }

    /// The voice-facing BRAND name (`"kokoro"`/`"system"`) used in voice listings, the
    /// session-voice snapshot/protocol, and `status` output — the MODEL/voice-family
    /// identity, distinct from [`as_str`](TtsEngine::as_str)'s config TOKEN (which is
    /// `built_in` for the neural engine). Single source of truth: both the MCP client and
    /// the engine read this so the brand never drifts.
    pub fn brand(self) -> &'static str {
        match self {
            TtsEngine::Kokoro => "kokoro",
            TtsEngine::System => "system",
        }
    }
}

/// Voice input mode (§always-listening). Default `RecordSubmit` == today's
/// Caps-Lock push-to-talk (record then submit). `Always` is the hands-free
/// continuous loop: mic open whenever Kokoro isn't speaking, submit driven by a
/// stopword + trailing-silence confirmation. See docs/ALWAYS-LISTENING.md.
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

    /// The canonical snake_case token this mode serializes to (round-trips through
    /// `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            ListenMode::RecordSubmit => "record_submit",
            ListenMode::Always => "always",
        }
    }
}

/// One rung of the speaker-diarization runtime ladder — the "who spoke when" analogue of
/// [`Provider`]. The model pair (Pyannote segmentation + WeSpeaker embeddings) runs through
/// FluidAudio's Core ML / ANE engine. The `diarizer_provider` field is a
/// `Vec<DiarizerProvider>` ladder that doubles as the on/off switch: EMPTY = diarization off
/// (the default), non-empty = on, with the first platform-usable rung winning (see
/// `default_diarizer_provider` and [`crate::VoiceConfig::resolved_diarizer_provider`]). There is NO
/// separate enable flag. Diarization is macOS-only for now (the single `apple_native` rung).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiarizerProvider {
    /// macOS: FluidAudio's Core ML / ANE diarizer. Not usable off macOS (skip it there).
    #[default]
    AppleNative,
}

impl DiarizerProvider {
    /// All variants (canonical-token order); single source for the catalog parity test.
    pub const ALL: &'static [DiarizerProvider] = &[DiarizerProvider::AppleNative];

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "apple_native" => Some(DiarizerProvider::AppleNative),
            _ => None,
        }
    }

    /// The canonical token this provider serializes to (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            DiarizerProvider::AppleNative => "apple_native",
        }
    }

    /// Whether this rung can run diarization on the CURRENT platform — the predicate the
    /// `diarizer_provider` priority array is walked with (see
    /// [`crate::VoiceConfig::resolved_diarizer_provider`]). `apple_native` is macOS-only.
    pub(crate) fn is_diarizer_usable(self) -> bool {
        match self {
            DiarizerProvider::AppleNative => cfg!(target_os = "macos"),
        }
    }
}

/// One rung of the SHARED on-device compute ladder for BOTH the Kokoro TTS and Parakeet
/// STT models (§A.1). The `provider` field is a `Vec<Provider>` priority ladder (default
/// ANE → CUDA → CPU, see `default_provider`); each engine walks it and skips rungs it
/// can't use on this platform (macOS STT lands on ANE; Windows/Linux x86_64 STT follows CUDA →
/// CPU like TTS, since Parakeet gained a CUDA EP). The `Ort*` variants are ONNX Runtime execution
/// providers (Kokoro drives its own ort session, so each is a real choice): `OrtCuda`
/// (NVIDIA GPU) triggers a one-time ~1.4 GB GPU-runtime download; `OrtCoreMl` is the ort
/// CoreML EP (macOS). `Ane` is the one NON-ort backend (FluidAudio native Core ML on the
/// Apple Neural Engine). The element default is `OrtCpu` (the always-available rung).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provider {
    /// ONNX Runtime, CPU execution provider.
    #[default]
    OrtCpu,
    /// ONNX Runtime, CUDA execution provider (NVIDIA GPU) — drives BOTH ort engines over the
    /// shared warm-helper runtime: Kokoro TTS (`synth.rs`) and the streaming Parakeet STT runner
    /// (`streaming.rs`), each registering the CUDA EP best-effort with a CPU fallback. (STT ships
    /// int8 models, so ORT places what it can on the GPU and runs the int8 ops it can't per-op on
    /// CPU — the token is still the realized runtime, gated on the GPU runtime being present.)
    OrtCuda,
    /// ONNX Runtime, CoreML execution provider (macOS) — ort offloading ops to Core ML.
    /// TTS only; explicit (NOT in the default ladder, as it benches slower than CPU for
    /// Kokoro). STT has no CoreML EP.
    OrtCoreMl,
    /// macOS: FluidAudio's native Core ML model pinned to the Apple Neural Engine (ANE) —
    /// a NATIVE backend, distinct from `OrtCoreMl` (the ort CoreML EP); it bypasses ONNX
    /// Runtime entirely. Used by BOTH engines on macOS. Falls back to ort CPU off macOS.
    Ane,
}

impl Provider {
    /// All variants (canonical-token order); single source for the catalog parity test.
    pub const ALL: &'static [Provider] = &[
        Provider::Ane,
        Provider::OrtCuda,
        Provider::OrtCoreMl,
        Provider::OrtCpu,
    ];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(Provider::OrtCpu),
            "cuda" => Some(Provider::OrtCuda),
            "coreml" => Some(Provider::OrtCoreMl),
            "ane" => Some(Provider::Ane),
            _ => None,
        }
    }

    /// The canonical lowercase token (round-trips through `parse()`, and is the exact
    /// string the warm child's `DONTSPEAK_PROVIDER` env expects).
    pub fn as_str(self) -> &'static str {
        match self {
            Provider::OrtCpu => "cpu",
            Provider::OrtCuda => "cuda",
            Provider::OrtCoreMl => "coreml",
            Provider::Ane => "ane",
        }
    }

    /// Whether this rung can serve STT on the CURRENT platform — the predicate the
    /// `provider` priority array is walked with (see [`crate::VoiceConfig::resolved_stt_provider`]).
    /// macOS: ANE (FluidAudio native) or CPU. Windows/Linux: CUDA (Parakeet GPU EP) or CPU.
    /// Elsewhere: CPU only. (`OrtCoreMl` is TTS-only — never STT.)
    pub(crate) fn is_stt_usable(self) -> bool {
        self.stt_usable_on(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// [`is_stt_usable`](Provider::is_stt_usable) as a PURE function of the target `(os, arch)` — the
    /// single source the resolver and the cross-platform fallback tests both walk (so an `cuda`
    /// first rung is provably skipped off Windows on a single host). Provider-keyed so the ANE
    /// Apple-Silicon rule ([`ane_usable_on`]) is SHARED with [`tts_usable_on`] and the two
    /// can't drift. `OrtCpu` runs everywhere; `OrtCuda` is the Windows/Linux GPU EP;
    /// `OrtCoreMl` is TTS-only (never STT).
    pub(crate) fn stt_usable_on(self, os: &str, arch: &str) -> bool {
        match self {
            Provider::OrtCpu => true,
            Provider::Ane => ane_usable_on(os, arch),
            Provider::OrtCuda => matches!(os, "windows" | "linux"),
            Provider::OrtCoreMl => false,
        }
    }

    /// Whether this rung can serve TTS (Kokoro) on the CURRENT platform. macOS: ANE
    /// (native), the ort CoreML EP, or CPU. Windows/Linux: CUDA or CPU. Elsewhere: CPU only.
    pub(crate) fn is_tts_usable(self) -> bool {
        self.tts_usable_on(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// [`is_tts_usable`](Provider::is_tts_usable) as a PURE function of the target `(os, arch)` — see
    /// [`stt_usable_on`](Provider::stt_usable_on). Same shape, plus the macOS-only `OrtCoreMl`
    /// EP; the ANE Apple-Silicon rule is the SAME [`ane_usable_on`] the STT path uses.
    pub(crate) fn tts_usable_on(self, os: &str, arch: &str) -> bool {
        match self {
            Provider::OrtCpu => true,
            Provider::Ane => ane_usable_on(os, arch),
            Provider::OrtCuda => matches!(os, "windows" | "linux"),
            Provider::OrtCoreMl => os == "macos",
        }
    }
}

/// ANE (FluidAudio's Core ML on the Neural Engine) is Apple-Silicon ONLY — Intel Macs have
/// no Neural Engine and ship no arm64 shim, so the FluidAudio backend can't load there. The
/// SINGLE source of this rule for BOTH [`Provider::stt_usable_on`] and
/// [`Provider::tts_usable_on`]: when they diverged, an Intel mac's default
/// `["ane", …]` ladder resolved to the dead `ane` rung and silently fell back to a slower
/// path (offline STT loop / ONNX TTS), so they must stay in lockstep.
fn ane_usable_on(os: &str, arch: &str) -> bool {
    os == "macos" && arch == "aarch64"
}

/// Whether an execution-provider PREFERENCE token (the `DONTSPEAK_PROVIDER` /
/// `DONTSPEAK_STT_PROVIDER` value a warm child reads) requests the NVIDIA GPU. THE single
/// definition, shared by Kokoro TTS and Parakeet STT so the "wants CUDA?" decision can't drift per
/// engine (`ds_model::cuda_session_builder` and both engines' load paths route through it).
pub fn provider_pref_wants_gpu(pref: &str) -> bool {
    pref.eq_ignore_ascii_case("cuda") || pref.eq_ignore_ascii_case("auto")
}

/// The REALIZED on-device execution provider a warm-child engine ACTUALLY loaded on — the SINGLE
/// source of truth for the "what actually ran" token vocabulary that Kokoro TTS and Parakeet STT
/// report across the process boundary (the child's `PROVIDER` / `STT_PROVIDER` stdout line).
///
/// Distinct from [`Provider`] (the config PREFERENCE ladder, lowercase tokens): this is the realized
/// backend — it includes `CoreMlAne`/`System`, which have no config-ladder rung — and serializes as
/// the canonical UPPERCASE token via [`as_str`](RealizedProvider::as_str). Producers return this
/// enum and stringify ONCE at the IPC edge; the status layer [`parse`](RealizedProvider::parse)s it
/// back and maps to a config [`Provider`] via [`to_provider`](RealizedProvider::to_provider) — so a
/// token typo is a compile error, not a silent "UI claims CUDA but runs CPU" mislabel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealizedProvider {
    /// ONNX Runtime CUDA execution provider (NVIDIA GPU).
    Cuda,
    /// ONNX Runtime CPU execution provider — the universal fallback.
    Cpu,
    /// ONNX Runtime CoreML execution provider (macOS ort offload).
    CoreMl,
    /// FluidAudio native Core ML on the Apple Neural Engine (macOS).
    CoreMlAne,
    /// macOS on-device `SFSpeechRecognizer` (the `system` STT engine; no ort runtime).
    System,
}

impl RealizedProvider {
    /// The canonical UPPERCASE wire token (round-trips through [`parse`](RealizedProvider::parse)).
    pub fn as_str(self) -> &'static str {
        match self {
            RealizedProvider::Cuda => "CUDA",
            RealizedProvider::Cpu => "CPU",
            RealizedProvider::CoreMl => "CoreML",
            RealizedProvider::CoreMlAne => "CoreML-ANE",
            RealizedProvider::System => "System",
        }
    }

    /// Parse a wire token; anything unrecognized → [`Cpu`](RealizedProvider::Cpu) (a missing/renamed
    /// token degrades to CPU, never a false GPU claim).
    pub fn parse(s: &str) -> Self {
        match s {
            "CUDA" => RealizedProvider::Cuda,
            "CoreML" => RealizedProvider::CoreMl,
            "CoreML-ANE" => RealizedProvider::CoreMlAne,
            "System" => RealizedProvider::System,
            _ => RealizedProvider::Cpu,
        }
    }

    /// Map to the config [`Provider`] token vocabulary for the status row (`System` has no ort rung,
    /// so it reads as CPU — the OS recognizer isn't an ort runtime).
    pub fn to_provider(self) -> Provider {
        match self {
            RealizedProvider::Cuda => Provider::OrtCuda,
            RealizedProvider::CoreMl => Provider::OrtCoreMl,
            RealizedProvider::CoreMlAne => Provider::Ane,
            RealizedProvider::Cpu | RealizedProvider::System => Provider::OrtCpu,
        }
    }
}

impl std::fmt::Display for RealizedProvider {
    /// Writes the canonical wire token — so `println!("PROVIDER {}", p)` and the like stay terse.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which live state colors the menu-bar icon AND whether it breathes — a SET (config
/// `tray_indicator = ["stt", "tts_animated"]`). Each state (mic `stt` / voice `tts`) appears
/// in at most ONE form: the plain token colors the pill STATICALLY, the `_animated` token
/// colors it AND pulses (breathing). DEFAULT = `["stt", "tts_animated"]` (mic static, voice
/// animated); `[]` = never color. The app reads the set (the engine passes it through in
/// `model_status` — it never acts on it). Animation is currently a macOS effect; other UIs
/// just treat the `_animated` tokens as colored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrayKind {
    Stt,         // mic / recording — colored, static
    Tts,         // voice / speaking — colored, static
    SttAnimated, // recording — colored + breathing
    TtsAnimated, // speaking — colored + breathing
}

impl TrayKind {
    /// All variants (canonical-token order); single source for the catalog parity test.
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

    /// The canonical lowercase token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            TrayKind::Stt => "stt",
            TrayKind::Tts => "tts",
            TrayKind::SttAnimated => "stt_animated",
            TrayKind::TtsAnimated => "tts_animated",
        }
    }

    /// The mic/recording state (static or animated form); else it's the voice/speaking state.
    pub(crate) fn is_stt(self) -> bool {
        matches!(self, TrayKind::Stt | TrayKind::SttAnimated)
    }
    /// Whether this entry breathes (the `_animated` form).
    pub(crate) fn animated(self) -> bool {
        matches!(self, TrayKind::SttAnimated | TrayKind::TtsAnimated)
    }
}

/// Normalize a tray-indicator set to at most ONE token per state, in canonical order
/// (stt, then tts), with the ANIMATED form winning if both forms of a state are present.
/// `[]` stays empty (never color). Used by both the config deserialize and `set_config`.
pub fn normalize_tray_indicator(kinds: Vec<TrayKind>) -> Vec<TrayKind> {
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

/// WHOSE still-pending speech a submit cancels — a SET (config
/// `input_clears = ["current", "other"]`), not a single mode. ANY submit (typed +
/// Enter, or a Caps-Lock dictation / hands-free submit — how you submitted no longer
/// matters) triggers the check. The cancel is permanent (pruned + the in-flight item
/// cancelled, not paused/resumable). Two INDEPENDENT members:
///   `current` — cancel the SUBMITTING window's own pending speech.
///   `other`   — cancel every OTHER session's pending speech, INCLUDING untagged/global
///               audio (e.g. the MCP `speak` tool with no session) — "other" means
///               "everything that isn't current".
/// An EMPTY set means submitting never cancels anything; pending speech plays to the
/// end. See [`crate::VoiceConfig::input_clears`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSpeechScope {
    Current,
    Other,
}

impl CancelSpeechScope {
    /// All variants (canonical-token order); single source for the catalog parity test.
    pub const ALL: &'static [CancelSpeechScope] =
        &[CancelSpeechScope::Current, CancelSpeechScope::Other];

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "current" => Some(CancelSpeechScope::Current),
            "other" => Some(CancelSpeechScope::Other),
            _ => None,
        }
    }

    /// The canonical lowercase token (round-trips through `parse()`).
    pub fn as_str(self) -> &'static str {
        match self {
            CancelSpeechScope::Current => "current",
            CancelSpeechScope::Other => "other",
        }
    }
}

/// What narration speaks — a SET (config `narrate = ["shorts", "digests"]`), not a single
/// mode. Two INDEPENDENT options that combine: `Digests` voices the spoken blockquotes Claude
/// writes (and injects the narration spec asking for them, so long replies get a spoken digest);
/// `Shorts` voices a SHORT reply that has NO blockquote — on its own, lightly cleaned — so brief
/// one-liners are heard even when there's no digest. Both ⇒ everything is spoken. An EMPTY set
/// means narrate nothing (the old "off"). `Digests` is also what gates the injected spec —
/// without it `provide` returns nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NarrateKind {
    Digests,
    Shorts,
}

impl NarrateKind {
    /// All variants (canonical-token order); single source for the catalog parity test.
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

// The client IDENTITY enum (`ClientSource`) that used to live here as `WireTarget` now lives
// in the `ds-client` leaf crate — `ds-log` and `ds-ipc` both need it, and neither may depend
// on `ds-config` (ds-config depends on ds-log ⇒ a cycle). `ds-config` re-exports it from
// `lib.rs`, so `ds_config::ClientSource` keeps working for every downstream crate.
//
// It is no longer a CLIENT-only enum: `ClientSource::CLIENTS` is the wire-able subset (the
// four clients the registry pins), and `DontSpeak` / `Unknown` are members too. Any path that
// means "a client we wire" must therefore gate on `is_client()` — see `de_exclude_clients`
// below, and `ds_wire::run`'s registry-lookup guard.

/// Parse an array of enum tokens in order, dropping unknowns and duplicates. `None` preserves
/// the distinction between a non-array value (use the field's fail-open fallback) and an empty
/// or all-unknown array (whose meaning differs by field).
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

/// Fail-open deserialize for `exclude_clients` (config-file). A present ARRAY ⇒
/// `Some(known CLIENT tokens, in order, deduped; unknown/non-client tokens dropped)`.
/// A present NON-array value ⇒ `None` (defer to all-supported). ABSENT ⇒ `None` via the
/// field's serde default. Modeled on [`de_narrate`], wrapped in `Option` to preserve the
/// tri-state (`None` = all supported vs `Some([])` = none).
///
/// The `is_client()` filter is LOAD-BEARING, not decoration: `ClientSource::parse` now accepts
/// `dontspeak` and `unknown` (where the old `WireTarget::parse` returned `None` for both), so
/// without it `exclude_clients = ["dontspeak"]` would put DontSpeak itself into a set that is
/// only ever consulted to decide which CLIENT to unwire. Pinned by
/// `exclude_clients_drops_non_client_tokens`.
pub(crate) fn de_exclude_clients<'de, D>(d: D) -> Result<Option<Vec<ClientSource>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(fail_open_vec!(&v, ClientSource, |token| {
        ClientSource::parse(token).filter(|client| client.is_client())
    }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Macros: fail-open deserialize, serialize-as-token, strict deserialize.
// (Textually scoped — see the module doc on why `enums` is declared first in `lib.rs`.)
// ─────────────────────────────────────────────────────────────────────────────

macro_rules! fail_open_de {
    ($fn_name:ident, $ty:ty) => {
        /// Fail-open deserialize: any unknown / wrong-typed value degrades to the
        /// type's `Default` rather than erroring the whole block. Uses `toml::Value`
        /// as the format-agnostic scratch (the config file is TOML); a value that
        /// can't be represented — or isn't a string token — falls back to default.
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
/// Default `input_clears` (when the config key is ABSENT): cancel the CURRENT
/// session's speech on any submit, so starting a new prompt (typed or spoken) silences
/// that terminal's still-pending reply, while every other terminal plays on. An explicit
/// `input_clears = []` still means "never cancel" (plays to the end).
pub(crate) fn default_input_clears() -> Vec<CancelSpeechScope> {
    vec![CancelSpeechScope::Current]
}
/// Fail-open array deserialize for `input_clears` (config file): an ARRAY keeps its
/// known tokens in order (deduped) and drops unknown ones; an EMPTY array means never
/// cancel (plays to the end). Any NON-array value (an absent field is handled by the
/// serde default; a stray wrong-typed TOML value) fails open to
/// [`default_input_clears`] (`[Current]`) — matching the documented default and the
/// same fail-open convention every sibling ladder field in this file uses, rather than
/// silently degrading to "never cancel", the semantic opposite of the default.
pub(crate) fn de_input_clears<'de, D>(d: D) -> Result<Vec<CancelSpeechScope>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(
        fail_open_vec!(&v, CancelSpeechScope, CancelSpeechScope::parse)
            .unwrap_or_else(default_input_clears),
    )
}

/// Serialize an enum as its canonical `as_str()` token — the inverse of the
/// `parse()` the fail-open deserializers use, so a `VoiceConfig` round-trips through
/// TOML. (`CaptureGain` has its own Serialize: a string `"auto"` or a number.)
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
// NOTE: `ClientSource` (the old `WireTarget`) is NOT in this list — it owns its Serialize in
// `ds-client` (same as-the-token behaviour, hand-written so that leaf crate needs no macro).

/// STRICT `Deserialize` for a token enum: an unrecognized value ERRORS (listing the
/// valid tokens) instead of failing open to the default. This is the opposite of the
/// `fail_open_de!` deserializers above — and deliberately so. The config FILE wants
/// fail-open (a hand-edited typo shouldn't brick the whole block), but `set_config`
/// wants strict (the caller should be TOLD a value was rejected, not silently snapped
/// to a default). Only `SetConfigArgs` (in `ds_tools`) uses this impl; `VoiceConfig`'s fields pin
/// `deserialize_with = "de_*"`, so the file path is unchanged.
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
strict_de!(Provider, "cpu|cuda|coreml|ane");
strict_de!(TrayKind, "stt|tts|stt_animated|tts_animated");
strict_de!(CancelSpeechScope, "current|other");
strict_de!(DiarizerProvider, "apple_native");
strict_de!(NarrateKind, "digests|shorts");

/// The default narration set: `shorts` first, then `digests` — both on, so a brief
/// blockquote-less reply is heard whole AND longer replies get their spoken-line digest.
/// An EMPTY `narrate` array is the way to opt OUT.
pub(crate) fn default_narrate() -> Vec<NarrateKind> {
    vec![NarrateKind::Shorts, NarrateKind::Digests]
}

/// Fail-open deserialize for `narrate` — a SET of [`NarrateKind`] tokens (config
/// `narrate = ["shorts", "digests"]`). An ARRAY keeps its known tokens in order (deduped) and
/// drops unknown ones; an EMPTY array means narrate nothing. Any NON-array value (an absent
/// field is handled by the serde default; a stray `true`/`"all"` bool/string) fails open to
/// [`default_narrate`]. Only the canonical `digests` / `shorts` tokens are accepted.
pub(crate) fn de_narrate<'de, D>(d: D) -> Result<Vec<NarrateKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, NarrateKind, NarrateKind::parse).unwrap_or_else(default_narrate))
}

/// The default tray-indicator set: the mic (`stt`) colors STATICALLY and the voice (`tts`)
/// colors AND breathes (`tts_animated`).
pub(crate) fn default_tray_indicator() -> Vec<TrayKind> {
    vec![TrayKind::Stt, TrayKind::TtsAnimated]
}

/// Fail-open deserialize for `tray_indicator` — now a SET of [`TrayKind`] tokens (config
/// `tray_indicator = ["stt", "tts"]`). An ARRAY keeps its known tokens in order (deduped)
/// and drops unknown ones; an EMPTY array means never color the icon (the old "none"). Any
/// NON-array value (an absent field is handled by the serde default; a legacy `"both"`/
/// `"none"` string) fails open to [`default_tray_indicator`] — there is NO legacy token
/// migration (clean rename, no compat shim).
pub(crate) fn de_tray_indicator<'de, D>(d: D) -> Result<Vec<TrayKind>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    let parsed =
        fail_open_vec!(&v, TrayKind, TrayKind::parse).unwrap_or_else(default_tray_indicator);
    Ok(normalize_tray_indicator(parsed))
}

/// The default compute-provider PRIORITY ladder: ANE (macOS native) → CUDA (Windows GPU) →
/// CPU. Each engine walks it and picks the first rung usable on this platform (see
/// [`crate::VoiceConfig::resolved_stt_provider`] / `resolved_tts_provider`). `OrtCoreMl` is
/// intentionally NOT in the default — it's explicit-only (slower than CPU for Kokoro).
pub fn default_provider() -> Vec<Provider> {
    vec![Provider::Ane, Provider::OrtCuda, Provider::OrtCpu]
}

/// Fail-open deserialize for `provider` — now an ORDERED PRIORITY array of [`Provider`] rungs
/// (config `provider = ["ane", "cuda", "cpu"]`); the first rung usable on this
/// platform wins. Keeps known tokens in order (deduped), drops unknown ones. Unlike the
/// narrate/tray sets, an EMPTY result is NOT "disabled" — there is always a compute backend —
/// so an empty array, an all-unknown array, or any non-array value falls open to
/// [`default_provider`]. NO legacy `auto` migration (clean rename, no compat shim).
pub(crate) fn de_provider<'de, D>(d: D) -> Result<Vec<Provider>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, Provider, Provider::parse)
        .filter(|providers| !providers.is_empty())
        .unwrap_or_else(default_provider))
}

/// The default diarizer ladder: EMPTY = diarization OFF. Diarization is opt-in (it powers the
/// on-demand `diarize`/`enroll` tools + speaker-lock), so the default is "no runtime, disabled".
/// A non-empty ladder both ENABLES it and sets the runtime priority — the on/off flag and the
/// runtime choice are ONE field (there is no separate `diarization_enabled`).
pub(crate) fn default_diarizer_provider() -> Vec<DiarizerProvider> {
    Vec::new()
}

/// Fail-open deserialize for `diarizer_provider` — an ORDERED PRIORITY array of
/// [`DiarizerProvider`] rungs (config `diarizer_provider = ["apple_native"]`); the
/// first rung usable on this platform wins. Keeps known tokens in order (deduped), drops
/// unknown ones. An EMPTY array means diarization is OFF (the default). Any non-array value
/// also reads as OFF. NO legacy `auto` migration (clean rename, no compat shim).
pub(crate) fn de_diarizer_provider<'de, D>(d: D) -> Result<Vec<DiarizerProvider>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(true));
    Ok(fail_open_vec!(&v, DiarizerProvider, DiarizerProvider::parse).unwrap_or_default())
}

// ─────────────────────────────────────────────────────────────────────────────
// Engine selection: an automatic LADDER (`tts_engine_ladder` / `stt_engine_ladder`,
// config-file only) plus a user PREFERENCE (`tts_engine` / `stt_engine`, settable via
// `set_config`).
//
// The LADDER is an ORDERED Vec of engine rungs (the speech-out / speech-in analogue of the
// `provider` and `diarizer_provider` ladders). EMPTY = that role is OFF; otherwise the first
// rung usable on THIS build/platform wins (see `*_usable` above and
// `VoiceConfig::resolved_tts` / `resolved_stt`). There is no `off` TOKEN any more — "off" is
// structural (an empty array), never a string a rung can carry.
//
// The PREFERENCE is `Option<Vec<Engine>>`, tri-state: `None` = unset (defer to the ladder),
// `Some(vec![])` = explicit off, `Some(vec![engine])` = force exactly that engine (no
// automatic substitution — an unusable choice resolves to off, never a different engine). See
// `VoiceConfig::resolved_tts` / `resolved_stt` for how the two combine.
// ─────────────────────────────────────────────────────────────────────────────

// The platform predicates as PURE functions of (os, arch) — `os`/`arch` are
// `std::env::consts::OS`/`ARCH` strings ("macos"/"windows"/"linux", "x86_64"/"aarch64").
// Splitting them out lets the cross-platform engine-selection logic be unit-tested for EVERY
// target on a single host (the `_on` callers above pin the compile target).
fn built_in_usable_on(os: &str, arch: &str) -> bool {
    !(os == "macos" && arch == "x86_64")
}

/// The ONE runtime fact the static `(os, arch)` matrix above can't see: whether the built-in ONNX
/// engines (Kokoro TTS / Parakeet STT) can actually run on **Intel macOS**. Every other platform
/// either ships a pinned onnxruntime dist or has none at all; Intel is unique in needing an
/// out-of-band runtime — a Homebrew keg ([`crate::brew_onnxruntime_dylib`]) or an explicit
/// `ORT_DYLIB_PATH`. Only the runtime `is_tts_usable`/`is_stt_usable` consult this (NOT the pure `_on`
/// matrix), so with the runtime present Intel resolves to `built_in` like every other platform,
/// and without it the ladders fall through to `system` (`say`) / `claude_code` rather than
/// resolving to an engine that can't load.
pub fn intel_mac_builtin_ort_available() -> bool {
    std::env::var_os("ORT_DYLIB_PATH")
        .map(|p| std::path::Path::new(&p).is_file())
        .unwrap_or(false)
        || crate::brew_onnxruntime_dylib().is_some()
}
/// The `system` STT engine (Apple's on-device recognizer) is macOS-any-arch: the shim
/// serves SpeechAnalyzer on macOS 26+ and legacy SFSpeechRecognizer on 14–25, and both
/// build for arm64 AND x86_64. A machine where the recognizer still can't run (locale,
/// permission, missing shim) is caught by the runtime probe, not this static matrix.
fn system_stt_buildable_on(os: &str, arch: &str) -> bool {
    let _ = arch;
    os == "macos"
}
/// Whether the `system` TTS synth (`say` / SAPI) exists on this OS — macOS + Windows; no
/// system synth is wired on Linux (built-in Kokoro covers it there).
fn system_tts_buildable_on(os: &str) -> bool {
    os == "macos" || os == "windows"
}

/// The default TTS engine LADDER: built-in (Kokoro) first, then the system synth — so an
/// arm64 mac / Windows / Linux uses Kokoro while an x86_64 mac (no Kokoro stack) falls through
/// to the system `say` voice. `[]` opts TTS out entirely.
pub(crate) fn default_tts_engine_ladder() -> Vec<TtsEngine> {
    vec![TtsEngine::Kokoro, TtsEngine::System]
}

/// The default STT engine LADDER: system (Apple's on-device recognizer) → built-in
/// (Parakeet) → claude_code (delegate to Claude Code's dictation), in descending
/// preference. `system` is macOS-only-usable, so this only changes the default on
/// macOS (any arch) — it now prefers the OS recognizer over Parakeet there; Windows and
/// Linux still land on `built_in` first since `system` is never usable off macOS.
/// `claude_code` is LAST and always usable, so a machine where neither on-device engine
/// can run still gets working dictation. `[]` opts dictation out (Caps still silences).
pub(crate) fn default_stt_engine_ladder() -> Vec<SttEngine> {
    vec![SttEngine::System, SttEngine::BuiltIn, SttEngine::ClaudeCode]
}

/// Fail-open deserialize for `tts_engine_ladder` — an ORDERED preference ladder. ARRAYS ONLY:
///   • an ARRAY of tokens (`["built_in","system"]`) — known tokens kept in order (deduped); an
///     empty / all-unknown array ⇒ EMPTY (= off);
///   • anything else (a scalar string, or a wrong-typed value) ⇒ the default ladder. A bare
///     scalar string is NO LONGER a one-rung shorthand — `[]` is the only way to disable.
pub(crate) fn de_tts_engine_ladder<'de, D>(d: D) -> Result<Vec<TtsEngine>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(parse_tts_ladder(&v))
}

/// Fail-open deserialize for `stt_engine_ladder` — see [`de_tts_engine_ladder`]; same rules,
/// STT rungs.
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

/// Fail-open TOML deserialize for the `tts_engine` PREFERENCE (config-file path, distinct from
/// the ladder above): a scalar string is the forced single choice, an empty array is explicit
/// off, and anything else (absent key, wrong-typed value, unrecognized string) is `None` (unset
/// — defer to the ladder).
pub(crate) fn de_tts_engine_pref<'de, D>(d: D) -> Result<Option<Vec<TtsEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(match v {
        toml::Value::String(s) => TtsEngine::parse(&s).map(|e| vec![e]),
        toml::Value::Array(items) if items.is_empty() => Some(Vec::new()),
        _ => None,
    })
}

/// Fail-open TOML deserialize for the `stt_engine` PREFERENCE — see [`de_tts_engine_pref`];
/// same rules, STT tokens.
pub(crate) fn de_stt_engine_pref<'de, D>(d: D) -> Result<Option<Vec<SttEngine>>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = toml::Value::deserialize(d).unwrap_or(toml::Value::Boolean(false));
    Ok(match v {
        toml::Value::String(s) => SttEngine::parse(&s).map(|e| vec![e]),
        toml::Value::Array(items) if items.is_empty() => Some(Vec::new()),
        _ => None,
    })
}

/// Serialize the `tts_engine` PREFERENCE back to TOML: an explicit single choice as its
/// scalar token, explicit off as `[]`. `None` is never reached — the field's
/// `skip_serializing_if = "Option::is_none"` filters it before this runs.
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

/// Serialize the `stt_engine` PREFERENCE — see [`se_tts_engine_pref`]; same rules, STT tokens.
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

/// STRICT (JSON) deserialize for `set_config`'s `tts_engine` PREFERENCE: a plain scalar
/// string — one of the real engine tokens (forces exactly that engine) or the literal
/// `"off"` (explicit off; NOT a real `TtsEngine` value, handled directly here since the
/// enum itself carries no `Off` variant). Any other shape (array, wrong type) or an
/// unrecognized token is a hard ERROR (unlike the fail-open config-file path). `None`
/// when the field is absent.
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

/// STRICT (JSON) deserialize for `set_config`'s `stt_engine` PREFERENCE — see
/// [`de_opt_pref_tts_engine`]; same rules, STT tokens.
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
        for rp in [Cuda, Cpu, CoreMl, CoreMlAne, System] {
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
        assert_eq!(CoreMlAne.as_str(), "CoreML-ANE");
        assert_eq!(System.as_str(), "System");
        // Unknown / casing drift → CPU, never a spurious GPU/ANE claim.
        assert_eq!(RealizedProvider::parse("cuda"), Cpu);
        assert_eq!(RealizedProvider::parse("Cuda"), Cpu);
        assert_eq!(RealizedProvider::parse(""), Cpu);
        assert_eq!(RealizedProvider::parse("bogus"), Cpu);
        // Mapping to the config Provider vocabulary (System has no ort rung → CPU).
        assert_eq!(Cuda.to_provider(), Provider::OrtCuda);
        assert_eq!(CoreMl.to_provider(), Provider::OrtCoreMl);
        assert_eq!(CoreMlAne.to_provider(), Provider::Ane);
        assert_eq!(Cpu.to_provider(), Provider::OrtCpu);
        assert_eq!(System.to_provider(), Provider::OrtCpu);
    }

    #[test]
    fn provider_pref_wants_gpu_tokens() {
        assert!(provider_pref_wants_gpu("cuda"));
        assert!(provider_pref_wants_gpu("CUDA"));
        assert!(provider_pref_wants_gpu("auto"));
        assert!(!provider_pref_wants_gpu("cpu"));
        assert!(!provider_pref_wants_gpu("ane"));
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

    #[test]
    fn engine_usability_matches_host_target() {
        // The host build's `*_usable()` must agree with the pure `_on(OS, ARCH)` it delegates
        // to (the cross-platform table below pins every OTHER target on this one host) — EXCEPT the
        // built-in engines on Intel macOS, whose runtime usability tracks the onnxruntime dylib
        // probe (Homebrew keg / ORT_DYLIB_PATH), a runtime fact the static matrix can't see.
        let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
        let intel_mac = os == "macos" && arch == "x86_64";
        for e in TtsEngine::ALL.iter().copied() {
            let want = if intel_mac && matches!(e, TtsEngine::Kokoro) {
                intel_mac_builtin_ort_available()
            } else {
                e.tts_usable_on(os, arch)
            };
            assert_eq!(e.is_tts_usable(), want, "{e:?}");
        }
        for e in SttEngine::ALL.iter().copied() {
            let want = if intel_mac && matches!(e, SttEngine::BuiltIn) {
                intel_mac_builtin_ort_available()
            } else {
                e.stt_usable_on(os, arch)
            };
            assert_eq!(e.is_stt_usable(), want, "{e:?}");
        }
    }

    #[test]
    fn engine_selection_per_platform() {
        // The FULL cross-platform engine-selection truth table — runnable on any host. For each
        // target, walk the DEFAULT ladders with the pure `_on` predicates and assert what
        // resolves. This is the whole point of the ladder: Windows / Linux run the built-in
        // (Parakeet) engines for both TTS and STT; macOS (any arch) prefers the OS recognizer
        // for STT (`system` is macOS-only-usable and now sits first in the STT ladder) while
        // still preferring Kokoro for TTS on Apple Silicon — an x86_64 mac (no ONNX, no arm64
        // Core-ML stack) falls through to `say` for TTS too.
        let resolve_tts = |os: &str, arch: &str| -> Option<TtsEngine> {
            default_tts_engine_ladder()
                .into_iter()
                .find(|e| e.tts_usable_on(os, arch))
        };
        let resolve_stt = |os: &str, arch: &str| -> Option<SttEngine> {
            default_stt_engine_ladder()
                .into_iter()
                .find(|e| e.stt_usable_on(os, arch))
        };
        let cases = [
            (
                "macos",
                "aarch64",
                Some(TtsEngine::Kokoro),
                Some(SttEngine::System),
            ),
            (
                "macos",
                "x86_64",
                Some(TtsEngine::System),
                Some(SttEngine::System),
            ),
            (
                "windows",
                "x86_64",
                Some(TtsEngine::Kokoro),
                Some(SttEngine::BuiltIn),
            ),
            (
                "windows",
                "aarch64",
                Some(TtsEngine::Kokoro),
                Some(SttEngine::BuiltIn),
            ),
            (
                "linux",
                "x86_64",
                Some(TtsEngine::Kokoro),
                Some(SttEngine::BuiltIn),
            ),
            (
                "linux",
                "aarch64",
                Some(TtsEngine::Kokoro),
                Some(SttEngine::BuiltIn),
            ),
        ];
        for (os, arch, want_tts, want_stt) in cases {
            assert_eq!(resolve_tts(os, arch), want_tts, "TTS on {os}/{arch}");
            assert_eq!(resolve_stt(os, arch), want_stt, "STT on {os}/{arch}");
        }
        // Invariant across every target: claude_code is always a usable STT rung (so the
        // default STT ladder never dead-ends).
        for (os, arch) in [
            ("macos", "x86_64"),
            ("windows", "x86_64"),
            ("linux", "aarch64"),
        ] {
            assert!(SttEngine::ClaudeCode.stt_usable_on(os, arch));
        }
        // System STT is macOS any-arch (legacy SFSpeechRecognizer covers Intel/pre-26);
        // System TTS is macOS + Windows only.
        assert!(SttEngine::System.stt_usable_on("macos", "aarch64"));
        assert!(SttEngine::System.stt_usable_on("macos", "x86_64"));
        assert!(!SttEngine::System.stt_usable_on("windows", "x86_64"));
        assert!(!SttEngine::System.stt_usable_on("linux", "aarch64"));
        assert!(!TtsEngine::System.tts_usable_on("linux", "x86_64"));
    }

    #[test]
    fn provider_usability_matches_across_stt_and_tts() {
        // ONE per-provider usability matrix, asserted for BOTH the STT and TTS ladders, so the
        // rules can't silently diverge — that divergence (ANE usable for the STT ladder but
        // meaningless on Intel) is what routed Intel dictation to the dead `ane` rung and its
        // slow fallback. Columns: expected (is_stt_usable, is_tts_usable) at that (os, arch).
        use Provider::*;
        let cases = [
            // ANE — Apple Silicon ONLY, identically for STT and TTS (the shared rule).
            (Ane, "macos", "aarch64", true, true),
            (Ane, "macos", "x86_64", false, false),
            (Ane, "windows", "x86_64", false, false),
            (Ane, "linux", "aarch64", false, false),
            // CPU — the universal rung, every platform, both axes.
            (OrtCpu, "macos", "x86_64", true, true),
            (OrtCpu, "macos", "aarch64", true, true),
            (OrtCpu, "windows", "x86_64", true, true),
            (OrtCpu, "linux", "aarch64", true, true),
            // CUDA — Windows/Linux GPU EP only, both axes; never macOS.
            (OrtCuda, "windows", "x86_64", true, true),
            (OrtCuda, "linux", "x86_64", true, true),
            (OrtCuda, "macos", "aarch64", false, false),
            // CoreML EP — macOS-only AND TTS-only (never STT).
            (OrtCoreMl, "macos", "aarch64", false, true),
            (OrtCoreMl, "macos", "x86_64", false, true),
            (OrtCoreMl, "windows", "x86_64", false, false),
        ];
        for (p, os, arch, want_stt, want_tts) in cases {
            assert_eq!(p.stt_usable_on(os, arch), want_stt, "stt {p:?} {os}/{arch}");
            assert_eq!(p.tts_usable_on(os, arch), want_tts, "tts {p:?} {os}/{arch}");
        }
    }

    #[test]
    fn provider_ladder_falls_back_per_platform() {
        // A cuda-FIRST ladder (`["cuda", "cpu"]`): CUDA leads, with `cpu` as the
        // universal fallback. CUDA is a real rung on Windows AND Linux (x86_64 ONNX-runtime GPU
        // EP); on macOS the resolver must SKIP it and land on the next usable rung — proving an
        // `cuda` first item degrades gracefully instead of dead-ending. This is the
        // cross-platform analogue of the live `model_status` check (which showed `cpu` on macOS).
        let ladder = [Provider::OrtCuda, Provider::OrtCpu];
        // arch is irrelevant for CUDA/CPU; pin one for the walk (Ane arch-gating is tested below).
        let resolve_tts = |os: &str| {
            ladder
                .iter()
                .copied()
                .find(|p| p.tts_usable_on(os, "x86_64"))
        };
        let resolve_stt = |os: &str| {
            ladder
                .iter()
                .copied()
                .find(|p| p.stt_usable_on(os, "x86_64"))
        };
        // Windows + Linux: CUDA is usable → it WINS (no fallback).
        for os in ["windows", "linux"] {
            assert_eq!(resolve_tts(os), Some(Provider::OrtCuda), "tts {os}");
            assert_eq!(resolve_stt(os), Some(Provider::OrtCuda), "stt {os}");
        }
        // macOS: CUDA is NOT usable → fall back to the next rung, `cpu`.
        assert_eq!(resolve_tts("macos"), Some(Provider::OrtCpu));
        assert_eq!(resolve_stt("macos"), Some(Provider::OrtCpu));
        // A lone `["cuda"]` ladder dead-ends on macOS — the RESOLVER (`resolved_*_provider`)
        // is what supplies the `OrtCpu` default there; the raw `find` returns None, confirming
        // CUDA itself is genuinely unusable (not silently treated as usable).
        let cuda_only = [Provider::OrtCuda];
        assert_eq!(
            cuda_only
                .iter()
                .copied()
                .find(|p| p.tts_usable_on("macos", "aarch64")),
            None
        );
        // (The ANE Apple-Silicon rule and full per-provider matrix live in
        // `provider_usability_matches_across_stt_and_tts` — one table, both axes.)
        // Host agreement: the cfg-gated host fns must match the pure `_on` for THIS (os, arch),
        // so the two never drift (the live engine walks the host fns).
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
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
            vec![TtsEngine::System, TtsEngine::Kokoro]
        );
        // An empty array yields EMPTY (= off).
        assert!(parse_tts_ladder(&arr(&[])).is_empty());
        // Unknown tokens drop; an explicit all-unknown array stays EMPTY (NOT the default —
        // the user gave an array, so we honor its emptiness).
        assert_eq!(
            parse_tts_ladder(&arr(&["festival", "built_in"])),
            vec![TtsEngine::Kokoro]
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
