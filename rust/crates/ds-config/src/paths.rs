//! Well-known paths from `$HOME`, plus per-OS data/model dirs.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// Well-known paths, resolved once from $HOME.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    /// Speaker pidfile — TTS process-group id (single-speaker barge-in).
    pub pidfile: PathBuf,
    /// Unified activity log (per-OS logs dir); shared via `ds_log`.
    pub log_file: PathBuf,
    pub settings_json: PathBuf,
    /// Claude Code keybindings — READ-ONLY for `claude_code` STT (`voice:pushToTalk`).
    pub keybindings_json: PathBuf,
    /// `~/.claude.json` — MCP half of `wire claude_code` (hooks are in `settings_json`).
    pub claude_code_config: PathBuf,
    /// Running `ds-narrate` pid.
    pub narrate_pid: PathBuf,
    /// Engine pid (reload/liveness). Not the speaker `pidfile` or `narrate_pid`.
    pub engine_pid: PathBuf,
    /// Engine IPC socket (`ds-ipc`); absence ⇒ engine down.
    pub engine_sock: PathBuf,
    /// Lifetime usage totals (`tts_secs`/`stt_secs`).
    pub stats_toml: PathBuf,
    /// Roaming settings root (`config.toml`, `speakers.json`) — see [`data_dir`].
    pub config_dir: PathBuf,
    /// Local state root (pidfiles, socket, stats). Models under [`model_dir`].
    pub state_dir: PathBuf,
    /// Local re-creatable cache root (usage snapshots; models use [`model_dir`]).
    pub cache_dir: PathBuf,
    /// Our settings single source (`config.toml`); engine watches it.
    pub config_toml: PathBuf,
    /// Enrolled voiceprints — see [`crate::speakers`].
    pub speakers_json: PathBuf,
    /// `~/.codex` — presence-gate for `wire codex`.
    pub codex_dir: PathBuf,
    /// Codex hooks file.
    pub codex_config: PathBuf,
    /// `~/.qwen` — presence-gate for `wire qwen_code`.
    pub qwen_dir: PathBuf,
    /// Qwen hooks + MCP (one file).
    pub qwen_settings: PathBuf,
    /// `~/.grok` — presence-gate for `wire grok`.
    pub grok_dir: PathBuf,
    /// Grok MCP entry.
    pub grok_config: PathBuf,
    /// Grok dedicated hooks file (unwire deletes).
    pub grok_hooks_json: PathBuf,
    /// Grok global rules; managed narrate section (hook stdout ignored — issue #95).
    pub grok_agents_md: PathBuf,
}

impl Paths {
    /// Resolve from $HOME. Fails only if unset.
    pub fn resolve() -> Option<Self> {
        let base = BaseDirs::new()?;
        let home = base.home_dir().to_path_buf();
        let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
        let claude_override = std::env::var_os("CLAUDE_CONFIG_DIR");
        let claude_dir = client_config_dir(&home, &cwd, claude_override.as_deref(), ".claude");
        let codex_dir = client_config_dir(
            &home,
            &cwd,
            std::env::var_os("CODEX_HOME").as_deref(),
            ".codex",
        );
        let qwen_dir = client_config_dir(
            &home,
            &cwd,
            std::env::var_os("QWEN_HOME").as_deref(),
            ".qwen",
        );
        let grok_dir = client_config_dir(
            &home,
            &cwd,
            std::env::var_os("GROK_HOME").as_deref(),
            ".grok",
        );
        let claude_code_config = claude_override
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|_| claude_dir.join(".claude.json"))
            .unwrap_or_else(|| home.join(".claude.json"));
        // Two roots, each idiomatic per OS (see [`data_dir`] / [`model_dir`] / `state_root`):
        //   config (roaming, user settings): config.toml, speakers.json
        //   state  (local, machine RUNTIME): stats.toml, pidfiles, the IPC socket, logs
        // On Windows/Linux these resolve to distinct OS dirs (roaming vs local / config vs
        // state); on macOS both are Application Support. The engine create_dir_all's both on
        // startup, and every writer create_dir_all's its own parent too.
        let config_dir = data_dir()?;
        let state_dir = state_root(&base);
        let cache_dir = cache_root(&base);
        Some(Self {
            // Runtime/state files live under the LOCAL state root (machine-specific, never
            // roamed); settings live under the roaming config root.
            pidfile: state_dir.join("speak-hook.pid"),
            log_file: log_path(&base, &state_dir),
            settings_json: claude_dir.join("settings.json"),
            keybindings_json: claude_dir.join("keybindings.json"),
            claude_code_config,
            narrate_pid: state_dir.join("narrate.pid"),
            engine_pid: state_dir.join("dontspeakd.pid"),
            engine_sock: state_dir.join("dontspeak.sock"),
            stats_toml: state_dir.join("stats.toml"),
            config_toml: config_dir.join("config.toml"),
            speakers_json: config_dir.join("speakers.json"),
            config_dir,
            state_dir,
            cache_dir,
            codex_config: codex_dir.join("config.toml"),
            codex_dir,
            qwen_settings: qwen_dir.join("settings.json"),
            qwen_dir,
            grok_config: grok_dir.join("config.toml"),
            grok_hooks_json: grok_dir.join("hooks").join("dontspeak.json"),
            grok_agents_md: grok_dir.join("AGENTS.md"),
            grok_dir,
            home,
            claude_dir,
        })
    }

    /// Env-free Paths under `home` when [`resolve`](Paths::resolve) is None (inert engine).
    /// Must not `set_var` (unsound with other threads). Layout immaterial — not a real session.
    pub fn rooted_at(home: &Path) -> Self {
        let home = home.to_path_buf();
        let claude_dir = home.join(".claude");
        let codex_dir = home.join(".codex");
        let qwen_dir = home.join(".qwen");
        let grok_dir = home.join(".grok");
        let ds_dir = home.join(".dontspeak");
        Self {
            pidfile: ds_dir.join("speak-hook.pid"),
            log_file: home.join("dontspeak.log"),
            settings_json: claude_dir.join("settings.json"),
            keybindings_json: claude_dir.join("keybindings.json"),
            claude_code_config: home.join(".claude.json"),
            narrate_pid: ds_dir.join("narrate.pid"),
            engine_pid: ds_dir.join("dontspeakd.pid"),
            engine_sock: ds_dir.join("dontspeak.sock"),
            stats_toml: ds_dir.join("stats.toml"),
            config_toml: ds_dir.join("config.toml"),
            speakers_json: ds_dir.join("speakers.json"),
            // The inert fallback uses ONE dir for both roots (layout is immaterial here).
            config_dir: ds_dir.clone(),
            state_dir: ds_dir.clone(),
            cache_dir: ds_dir,
            codex_config: codex_dir.join("config.toml"),
            codex_dir,
            qwen_settings: qwen_dir.join("settings.json"),
            qwen_dir,
            grok_config: grok_dir.join("config.toml"),
            grok_hooks_json: grok_dir.join("hooks").join("dontspeak.json"),
            grok_agents_md: grok_dir.join("AGENTS.md"),
            grok_dir,
            home,
            claude_dir,
        }
    }
}

/// Resolve a client-specific home without mutating process environment in tests.
/// Relative overrides follow the client's launch cwd; `~` remains the user home.
fn client_config_dir(
    home: &Path,
    cwd: &Path,
    override_value: Option<&OsStr>,
    default_name: &str,
) -> PathBuf {
    let Some(value) = override_value.filter(|value| !value.is_empty()) else {
        return home.join(default_name);
    };
    let path = Path::new(value);
    if let Ok(rest) = path.strip_prefix("~") {
        return home.join(rest);
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Our brand subfolder under each OS base dir. PascalCase on Windows/macOS (the native
/// `<Company>\<Product>`-style convention — here just the product, no extra `data`/`config`
/// leaf); lowercase on Linux (the XDG convention is a lowercase app id).
#[cfg(not(target_os = "linux"))]
const APP_DIR: &str = "DontSpeak";
#[cfg(target_os = "linux")]
const APP_DIR: &str = "dontspeak";

/// Our roaming user-settings root — `config.toml` and `speakers.json`.
/// Idiomatic, no vendor/`data` leaf, per platform:
///   Windows: `%APPDATA%\DontSpeak`                       (Roaming — settings follow the user)
///   macOS:   `~/Library/Application Support/DontSpeak`
///   Linux:   `$XDG_CONFIG_HOME`/`~/.config/dontspeak`
pub fn data_dir() -> Option<PathBuf> {
    Some(BaseDirs::new()?.config_dir().join(APP_DIR))
}

/// Downloaded model assets (kokoro onnx + voices, parakeet, the onnxruntime dylib) — a
/// `models/` subdir under the OS CACHE root. These are large, machine-specific,
/// re-downloadable blobs, so they belong in the per-OS cache location (Microsoft's
/// guidance: large/regenerable data → `%LOCALAPPDATA%`, not roaming `%APPDATA%`):
///   Windows: `%LOCALAPPDATA%\DontSpeak\models`
///   macOS:   `~/Library/Caches/DontSpeak/models`
///   Linux:   `$XDG_CACHE_HOME`/`~/.cache/dontspeak/models`
pub fn model_dir() -> Option<PathBuf> {
    // Portable / bundled builds ship the models alongside the app and point this at them via
    // DONTSPEAK_MODEL_DIR, so an EXTRACTED, no-install copy reads its bundled models in place
    // (and an offline installer can target the per-user cache explicitly). Empty = ignored.
    if let Some(d) = std::env::var_os("DONTSPEAK_MODEL_DIR")
        && !d.is_empty()
    {
        let path = PathBuf::from(d);
        if path.is_absolute() && path.is_dir() {
            return Some(path);
        }
        eprintln!(
            "dontspeak: ignoring DONTSPEAK_MODEL_DIR={} because it is not an existing absolute directory",
            path.display()
        );
    }
    Some(model_dir_under(BaseDirs::new()?.cache_dir()))
}

fn model_dir_under(cache_dir: &Path) -> PathBuf {
    cache_dir.join(APP_DIR).join("models")
}

fn cache_root(base: &BaseDirs) -> PathBuf {
    base.cache_dir().join(APP_DIR)
}

/// Homebrew onnxruntime dylib on Intel mac (None elsewhere). Single source for loader +
/// `intel_mac_builtin_ort_available`. Versioned file ≥ 1.27 (older deadlocks on SepFormer).
pub fn brew_onnxruntime_dylib() -> Option<PathBuf> {
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        for lib_dir in [
            "/usr/local/opt/onnxruntime/lib",
            "/opt/homebrew/opt/onnxruntime/lib",
        ] {
            let Ok(entries) = std::fs::read_dir(lib_dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if let Some(v) = name
                    .strip_prefix("libonnxruntime.")
                    .and_then(|r| r.strip_suffix(".dylib"))
                    && parse_dylib_version(v).is_some_and(|ver| ver >= (1, 27, 0))
                    && e.path().is_file()
                {
                    return Some(e.path());
                }
            }
        }
        None
    }
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    {
        None
    }
}

/// Parse a dotted dylib version ("1.27.0", "1.27") into a comparable triple; a missing patch reads
/// as 0, anything non-numeric (including the bare "1" major-only symlink, which carries no minor to
/// gate on) is `None`.
#[cfg(any(all(target_os = "macos", target_arch = "x86_64"), test))]
fn parse_dylib_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// FluidAudio Core ML cache under [`model_dir`]/coreml` — explicit so downloads aren't
/// scattered; uninstaller removes the whole DontSpeak cache.
pub fn coreml_dir() -> Option<PathBuf> {
    Some(model_dir()?.join("coreml"))
}

/// Non-empty subdir whose name contains `needle`? Pure; shared by helper + status.
pub fn coreml_model_present_in(dir: &Path, needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    let nonempty = |p: &Path| {
        std::fs::read_dir(p)
            .map(|mut e| e.next().is_some())
            .unwrap_or(false)
    };
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.file_type().map(|t| t.is_dir()).unwrap_or(false)
                    && e.file_name()
                        .to_str()
                        .map(|n| n.to_ascii_lowercase().contains(&needle))
                        .unwrap_or(false)
                    && nonempty(&e.path())
            })
        })
        .unwrap_or(false)
}

/// As `coreml_model_present_in`, probing [`coreml_dir`] — the ONE folder every Core ML
/// model now downloads to. `false` if the cache dir can't resolve.
pub fn coreml_model_present(needle: &str) -> bool {
    coreml_dir()
        .map(|d| coreml_model_present_in(&d, needle))
        .unwrap_or(false)
}

#[cfg(test)]
mod coreml_present_tests {
    use super::coreml_model_present_in;

    #[test]
    fn present_only_for_a_nonempty_matching_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Nothing yet → not present (clean install ⇒ the dot reads "downloading").
        assert!(!coreml_model_present_in(dir, "kokoro"));
        // An EMPTY matching subdir (mid-download / partial) → still not present.
        std::fs::create_dir_all(dir.join("kokoro-82m-coreml")).unwrap();
        assert!(!coreml_model_present_in(dir, "kokoro"));
        // A NON-EMPTY matching subdir → present.
        std::fs::write(dir.join("kokoro-82m-coreml/model.mlmodelc"), b"x").unwrap();
        assert!(coreml_model_present_in(dir, "kokoro"));
        // Case-insensitive substring; a non-matching needle doesn't count.
        assert!(coreml_model_present_in(dir, "KOKORO"));
        assert!(!coreml_model_present_in(dir, "parakeet"));
    }
}

/// Our local machine STATE/runtime root — `stats.toml`, pidfiles, the IPC socket, and
/// (via [`log_path`]) logs. Machine-specific, never roamed:
///   Windows: `%LOCALAPPDATA%\DontSpeak`
///   macOS:   `~/Library/Application Support/DontSpeak`   (macOS has no roaming/local split)
///   Linux:   `$XDG_STATE_HOME`/`~/.local/state/dontspeak`
fn state_root(base: &BaseDirs) -> PathBuf {
    #[cfg(target_os = "windows")]
    let root = base.data_local_dir().to_path_buf();
    #[cfg(target_os = "macos")]
    let root = base.data_dir().to_path_buf();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = base
        .state_dir()
        .unwrap_or_else(|| base.data_dir())
        .to_path_buf();
    root.join(APP_DIR)
}

/// The activity-log file, in each OS's conventional LOG location:
///   Windows: `%LOCALAPPDATA%\DontSpeak\logs\dontspeak.log`   (under the state root)
///   macOS:   `~/Library/Logs/DontSpeak/dontspeak.log`        (the dedicated Logs folder)
///   Linux:   `$XDG_STATE_HOME`/`~/.local/state/dontspeak/logs/dontspeak.log`
fn log_path(base: &BaseDirs, state: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let _ = state;
        base.home_dir()
            .join("Library/Logs")
            .join(APP_DIR)
            .join("dontspeak.log")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = base;
        state.join("logs").join("dontspeak.log")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_model_dir_has_the_platform_app_and_models_suffix() {
        let dir = model_dir_under(Path::new("cache-root"));
        assert_eq!(dir, Path::new("cache-root").join(APP_DIR).join("models"));
    }

    #[test]
    fn client_home_override_expands_relative_and_tilde_paths() {
        let home = Path::new("home");
        let cwd = Path::new("worktree");
        assert_eq!(
            client_config_dir(home, cwd, None, ".codex"),
            home.join(".codex")
        );
        assert_eq!(
            client_config_dir(home, cwd, Some(OsStr::new("profiles/codex")), ".codex"),
            cwd.join("profiles/codex")
        );
        assert_eq!(
            client_config_dir(home, cwd, Some(OsStr::new("~/profiles/qwen")), ".qwen"),
            home.join("profiles/qwen")
        );
    }

    #[cfg(unix)]
    #[test]
    fn client_home_override_preserves_absolute_paths() {
        assert_eq!(
            client_config_dir(
                Path::new("/home/person"),
                Path::new("/work"),
                Some(OsStr::new("/var/lib/codex")),
                ".codex",
            ),
            Path::new("/var/lib/codex")
        );
    }

    /// The brew-probe version gate: full versions parse and compare against the 1.27 floor; the
    /// bare major-only symlink ("1") and junk are rejected (no minor to gate on).
    #[test]
    fn brew_dylib_version_gate() {
        assert_eq!(parse_dylib_version("1.27.0"), Some((1, 27, 0)));
        assert_eq!(parse_dylib_version("1.27"), Some((1, 27, 0)));
        assert!(parse_dylib_version("1.28.1").is_some_and(|v| v >= (1, 27, 0)));
        assert!(parse_dylib_version("1.24.2").is_some_and(|v| v < (1, 27, 0)));
        assert_eq!(parse_dylib_version("1"), None); // major-only symlink
        assert_eq!(parse_dylib_version("1.27.0.extra"), None);
        assert_eq!(parse_dylib_version("current"), None);
    }
}
