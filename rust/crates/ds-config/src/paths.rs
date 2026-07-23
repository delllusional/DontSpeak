//! Well-known paths from `$HOME` + per-OS data/model dirs.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// Well-known paths, resolved once from $HOME.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    /// Unified activity log (per-OS logs dir); shared via `ds_log`.
    pub log_file: PathBuf,
    pub settings_json: PathBuf,
    /// Claude Code keybindings — READ-ONLY for `claude_code` STT (`voice:pushToTalk`).
    pub keybindings_json: PathBuf,
    /// `~/.claude.json` — MCP half of `wire claude` (hooks are in `settings_json`).
    pub claude_code_config: PathBuf,
    /// Running `ds-narrate` pid.
    pub narrate_pid: PathBuf,
    /// Engine pid (reload/liveness). Not `narrate_pid`.
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
    /// `~/.codex` — Codex config and managed-install root.
    pub codex_dir: PathBuf,
    /// Codex hooks file.
    pub codex_config: PathBuf,
    /// `~/.qwen` — Qwen Code config root.
    pub qwen_dir: PathBuf,
    /// Qwen hooks + MCP (one file).
    pub qwen_settings: PathBuf,
    /// `~/.grok` — Grok config and client-binary root.
    pub grok_dir: PathBuf,
    /// Grok MCP entry.
    pub grok_config: PathBuf,
    /// Grok dedicated hooks file (unwire deletes).
    pub grok_hooks_json: PathBuf,
    /// Grok global rules; managed narrate section (hook stdout ignored — issue #95).
    pub grok_agents_md: PathBuf,
    /// `~/.kimi-code` — Kimi Code config root.
    pub kimi_dir: PathBuf,
    /// Kimi Code hooks file (flat `[[hooks]]` array-of-tables).
    pub kimi_config_toml: PathBuf,
    /// Kimi Code MCP entry (`mcpServers.DontSpeak`, Claude shape).
    pub kimi_mcp_json: PathBuf,
    /// Kimi Code OAuth credentials (usage stats; read-only).
    pub kimi_credentials_json: PathBuf,
    /// `~/.hermes` — Hermes config root.
    pub hermes_dir: PathBuf,
    /// Hermes shell hooks + MCP (`hooks:` / `mcp_servers.DontSpeak` in config.yaml).
    pub hermes_config_yaml: PathBuf,
    /// Hermes first-use consent for shell hooks (`(event, command)` approvals).
    pub hermes_shell_hooks_allowlist: PathBuf,
    /// `$PATH` captured once at [`Self::resolve`]. Tests may replace this with a synthetic path.
    pub path_env: Option<OsString>,
    /// Whether the shared client-binary resolver may consult other ambient install sources.
    /// Kept separate from [`Self::path_env`] so a synthetic test path does not enable host
    /// overrides, login-shell PATH, APPDATA, or machine-global directories.
    pub(crate) live_client_environment: bool,
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
        let kimi_dir = client_config_dir(
            &home,
            &cwd,
            std::env::var_os("KIMI_CODE_HOME").as_deref(),
            ".kimi-code",
        );
        let hermes_dir = client_config_dir(
            &home,
            &cwd,
            std::env::var_os("HERMES_HOME").as_deref(),
            ".hermes",
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
            kimi_config_toml: kimi_dir.join("config.toml"),
            kimi_mcp_json: kimi_dir.join("mcp.json"),
            kimi_credentials_json: kimi_dir.join("credentials").join("kimi-code.json"),
            kimi_dir,
            hermes_config_yaml: hermes_dir.join("config.yaml"),
            hermes_shell_hooks_allowlist: hermes_dir.join("shell-hooks-allowlist.json"),
            hermes_dir,
            home,
            claude_dir,
            path_env: std::env::var_os("PATH"),
            live_client_environment: true,
        })
    }

    /// Env-free Paths under `home` (inert engine when resolve fails). No `set_var`.
    /// Layout immaterial — not a real session.
    pub fn rooted_at(home: &Path) -> Self {
        let home = home.to_path_buf();
        let claude_dir = home.join(".claude");
        let codex_dir = home.join(".codex");
        let qwen_dir = home.join(".qwen");
        let grok_dir = home.join(".grok");
        let kimi_dir = home.join(".kimi-code");
        let hermes_dir = home.join(".hermes");
        let ds_dir = home.join(".dontspeak");
        Self {
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
            kimi_config_toml: kimi_dir.join("config.toml"),
            kimi_mcp_json: kimi_dir.join("mcp.json"),
            kimi_credentials_json: kimi_dir.join("credentials").join("kimi-code.json"),
            kimi_dir,
            hermes_config_yaml: hermes_dir.join("config.yaml"),
            hermes_shell_hooks_allowlist: hermes_dir.join("shell-hooks-allowlist.json"),
            hermes_dir,
            home,
            claude_dir,
            path_env: None,
            live_client_environment: false,
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

/// Brand subfolder: PascalCase (Win/macOS), lowercase (Linux XDG).
#[cfg(not(target_os = "linux"))]
const APP_DIR: &str = "DontSpeak";
#[cfg(target_os = "linux")]
const APP_DIR: &str = "dontspeak";

/// Roaming settings root (`config.toml`, `speakers.json`).
pub fn data_dir() -> Option<PathBuf> {
    Some(BaseDirs::new()?.config_dir().join(APP_DIR))
}

/// Model cache (`…/DontSpeak/models`). Override: absolute `DONTSPEAK_MODEL_DIR`.
pub fn model_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("DONTSPEAK_MODEL_DIR")
        && !d.is_empty()
    {
        let path = PathBuf::from(d);
        if path.is_absolute() && path.is_dir() {
            return Some(path);
        }
        log::warn!(
            target: "config",
            "ignoring DONTSPEAK_MODEL_DIR={} because it is not an existing absolute directory",
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

/// Homebrew onnxruntime dylib on Intel mac (None elsewhere) — the loader's last resort behind
/// the app-bundled and downloaded copies. Floor 1.23 = the workspace `ort` api level; SepFormer,
/// which wanted 1.27, is Apple-Silicon-only (its diarizer rung is MLX).
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
                    && parse_dylib_version(v).is_some_and(|ver| ver >= (1, 23, 0))
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

/// MLX model cache under [`model_dir`]/mlx — explicit so downloads stay together.
pub fn mlx_dir() -> Option<PathBuf> {
    Some(model_dir()?.join("mlx"))
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
    fn kimi_paths_follow_the_kimi_code_home_layout() {
        let paths = Paths::rooted_at(Path::new("home"));
        assert_eq!(paths.kimi_dir, Path::new("home").join(".kimi-code"));
        assert_eq!(paths.kimi_config_toml, paths.kimi_dir.join("config.toml"));
        assert_eq!(paths.kimi_mcp_json, paths.kimi_dir.join("mcp.json"));
        assert_eq!(
            paths.kimi_credentials_json,
            paths.kimi_dir.join("credentials").join("kimi-code.json")
        );
    }

    #[test]
    fn hermes_paths_follow_the_hermes_home_layout() {
        let paths = Paths::rooted_at(Path::new("home"));
        assert_eq!(paths.hermes_dir, Path::new("home").join(".hermes"));
        assert_eq!(
            paths.hermes_config_yaml,
            paths.hermes_dir.join("config.yaml")
        );
        assert_eq!(
            paths.hermes_shell_hooks_allowlist,
            paths.hermes_dir.join("shell-hooks-allowlist.json")
        );
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

    /// The brew-probe version gate: full versions parse and compare against the 1.23 floor (the
    /// `ort` api level); the bare major-only symlink ("1") and junk are rejected (no minor to
    /// gate on).
    #[test]
    fn brew_dylib_version_gate() {
        assert_eq!(parse_dylib_version("1.27.0"), Some((1, 27, 0)));
        assert_eq!(parse_dylib_version("1.27"), Some((1, 27, 0)));
        assert!(parse_dylib_version("1.23.2").is_some_and(|v| v >= (1, 23, 0)));
        assert!(parse_dylib_version("1.22.0").is_some_and(|v| v < (1, 23, 0)));
        assert_eq!(parse_dylib_version("1"), None); // major-only symlink
        assert_eq!(parse_dylib_version("1.27.0.extra"), None);
        assert_eq!(parse_dylib_version("current"), None);
    }
}
