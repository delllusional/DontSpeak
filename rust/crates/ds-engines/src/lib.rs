//! STT engine factory: resolved config → `Box<dyn Stt>`.
//!
//! No silent substitution: if `built_in` / live `system` can't be constructed here
//! (Parakeet + system recognizer run on the warm helper), return the same inert
//! placeholder as off/`None` — not another working engine. `claude_code` is always
//! live here (key tap). `make_stt` always succeeds (may be inert).
//!
//! Generic over `P` via `Rc<P>` so macOS `!Send` CGEventSource need not be Sync.

use std::rc::Rc;

use ds_config::{SttEngine, VoiceConfig};
use ds_platform::{FrontmostWindow, KeyInjector};
use ds_stt::{ClaudeNative, Stt, SystemStt};

/// Probe before honoring a non-default engine. Tests inject fakes.
pub trait EngineAvailability {
    /// System STT usable? Real probe on macOS; false on Windows/Linux.
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
    log::warn!(target: "engines", "{msg}");
}

/// Build STT from config; may return inert (see crate doc).
pub fn make_stt<P>(cfg: &VoiceConfig, plat: Rc<P>) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    make_stt_with(cfg, plat, &RealAvailability)
}

/// Claude Code `voice:pushToTalk` chord from keybindings.json (read-only).
/// Injectable Paths (tempdir in tests). `None` ⇒ default chord.
fn claude_code_chord(paths: Option<&ds_config::Paths>) -> ds_platform::KeyChord {
    paths
        .map(|p| ds_platform::KeyChord::parse(&ds_config::read_claude_code_voice(p).key))
        .unwrap_or_default()
}

/// [`make_stt`] with injected availability (tests).
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

/// Full seam: probe + Paths. Production: `Paths::resolve()`; tests: tempdir.
pub fn make_stt_at<P>(
    cfg: &VoiceConfig,
    plat: Rc<P>,
    avail: &dyn EngineAvailability,
    paths: Option<&ds_config::Paths>,
) -> Box<dyn Stt>
where
    P: KeyInjector + FrontmostWindow + 'static,
{
    // `None` = dictation off (engine skips `stt.start()` on Caps).
    stt_box(cfg.resolved_stt(), plat, avail, paths)
}

/// Ladder-free inverse of [`VoiceConfig::resolved_stt`]. Substitution: crate doc.
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
            // Real Parakeet is warm-helper only — inert placeholder here.
            warn(
                "built_in STT unavailable here (no warm helper / model not ready); \
                 dictation is off here (not substituted)",
            );
            let _ = plat;
            Box::new(SystemStt::new())
        }
        Some(SttEngine::System) => {
            // Live recognizer is warm-helper only — inert here (not claude_native).
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
        // claude_code live; built_in/system/None → inert SystemStt. Tempdir Paths.
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
        let cfg = VoiceConfig::default();
        let avail = FakeAvail { system_stt: true };
        let want_stt = match cfg.resolved_stt() {
            Some(SttEngine::ClaudeCode) => "claude_code",
            // System or BuiltIn (inert) → SystemStt.
            _ => "system",
        };
        assert_eq!(make_stt_with(&cfg, plat(), &avail).kind(), want_stt);
    }
}
