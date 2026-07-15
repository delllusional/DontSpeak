//! ds-engines — the STT engine selection factory.
//!
//! Maps a resolved config STT engine → `Box<dyn Stt>` with a STRICT
//! no-silent-substitution discipline: when the resolved engine can't actually be
//! constructed HERE (`built_in` STT, which this helper-less factory can never host — real
//! Parakeet runs through the warm helper — and likewise the live `system` recognizer), the
//! box degrades to the SAME INERT placeholder the off/`None` case already uses — never to
//! a DIFFERENT, working engine the user didn't choose. `claude_code` STT is the one engine
//! that's genuinely always live here (it just taps a key). `make_stt` ALWAYS succeeds
//! (never panics/errors); it just may hand back an inert box.
//!
//! `make_stt` is generic over the engine's concrete platform `P` and takes an
//! `Rc<P>` (the engine owns the platform for its whole life and hands the
//! factory a shared clone). This avoids forcing `Platform: Send + Sync` /
//! `unsafe impl Sync` for the macOS `!Send` CGEventSource — see ds-stt's note on
//! why `Stt` is non-`Send`.

use std::rc::Rc;

use ds_config::{SttEngine, VoiceConfig};
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::{ClaudeNative, Stt, SystemStt};

// ─────────────────────────────────────────────────────────────────────────────
// Availability probing — real probes by default, mockable in tests.
// ─────────────────────────────────────────────────────────────────────────────

/// The availability questions the factory asks before honoring a non-default
/// engine. The real impl probes the OS / model dir; tests inject a fake to drive
/// the fallback branches WITHOUT a model, audio, or network.
pub trait EngineAvailability {
    /// Is System STT (macOS `SFSpeechRecognizer`, via the warm helper) usable on this
    /// OS? Real on-device probe on macOS; false on Windows/Linux (still deferred there).
    fn system_stt_supported(&self) -> bool;
}

/// The production availability probe.
pub struct RealAvailability;

impl EngineAvailability for RealAvailability {
    fn system_stt_supported(&self) -> bool {
        SystemStt::available()
    }
}

fn warn(msg: &str) {
    eprintln!("dontspeak/engines: {msg}");
}

// ─────────────────────────────────────────────────────────────────────────────
// STT factory
// ─────────────────────────────────────────────────────────────────────────────

/// Build the STT engine from config, degrading to ClaudeNative
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
///
/// Injectable seam: production (`make_stt_with`/`stt_box`) passes the real resolved `Paths`;
/// tests pass a tempdir-rooted one instead so this never touches the real `$HOME`. `None` (no
/// `$HOME`, or the caller deliberately withholds it) yields the default chord — the same
/// fallback `Paths::resolve()` failing used to produce.
fn claude_code_chord(paths: Option<&ds_config::Paths>) -> ds_platform::KeyChord {
    paths
        .map(|p| ds_platform::KeyChord::parse(&ds_config::read_claude_code_voice(p).key))
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
    make_stt_at(cfg, plat, avail, ds_config::Paths::resolve().as_ref())
}

/// `make_stt` with both an injected availability probe AND an injected `Paths` — the
/// full test seam. Production entry points (`make_stt`/`make_stt_with`) wrap this with
/// `ds_config::Paths::resolve().as_ref()`; tests pass a tempdir-rooted `Paths` so the
/// `ClaudeCode` arm's `claude_code_chord` read never touches the real `$HOME`.
pub fn make_stt_at<P>(
    cfg: &VoiceConfig,
    plat: Rc<P>,
    avail: &dyn EngineAvailability,
    paths: Option<&ds_config::Paths>,
) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    // `stt_engine`/`stt_engine_ladder` is the single STT-path selector: claude_code delegates
    // to Claude Code's own voice dictation (we tap its bound key); built_in/system are LOCAL
    // STT. The built-in engine runs THROUGH the warm helper (dontspeakd::build_stt →
    // HelperStt), not in-process — this helper-less factory (the fallback for tests /
    // no-engine hosts) can never host it, so it returns the SAME INERT box the off/`None` case
    // uses — NEVER substituted with claude_code's cloud dictation. Resolve `stt_engine` to the
    // engine that runs on this build, then map it. `None` (off, or no usable rung) = dictation
    // off — the engine routes a Caps tap to voice-silence and never calls `stt.start()`, so
    // the inert box is never used.
    stt_box(cfg.resolved_stt(), plat, avail, paths)
}

/// Map a SINGLE (already-resolved) STT engine to its box — the ladder-free inverse of
/// [`VoiceConfig::resolved_stt`]. `claude_code` is the one engine genuinely live here (it
/// just taps a key); `built_in` (this helper-less factory can never host real Parakeet) and
/// an unavailable `system` degrade to the SAME INERT `SystemStt` the off (`None`) case uses —
/// NO substitution to a different, working engine.
///
/// `paths` is forwarded to `claude_code_chord` for the `ClaudeCode` arm — see its doc for the
/// injectable-seam rationale.
fn stt_box<P>(
    engine: Option<SttEngine>,
    plat: Rc<P>,
    avail: &dyn EngineAvailability,
    paths: Option<&ds_config::Paths>,
) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    match engine {
        // Off — inert.
        None => Box::new(SystemStt::new()),
        Some(SttEngine::ClaudeCode) => Box::new(ClaudeNative::new(plat, claude_code_chord(paths))),
        Some(SttEngine::BuiltIn) => {
            // This helper-less factory can never host real Parakeet (it runs through the warm
            // helper — dontspeakd::build_stt uses HelperStt whenever that's actually available,
            // and only reaches this factory when it isn't). NO substitution: dictation is off
            // here, never silently rerouted to Claude Code's cloud transcription.
            warn(
                "built_in STT unavailable here (no warm helper / model not ready); \
                 dictation is off here (not substituted)",
            );
            let _ = plat;
            Box::new(SystemStt::new())
        }
        Some(SttEngine::System) => {
            // System STT (Apple's on-device recognizer) runs THROUGH the warm helper
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
        fn is_terminal_frontmost(&self) -> bool {
            self._frontmost.get()
        }
    }

    // Injectable availability fake driving the fallback branches.
    struct FakeAvail {
        system_stt: bool,
    }
    impl EngineAvailability for FakeAvail {
        fn system_stt_supported(&self) -> bool {
            self.system_stt
        }
    }

    fn plat() -> Rc<FakePlat> {
        Rc::new(FakePlat::default())
    }

    // ── STT engine → box MAPPING (pure, ladder-free) ────────────────────────

    #[test]
    fn stt_box_maps_each_engine() {
        // claude_code maps to the LIVE Ctrl+G ClaudeNative path (its actual implementation).
        // built_in — unavailable in this helper-less factory (real Parakeet runs in
        // dontspeakd's warm helper) — maps to the INERT SystemStt box, NEVER substituted to
        // claude_code's cloud dictation. system (available or not) already followed this
        // no-substitution rule. None (off) is inert.
        //
        // The ClaudeCode arm builds `claude_code_chord(paths)`, which reads Claude Code's
        // keybindings.json — a tempdir-rooted `Paths` keeps that read off the real `$HOME`
        // (see `claude_code_chord`).
        let avail = FakeAvail { system_stt: true };
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        for (engine, want) in [
            (Some(SttEngine::ClaudeCode), "claude_code"),
            (Some(SttEngine::BuiltIn), "system"), // inert SystemStt, NOT claude_code
            (Some(SttEngine::System), "system"),
            (None, "system"), // inert SystemStt
        ] {
            assert_eq!(
                stt_box(engine, plat(), &avail, Some(&paths)).kind(),
                want,
                "stt_engine {engine:?} must map to the expected box",
            );
        }
    }

    // ── make_stt_at: the full injectable-Paths seam, exercised directly ──────

    #[test]
    fn make_stt_at_is_hermetic_with_injected_paths() {
        // Proves the PUBLIC entry point itself (not just the private stt_box) never
        // touches the real $HOME: a tempdir-rooted Paths is injected all the way through
        // make_stt_at, and the ClaudeCode arm's keybindings.json read stays hermetic.
        let cfg = VoiceConfig {
            stt_engine: Some(vec![SttEngine::ClaudeCode]),
            ..VoiceConfig::default()
        };
        let avail = FakeAvail { system_stt: true };
        let dir = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(dir.path());
        assert_eq!(
            make_stt_at(&cfg, plat(), &avail, Some(&paths)).kind(),
            "claude_code"
        );
    }

    // ── Default-ladder RESOLUTION through make_* (arch-dependent) ────────────

    #[test]
    fn default_ladder_resolves_and_builds() {
        // The default ladder always RESOLVES to a usable engine. STT's default ladder is
        // `[system, built_in, claude_code]`, and `system` is statically buildable on ANY
        // macOS arch (see `system_stt_buildable_on`), so it wins first there — the
        // built_in/claude_code rungs are only reachable off macOS.
        //
        // Whether the BOX this factory builds is LIVE, though, now depends on which engine
        // resolved: `system`/`claude_code` build a live box here; `built_in` (reachable off
        // macOS) maps to the INERT box in this helper-less factory (§12 no-substitution fix) —
        // real Parakeet only runs through dontspeakd's warm helper, which this factory isn't.
        let cfg = VoiceConfig::default();
        let avail = FakeAvail { system_stt: true };
        let want_stt = match cfg.resolved_stt() {
            Some(SttEngine::ClaudeCode) => "claude_code",
            // System, or BuiltIn (inert here) — both map to the SystemStt box.
            _ => "system",
        };
        assert_eq!(make_stt_with(&cfg, plat(), &avail).kind(), want_stt);
    }
}
