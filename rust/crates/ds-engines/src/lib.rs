//! STT engine selection factory.
//!
//! Maps a resolved config STT engine → `Box<dyn Stt>` with a STRICT
//! no-silent-substitution discipline: when the resolved engine can't be
//! constructed HERE (`built_in` STT — real Parakeet runs through the warm
//! helper — and the live `system` recognizer), the box degrades to the SAME
//! INERT placeholder the off/`None` case uses — never a DIFFERENT working
//! engine the user didn't choose. `claude_code` is the one engine always live
//! here (it taps a key). `make_stt` ALWAYS succeeds; it may hand back an inert box.
//!
//! Generic over platform `P` with an `Rc<P>` (the engine owns the platform for
//! its life). Avoids forcing `Platform: Send + Sync` / `unsafe impl Sync` for
//! the macOS `!Send` CGEventSource — see ds-stt on why `Stt` is non-`Send`.

use std::rc::Rc;

use ds_config::{SttEngine, VoiceConfig};
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::{ClaudeNative, Stt, SystemStt};

/// Availability questions before honoring a non-default engine. Production probes
/// the OS / model dir; tests inject a fake to drive fallbacks without hardware.
pub trait EngineAvailability {
    /// System STT (macOS `SFSpeechRecognizer` via the warm helper) usable here?
    /// Real probe on macOS; false on Windows/Linux.
    fn system_stt_supported(&self) -> bool;
}

/// Production availability probe.
pub struct RealAvailability;

impl EngineAvailability for RealAvailability {
    fn system_stt_supported(&self) -> bool {
        SystemStt::available()
    }
}

fn warn(msg: &str) {
    eprintln!("dontspeak/engines: {msg}");
}

/// Build STT from config. Never panics; may return an inert box (see crate doc).
/// Generic over platform `P`; borrows it through the shared `Rc`.
pub fn make_stt<P>(cfg: &VoiceConfig, plat: Rc<P>) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    make_stt_with(cfg, plat, &RealAvailability)
}

/// Claude Code `voice:pushToTalk` key (default `Space`), read from keybindings.json
/// — never written. Injectable: production passes real `Paths`; tests pass a
/// tempdir-rooted one so this never touches `$HOME`. `None` ⇒ default chord.
fn claude_code_chord(paths: Option<&ds_config::Paths>) -> ds_platform::KeyChord {
    paths
        .map(|p| ds_platform::KeyChord::parse(&ds_config::read_claude_code_voice(p).key))
        .unwrap_or_default()
}

/// [`make_stt`] with an injected availability probe (tests).
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

/// Full test seam: injected probe + `Paths`. Production wraps with
/// `Paths::resolve()`; tests pass a tempdir so `claude_code_chord` stays hermetic.
pub fn make_stt_at<P>(
    cfg: &VoiceConfig,
    plat: Rc<P>,
    avail: &dyn EngineAvailability,
    paths: Option<&ds_config::Paths>,
) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    // Resolve then map; see crate doc for no-silent-substitution. `None` = dictation
    // off (engine never calls `stt.start()` on Caps).
    stt_box(cfg.resolved_stt(), plat, avail, paths)
}

/// Map one already-resolved STT engine to its box (ladder-free inverse of
/// [`VoiceConfig::resolved_stt`]). See crate doc for substitution rules;
/// `paths` is for the `ClaudeCode` arm — see [`claude_code_chord`].
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
        None => Box::new(SystemStt::new()),
        Some(SttEngine::ClaudeCode) => Box::new(ClaudeNative::new(plat, claude_code_chord(paths))),
        Some(SttEngine::BuiltIn) => {
            // Helper-less factory can't host real Parakeet — inert, never substituted.
            warn(
                "built_in STT unavailable here (no warm helper / model not ready); \
                 dictation is off here (not substituted)",
            );
            let _ = plat;
            Box::new(SystemStt::new())
        }
        Some(SttEngine::System) => {
            // Live recognizer runs through the warm helper; inert here (never → claude_native).
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

    /// Minimal platform for `make_stt` bounds; no real OS calls.
    #[derive(Default)]
    struct FakePlat {
        _frontmost: Cell<bool>,
    }
    impl KeyInjector for FakePlat {}
    impl FrontmostWindow for FakePlat {
        fn is_terminal_frontmost(&self) -> bool {
            self._frontmost.get()
        }
    }

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

    #[test]
    fn stt_box_maps_each_engine() {
        // No-silent-substitution: claude_code live; built_in/system/None → inert SystemStt.
        // Tempdir Paths keeps ClaudeCode's keybindings.json read off real `$HOME`.
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

    #[test]
    fn make_stt_at_is_hermetic_with_injected_paths() {
        // Public entry (not just private stt_box) stays hermetic via injected Paths.
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

    #[test]
    fn default_ladder_resolves_and_builds() {
        // Default ladder always resolves. macOS: system first (statically buildable).
        // Box liveness: system/claude_code live here; built_in (off macOS) is inert
        // (real Parakeet only via dontspeakd's warm helper).
        let cfg = VoiceConfig::default();
        let avail = FakeAvail { system_stt: true };
        let want_stt = match cfg.resolved_stt() {
            Some(SttEngine::ClaudeCode) => "claude_code",
            // System, or BuiltIn (inert here) — both map to SystemStt.
            _ => "system",
        };
        assert_eq!(make_stt_with(&cfg, plat(), &avail).kind(), want_stt);
    }
}
