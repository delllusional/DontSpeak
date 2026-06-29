//! Well-known paths resolved once from `$HOME`, plus the per-OS data/model dirs.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// All the well-known paths, resolved once from $HOME.
#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub claude_dir: PathBuf,
    pub hooks_dir: PathBuf,
    /// Speaker pidfile (`speak-hook.pid` in the local `state_dir`) — the TTS
    /// process-group id for the single-speaker barge-in contract.
    pub pidfile: PathBuf,
    /// The ONE unified activity log every dontspeak process appends to
    /// (`~/Library/Logs/dontspeak.log`): engine + hooks + mcp share it via [`crate::log`],
    /// one leveled format, with sudo-free in-process size rotation.
    pub log_file: PathBuf,
    pub settings_json: PathBuf,
    /// Claude Code's `keybindings.json` (`~/.claude/keybindings.json`) — READ-ONLY for the
    /// `claude_code` STT engine to learn which key `voice:pushToTalk` is bound to. DontSpeak
    /// never writes it.
    pub keybindings_json: PathBuf,
    /// Side file holding the running `ds-narrate` pid (for `--stop`).
    pub narrate_pid: PathBuf,
    /// Side file holding the running `dontspeakd` engine pid (for the GUI's
    /// SIGHUP-to-reload nudge + liveness probe). DISTINCT from the SPEAKER
    /// `pidfile` (a TTS pgid) and the `narrate_pid` (the narrator pid).
    pub engine_pid: PathBuf,
    /// Unix-domain socket the engine listens on for RPC (`ds-ipc`). Clients (the
    /// SwiftUI app via ds-core, and the speak/narrate hooks) connect here;
    /// absence means "engine down" so clients fall back to their legacy path.
    pub engine_sock: PathBuf,
    /// `stats.toml` in the local state dir — persisted lifetime usage totals (`tts_secs`,
    /// `stt_secs`) shown under the Status panel's "DontSpeak" row. Written atomically after
    /// each utterance.
    pub stats_toml: PathBuf,
    /// OUR roaming SETTINGS root — see [`data_dir`]. Holds the user-portable config
    /// (`config.toml`, `speakers.json`, `narration-spec.md`). Per-OS idiomatic:
    /// `%APPDATA%\DontSpeak` (Windows), `~/Library/Application Support/DontSpeak` (macOS),
    /// `$XDG_CONFIG_HOME`/`~/.config/dontspeak` (Linux). A neutral home, NOT tied to any
    /// one client, so a user's config doesn't squat in `~/.claude`.
    pub config_dir: PathBuf,
    /// OUR local STATE/runtime root — machine-specific, NEVER roamed. Holds `stats.toml`,
    /// the pidfiles, and the IPC socket. Per-OS idiomatic: `%LOCALAPPDATA%\DontSpeak`
    /// (Windows), `~/Library/Application Support/DontSpeak` (macOS — same as config; macOS
    /// has no roaming/local split), `$XDG_STATE_HOME`/`~/.local/state/dontspeak` (Linux).
    /// Downloaded models live separately under [`model_dir`] (the OS CACHE dir).
    pub state_dir: PathBuf,
    /// `config.toml` in [`data_dir`] — the single source of truth for DontSpeak's own
    /// settings (voice, narration, engines, MCP-HTTP). Replaces the old `dontspeak`
    /// block in `~/.claude/settings.json`. The engine hot-reloads on its mtime.
    pub config_toml: PathBuf,
    /// `speakers.json` in the roaming `config_dir` ([`data_dir`]) — enrolled speaker
    /// voiceprints (name → WeSpeaker embedding) used to label diarization output by name.
    /// Written atomically on each `enroll`/`forget_speaker`. See [`crate::speakers`].
    pub speakers_json: PathBuf,
    /// `narration-spec.md` in [`data_dir`] — the USER-EDITABLE markdown spec injected into
    /// Claude every turn (via the `UserPromptSubmit` `provide` hook, when `narrate` includes
    /// `digests`) telling it to lead each reply with a plain-text spoken line in a blockquote. The engine
    /// writes [`crate::DEFAULT_NARRATION_SPEC`] here on startup if it's absent; edit it to shape how
    /// replies are narrated.
    pub narration_spec: PathBuf,
    /// Claude DESKTOP's config dir — `BaseDirs::config_dir()/Claude`, which is
    /// `~/Library/Application Support/Claude` (macOS), `%APPDATA%\Claude` (Windows),
    /// `~/.config/Claude` (Linux). Its existence is how `wire-desktop` detects that
    /// Claude Desktop has run at least once. (Distinct from `claude_dir` = Claude
    /// CODE's `~/.claude`.)
    pub claude_desktop_dir: PathBuf,
    /// Claude Desktop's `claude_desktop_config.json` — where `wire-desktop` adds (or
    /// removes) the `mcpServers.dontspeak` stdio entry so Desktop can spawn our MCP
    /// bridge. Desktop has no hook system, so this is registration ONLY (no narration).
    pub claude_desktop_config: PathBuf,
    /// OpenAI Codex CLI's config dir (`~/.codex`). Its existence is how `wire-hooks`
    /// auto-detects Codex and decides whether to wire the narration hooks.
    pub codex_dir: PathBuf,
    /// Codex's `~/.codex/config.toml` — where `wire-hooks` adds (or removes) the
    /// `UserPromptSubmit`→`provide` (narration spec) and `Stop`→`notify` (speak reply) hooks.
    pub codex_config: PathBuf,
}

impl Paths {
    /// Resolve from the current user's home. Fails only if $HOME is unset.
    pub fn resolve() -> Option<Self> {
        let base = BaseDirs::new()?;
        let home = base.home_dir().to_path_buf();
        let claude_dir = home.join(".claude");
        let hooks_dir = claude_dir.join("hooks");
        let codex_dir = home.join(".codex");
        // Two roots, each idiomatic per OS (see [`data_dir`] / [`model_dir`] / `state_root`):
        //   config (roaming, user SETTINGS): config.toml, speakers.json, narration-spec.md
        //   state  (local, machine RUNTIME): stats.toml, pidfiles, the IPC socket, logs
        // On Windows/Linux these resolve to distinct OS dirs (roaming vs local / config vs
        // state); on macOS both are Application Support. The engine create_dir_all's both on
        // startup, and every writer create_dir_all's its own parent too.
        let config_dir = data_dir()?;
        let state_dir = state_root(&base);
        // Claude Desktop keeps its config under the OS roaming-config dir (Application
        // Support on macOS, %APPDATA% on Windows, ~/.config on Linux) — exactly what
        // `BaseDirs::config_dir()` resolves, so no per-OS branching here.
        let claude_desktop_dir = base.config_dir().join("Claude");
        let claude_desktop_config = claude_desktop_dir.join("claude_desktop_config.json");
        Some(Self {
            // Runtime/state files live under the LOCAL state root (machine-specific, never
            // roamed); settings live under the roaming config root.
            pidfile: state_dir.join("speak-hook.pid"),
            log_file: log_path(&base, &state_dir),
            settings_json: claude_dir.join("settings.json"),
            keybindings_json: claude_dir.join("keybindings.json"),
            narrate_pid: state_dir.join("narrate.pid"),
            engine_pid: state_dir.join("dontspeakd.pid"),
            engine_sock: state_dir.join("dontspeak.sock"),
            stats_toml: state_dir.join("stats.toml"),
            config_toml: config_dir.join("config.toml"),
            speakers_json: config_dir.join("speakers.json"),
            narration_spec: config_dir.join("narration-spec.md"),
            config_dir,
            state_dir,
            claude_desktop_dir,
            claude_desktop_config,
            codex_config: codex_dir.join("config.toml"),
            codex_dir,
            home,
            claude_dir,
            hooks_dir,
        })
    }

    /// Build a `Paths` rooted at an explicit `home` dir WITHOUT reading or writing
    /// any environment variable. The env-free fallback for when [`resolve`](Paths::resolve)
    /// returns `None` (no `$HOME`): the engine factory returns an INERT engine box that
    /// fail-quiets at speak time, so it must NOT mutate the process environment (a
    /// `set_var("HOME", …)` is unsound once other threads are running). Every file is
    /// rooted under `home` so the result is total; this path is never used to write a
    /// real session, so the exact (non-OS-conventional) layout here is immaterial.
    pub fn rooted_at(home: &Path) -> Self {
        let home = home.to_path_buf();
        let claude_dir = home.join(".claude");
        let hooks_dir = claude_dir.join("hooks");
        let codex_dir = home.join(".codex");
        let ds_dir = home.join(".dontspeak");
        let claude_desktop_dir = home.join("Claude");
        let claude_desktop_config = claude_desktop_dir.join("claude_desktop_config.json");
        Self {
            pidfile: ds_dir.join("speak-hook.pid"),
            log_file: home.join("dontspeak.log"),
            settings_json: claude_dir.join("settings.json"),
            keybindings_json: claude_dir.join("keybindings.json"),
            narrate_pid: ds_dir.join("narrate.pid"),
            engine_pid: ds_dir.join("dontspeakd.pid"),
            engine_sock: ds_dir.join("dontspeak.sock"),
            stats_toml: ds_dir.join("stats.toml"),
            config_toml: ds_dir.join("config.toml"),
            speakers_json: ds_dir.join("speakers.json"),
            narration_spec: ds_dir.join("narration-spec.md"),
            // The inert fallback uses ONE dir for both roots (layout is immaterial here).
            config_dir: ds_dir.clone(),
            state_dir: ds_dir,
            claude_desktop_dir,
            claude_desktop_config,
            codex_config: codex_dir.join("config.toml"),
            codex_dir,
            home,
            claude_dir,
            hooks_dir,
        }
    }

    /// True if Claude Desktop appears installed: its config dir exists (it has run at
    /// least once) OR its app/install location is present (installed, maybe never
    /// launched). Gates the optional Desktop MCP registration so we never scatter a
    /// stray `Claude/` config dir on a machine that doesn't have Desktop.
    pub fn claude_desktop_present(&self) -> bool {
        if self.claude_desktop_dir.exists() {
            return true;
        }
        #[cfg(target_os = "macos")]
        {
            std::path::Path::new("/Applications/Claude.app").exists()
                || self.home.join("Applications/Claude.app").exists()
        }
        #[cfg(target_os = "windows")]
        {
            // Claude Desktop installs per-user under %LOCALAPPDATA%.
            self.home.join("AppData/Local/AnthropicClaude").exists()
                || self.home.join("AppData/Local/Programs/claude").exists()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            // No official Claude Desktop on Linux — the config dir check above is the
            // only signal (community builds use ~/.config/Claude).
            false
        }
    }

    /// `~/.claude/hooks/mic-active` — CoreAudio helper used to stop speech when
    /// the user starts dictating (mic active edge).
    pub fn mic_active(&self) -> PathBuf {
        self.hooks_dir.join("mic-active")
    }
}

/// Our brand subfolder under each OS base dir. PascalCase on Windows/macOS (the native
/// `<Company>\<Product>`-style convention — here just the product, no extra `data`/`config`
/// leaf); lowercase on Linux (the XDG convention is a lowercase app id).
#[cfg(not(target_os = "linux"))]
const APP_DIR: &str = "DontSpeak";
#[cfg(target_os = "linux")]
const APP_DIR: &str = "dontspeak";

/// Our roaming/user SETTINGS root — `config.toml`, `speakers.json`, `narration-spec.md`.
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
        return Some(PathBuf::from(d));
    }
    Some(BaseDirs::new()?.cache_dir().join(APP_DIR).join("models"))
}

/// The FluidAudio Core ML / ANE model cache (Kokoro TTS, Parakeet STT, diarization), a
/// `coreml/` subdir under [`model_dir`]. We pass this EXPLICITLY to the shim's
/// `smk_*_init` so FluidAudio downloads here instead of its own scattered defaults
/// (`~/.cache/fluidaudio/Models` for Kokoro, `~/Library/Application Support/FluidAudio` for
/// ASR/diarization) — so every download lives under the one DontSpeak cache folder that the
/// uninstaller removes wholesale. FluidAudio creates its per-model subdirs under it.
///   macOS: `~/Library/Caches/DontSpeak/models/coreml`
pub fn coreml_dir() -> Option<PathBuf> {
    Some(model_dir()?.join("coreml"))
}

/// Whether a FluidAudio Core ML model whose subdir name contains `needle` is fully present in
/// `dir`: a non-empty matching subdir = installed; absent/empty = still fetching / not there.
/// PURE (the disk read is the only effect), so it's unit-testable against a temp dir. Shared
/// by the warm helper (to emit "downloading") and the engine status (diarization presence).
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

/// As [`coreml_model_present_in`], probing [`coreml_dir`] — the ONE folder every Core ML
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
    fn model_dir_resolves() {
        assert!(model_dir().is_some());
    }
}
