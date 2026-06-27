//! ds-engines — the engine selection factory (ARCHITECTURE §A.3).
//!
//! The ONE place that maps a config enum → `Box<dyn Trait>` with the
//! degrade-to-default-never-silent discipline: an unavailable engine (System TTS
//! on an unsupported OS, Parakeet with no model) logs a warning and falls back to
//! the Phase-1 default (Kokoro TTS / ClaudeNative STT). System STT is the deliberate
//! exception to fall-BACK: it runs through the warm helper, so this helper-less
//! factory returns the INERT `SystemStt` (never claude_native) — selecting `system`
//! must not silently become Claude-native dictation. `make_*` ALWAYS succeeds.
//!
//! `make_stt` is generic over the engine's concrete platform `P` and takes an
//! `Rc<P>` (the engine owns the platform for its whole life and hands the
//! factory a shared clone). This avoids forcing `Platform: Send + Sync` /
//! `unsafe impl Sync` for the macOS `!Send` CGEventSource — see ds-stt's note on
//! why `Stt` is non-`Send`.

use std::rc::Rc;

use ds_config::{DiarizerProvider, SttEngine, TtsEngine, VoiceConfig};
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::diarize::Diarizer;
use ds_stt::{ClaudeNative, Stt, SystemStt};
use ds_tts::{KokoroTts, SystemTts, Tts};

// ─────────────────────────────────────────────────────────────────────────────
// Availability probing — real probes by default, mockable in tests.
// ─────────────────────────────────────────────────────────────────────────────

/// The availability questions the factory asks before honoring a non-default
/// engine. The real impl probes the OS / model dir; tests inject a fake to drive
/// the fallback branches WITHOUT a model, audio, or network.
pub trait EngineAvailability {
    /// Is a System TTS backend usable on this OS? Drives System → Kokoro.
    fn system_tts_supported(&self) -> bool;
    /// Is System STT wired on this OS? Always false in Phase 2 (deferred).
    fn system_stt_supported(&self) -> bool;
}

/// The production availability probe.
pub struct RealAvailability;

impl EngineAvailability for RealAvailability {
    fn system_tts_supported(&self) -> bool {
        SystemTts::available()
    }
    fn system_stt_supported(&self) -> bool {
        SystemStt::available()
    }
}

fn warn(msg: &str) {
    eprintln!("dontspeak/engines: {msg}");
}

// ─────────────────────────────────────────────────────────────────────────────
// TTS factory
// ─────────────────────────────────────────────────────────────────────────────

/// Build the TTS engine from config, falling back to Kokoro when System is
/// chosen but unavailable on this OS.
pub fn make_tts(cfg: &VoiceConfig) -> Box<dyn Tts> {
    make_tts_with(cfg, &RealAvailability)
}

/// `make_tts` with an injected availability probe (for tests).
pub fn make_tts_with(cfg: &VoiceConfig, avail: &dyn EngineAvailability) -> Box<dyn Tts> {
    let paths = match ds_config::Paths::resolve() {
        Some(p) => p,
        None => {
            // Without $HOME the pidfile path can't resolve. Kokoro is still the
            // right default; dummy_paths() gives a temp-rooted Paths and the box
            // fail-quiets at speak time (no pidfile written).
            warn("cannot resolve paths; TTS will be inert");
            return Box::new(KokoroTts::new(dummy_paths()));
        }
    };

    // Resolve the `tts_engine` preference ladder to the engine that runs on this build, then
    // map it. `None` (empty ladder / no usable rung) = spoken replies off — every speak path
    // gates on `cfg.tts_on()` first, so the inert Kokoro box is never asked to speak.
    tts_box(cfg.resolved_tts().unwrap_or(TtsEngine::Off), avail, paths)
}

/// Map a SINGLE (already-resolved) TTS engine to its box — the ladder-free inverse of
/// [`VoiceConfig::resolved_tts`]. `Off` → the inert Kokoro box (never asked to speak).
fn tts_box(
    engine: TtsEngine,
    avail: &dyn EngineAvailability,
    paths: ds_config::Paths,
) -> Box<dyn Tts> {
    match engine {
        TtsEngine::System => {
            if avail.system_tts_supported() {
                Box::new(SystemTts::new(paths))
            } else {
                warn("system TTS unavailable on this OS; falling back to kokoro");
                Box::new(KokoroTts::new(paths))
            }
        }
        // Kokoro, or off → the Kokoro box (inert when off).
        TtsEngine::Kokoro | TtsEngine::Off => Box::new(KokoroTts::new(paths)),
    }
}

/// A placeholder Paths when $HOME can't be resolved (the KokoroTts box then
/// fail-quiets at speak time — the ds-helper helper can't write the pidfile
/// without a real ~/.claude, so no speaker is ever recorded). Builds the fallback
/// via the env-free [`ds_config::Paths::rooted_at`] so it NEVER mutates the process
/// environment (an unsound `set_var("HOME", …)` once engine threads are running);
/// the box is inert, so the temp-rooted layout is never actually written.
fn dummy_paths() -> ds_config::Paths {
    ds_config::Paths::resolve()
        .unwrap_or_else(|| ds_config::Paths::rooted_at(&std::env::temp_dir()))
}

// ─────────────────────────────────────────────────────────────────────────────
// STT factory
// ─────────────────────────────────────────────────────────────────────────────

/// Build the STT engine from config, degrading to the Phase-1 ClaudeNative
/// default whenever the selected engine is unavailable.
///
/// Generic over the engine's platform `P`; takes the shared `Rc<P>` the engine
/// owns. The returned `Box<dyn Stt>` borrows the platform through that `Rc`.
pub fn make_stt<P>(cfg: &VoiceConfig, plat: Rc<P>) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    make_stt_with(cfg, plat, &RealAvailability)
}

/// The key Claude Code's `voice:pushToTalk` is bound to, READ from Claude Code's config
/// (default `Space`), parsed into a [`ds_platform::KeyChord`] for `ClaudeNative` to tap.
/// Read-don't-write: we only read Claude Code's keybindings.json, never modify it.
fn claude_code_chord() -> ds_platform::KeyChord {
    ds_config::Paths::resolve()
        .map(|p| {
            ds_platform::KeyChord::parse(&ds_config::read_claude_code_voice(&p).key)
        })
        .unwrap_or_default()
}

/// `make_stt` with an injected availability probe (for tests).
pub fn make_stt_with<P>(
    cfg: &VoiceConfig,
    plat: Rc<P>,
    avail: &dyn EngineAvailability,
) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    // `stt_engine` is the single STT-path selector: claude_code delegates to Claude Code's
    // own voice dictation (we tap its bound key); built_in/system are LOCAL STT. The
    // built-in engine runs THROUGH the warm helper (dontspeakd::build_stt → HelperStt), not
    // in-process — so this factory (the helper-less fallback for tests / no-engine hosts)
    // degrades built_in to the Claude Code dictation path here. ClaudeNative taps the key
    // Claude Code is configured with (read from its keybindings.json; default Space).
    // Resolve the `stt_engine` preference ladder to the engine that runs on this build, then
    // map it. `None` (empty ladder / no usable rung) = dictation off — the engine routes a Caps
    // tap to voice-silence and never calls `stt.start()`, so the inert box is never used.
    stt_box(cfg.resolved_stt().unwrap_or(SttEngine::Off), plat, avail)
}

/// Map a SINGLE (already-resolved) STT engine to its box — the ladder-free inverse of
/// [`VoiceConfig::resolved_stt`]. `built_in` degrades to the Ctrl+G (ClaudeNative) path in this
/// helper-less factory (real Parakeet runs in dontspeakd's warm helper); `system` returns the
/// INERT `SystemStt` (never claude_native); `Off` → inert.
fn stt_box<P>(engine: SttEngine, plat: Rc<P>, avail: &dyn EngineAvailability) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    match engine {
        SttEngine::ClaudeCode | SttEngine::BuiltIn => {
            Box::new(ClaudeNative::new(plat, claude_code_chord()))
        }
        SttEngine::System => {
            // System STT (macOS SpeechAnalyzer) runs THROUGH the warm helper
            // (dontspeakd::build_stt → HelperStt). This helper-less factory can't host the live
            // recognizer, so it returns the INERT SystemStt rather than degrading to
            // claude_native — selecting `system` must NEVER silently become Claude-native
            // dictation (the engine surfaces "unavailable" instead).
            if !avail.system_stt_supported() {
                warn("system STT unavailable (on-device recognizer not ready)");
            }
            let _ = plat;
            Box::new(SystemStt::new())
        }
        SttEngine::Off => Box::new(SystemStt::new()),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Diarization factory (on-demand — driven by the `diarize` MCP tool, not the
// Caps loop). Unlike make_tts/make_stt this can return None: diarization is an
// optional capability, and the tool surfaces "unavailable" rather than degrading
// to some other behavior.
// ─────────────────────────────────────────────────────────────────────────────

/// Build the diarizer the `diarize` tool should use, or `None` when no backend is
/// available for the resolved provider on this platform. Core ML / ANE on macOS;
/// off macOS `AppleNative` (the only rung) has no backend and resolves to `None`.
///
/// NOTE: the diarize path runs inside the warm `ds-helper` (in `ds-tts`), which
/// cannot depend on `ds-engines` without a cycle — so the helper resolves
/// `diarizer_provider` inline (`ensure_coreml_diarizer`) and constructs the backend
/// directly. This factory is the engine-side seam for provider routing; it's exercised by
/// the tests below to keep the `provider → backend` mapping honest. Keep the two resolution
/// sites in sync.
pub fn make_diarizer(provider: DiarizerProvider) -> Option<Box<dyn Diarizer>> {
    // `provider` is an ALREADY-RESOLVED rung (the caller walks the ladder via
    // `VoiceConfig::resolved_diarizer`); this just maps rung → backend.
    match provider {
        #[cfg(target_os = "macos")]
        DiarizerProvider::AppleNative => {
            Some(Box::new(ds_stt::diarize::CoremlDiarizer::new()))
        }
        // AppleNative is macOS-only; the resolver never hands it here off macOS.
        #[allow(unreachable_patterns)]
        other => {
            warn(&format!(
                "no diarizer backend for {other:?} on this platform"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // A minimal platform satisfying the make_stt bounds (KeyInjector +
    // FrontmostWindow). No real OS calls.
    #[derive(Default)]
    struct FakePlat {
        _frontmost: Cell<bool>,
    }
    // Inherits the default no-op `tap_key`/`type_text`/`press_enter` (no real OS calls).
    impl KeyInjector for FakePlat {}
    impl FrontmostWindow for FakePlat {
        fn terminal_frontmost(&self) -> bool {
            self._frontmost.get()
        }
    }

    // Injectable availability fake driving the fallback branches.
    struct FakeAvail {
        system_tts: bool,
        system_stt: bool,
    }
    impl EngineAvailability for FakeAvail {
        fn system_tts_supported(&self) -> bool {
            self.system_tts
        }
        fn system_stt_supported(&self) -> bool {
            self.system_stt
        }
    }

    fn cfg_with(stt: SttEngine, tts: TtsEngine) -> VoiceConfig {
        VoiceConfig {
            stt_engine: vec![stt],
            tts_engine: vec![tts],
            ..Default::default()
        }
    }

    fn plat() -> Rc<FakePlat> {
        Rc::new(FakePlat::default())
    }

    fn test_paths() -> ds_config::Paths {
        ds_config::Paths::resolve()
            .unwrap_or_else(|| ds_config::Paths::rooted_at(&std::env::temp_dir()))
    }

    // ── TTS engine → box MAPPING (pure, ladder-free: arch-independent) ──────

    #[test]
    fn tts_box_maps_each_engine() {
        let avail = FakeAvail {
            system_tts: true,
            system_stt: false,
        };
        // Kokoro / Off → the Kokoro box; System (supported) → the system `say` box.
        assert_eq!(tts_box(TtsEngine::Kokoro, &avail, test_paths()).kind(), "kokoro");
        assert_eq!(tts_box(TtsEngine::Off, &avail, test_paths()).kind(), "kokoro");
        assert_eq!(tts_box(TtsEngine::System, &avail, test_paths()).kind(), "system");
    }

    #[test]
    fn tts_box_system_unavailable_falls_back_to_kokoro() {
        let avail = FakeAvail {
            system_tts: false, // unsupported OS
            system_stt: false,
        };
        assert_eq!(tts_box(TtsEngine::System, &avail, test_paths()).kind(), "kokoro");
    }

    // ── STT engine → box MAPPING (pure, ladder-free) ────────────────────────

    #[test]
    fn stt_box_maps_each_engine() {
        // claude_code AND built_in map to the Ctrl+G ClaudeNative path (real Parakeet runs in
        // dontspeakd's warm helper, not this helper-less factory); system → the INERT SystemStt
        // (kind "system"), NEVER claude_native; off → inert.
        let avail = FakeAvail {
            system_tts: true,
            system_stt: true,
        };
        for (engine, want) in [
            (SttEngine::ClaudeCode, "claude_code"),
            (SttEngine::BuiltIn, "claude_code"),
            (SttEngine::System, "system"),
            (SttEngine::Off, "system"), // inert SystemStt
        ] {
            assert_eq!(
                stt_box(engine, plat(), &avail).kind(),
                want,
                "stt_engine {engine:?} must map to a live box",
            );
        }
    }

    // ── Default-ladder RESOLUTION through make_* (arch-dependent) ────────────

    #[test]
    fn default_ladder_resolves_and_builds() {
        // The default ladders always build a live engine. STT resolves to built_in (arm64
        // mac / win / linux) or claude_code (x86_64 mac) — BOTH map to the "claude_code" box.
        // TTS resolves to kokoro where the built-in stack is usable, else the system `say`.
        let cfg = VoiceConfig::default();
        let avail = FakeAvail {
            system_tts: true,
            system_stt: true,
        };
        assert_eq!(make_stt_with(&cfg, plat(), &avail).kind(), "claude_code");
        let want_tts = if cfg.resolved_tts() == Some(TtsEngine::System) {
            "system"
        } else {
            "kokoro"
        };
        assert_eq!(make_tts_with(&cfg, &avail).kind(), want_tts);
    }

    // ── Diarization factory ─────────────────────────────────────────────────

    #[test]
    #[cfg(target_os = "macos")]
    fn apple_native_diarizer_is_coreml_on_macos() {
        // The resolved AppleNative rung maps to the Core ML / ANE backend on macOS (lazy —
        // constructing it loads no models, so this is safe without the dylib/models present).
        assert!(make_diarizer(DiarizerProvider::AppleNative).is_some());
    }
}
