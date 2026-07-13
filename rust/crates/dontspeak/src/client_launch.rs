//! Registry-driven `dontspeak <client>` launchers. The wrapper makes the resident host
//! available before a client starts and otherwise preserves the child's argv, cwd, stdio,
//! and exit status. Codex's interactive commands are the one exception: they first ask the
//! engine to attach its narration subscriber, then receive the same endpoint via `--remote`.

use std::path::{Path, PathBuf};
use std::process::Command;

use ds_config::{ClientSpec, LaunchMode, Paths, VoiceConfig};
use ds_ipc::{Request, Response};

use crate::engine_launch::ensure_engine;

/// Launch one registry client and return the exit code this wrapper should expose.
pub(crate) fn run(spec: &ClientSpec, args: &[String]) -> i32 {
    let Some(paths) = Paths::resolve() else {
        eprintln!("dontspeak: could not resolve the user data directory");
        return 1;
    };
    let configured_bin = match spec.launch.mode {
        LaunchMode::Direct => spec.launch.command,
        LaunchMode::CodexRemote => {
            let cfg = VoiceConfig::load(&paths);
            return run_codex(spec, args, &paths, &cfg.codex_bin);
        }
    };
    run_direct(spec, args, &paths, configured_bin, true)
}

fn run_codex(spec: &ClientSpec, args: &[String], paths: &Paths, configured_bin: &str) -> i32 {
    match codex_invocation(args) {
        CodexInvocation::Direct => run_direct(spec, args, paths, configured_bin, false),
        CodexInvocation::CustomRemote => {
            eprintln!(
                "dontspeak codex: `--remote` is managed by DontSpeak; use `codex` directly for a custom remote endpoint"
            );
            2
        }
        CodexInvocation::Narrated => {
            if !ensure_engine(&paths.engine_sock) {
                eprintln!("dontspeak codex: the DontSpeak host did not become ready");
                return 1;
            }
            let endpoint = match ds_ipc::request(&paths.engine_sock, &Request::EnsureCodexStream) {
                Ok(Response::CodexStreamReady { endpoint }) => endpoint,
                Ok(Response::Error { message }) => {
                    eprintln!("dontspeak codex: {message}");
                    return 1;
                }
                Ok(other) => {
                    eprintln!("dontspeak codex: engine returned an unexpected response: {other:?}");
                    return 1;
                }
                Err(error) => {
                    eprintln!("dontspeak codex: could not prepare narration: {error}");
                    return 1;
                }
            };
            let mut remote_args = vec!["--remote".to_string(), endpoint];
            remote_args.extend_from_slice(args);
            run_direct(spec, &remote_args, paths, configured_bin, false)
        }
    }
}

fn run_direct(
    spec: &ClientSpec,
    args: &[String],
    paths: &Paths,
    configured_bin: &str,
    ensure_host: bool,
) -> i32 {
    if ensure_host && !information_only(args) && !ensure_engine(&paths.engine_sock) {
        eprintln!(
            "dontspeak {}: the DontSpeak host did not become ready",
            spec.launch.command
        );
        return 1;
    }
    let Some(bin) = resolve_client_bin(configured_bin, spec.launch.command, paths) else {
        eprintln!(
            "dontspeak {}: client executable {configured_bin:?} was not found",
            spec.launch.command
        );
        return 127;
    };
    match Command::new(&bin).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "dontspeak {}: could not start {}: {error}",
                spec.launch.command,
                bin.display()
            );
            1
        }
    }
}

fn information_only(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help" | "-V" | "--version"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexInvocation {
    Narrated,
    Direct,
    CustomRemote,
}

/// Decide whether this is an interactive TUI invocation. Codex only accepts `--remote` for
/// its base TUI, `resume`, and `fork`; management and noninteractive commands must pass
/// through untouched. Global option values are skipped so `codex -C path exec …` is still
/// classified by `exec` rather than by `path`.
fn codex_invocation(args: &[String]) -> CodexInvocation {
    if args
        .iter()
        .any(|arg| arg == "--remote" || arg.starts_with("--remote="))
    {
        return CodexInvocation::CustomRemote;
    }
    let Some(command) = first_codex_positional(args) else {
        return if information_only(args) {
            CodexInvocation::Direct
        } else {
            CodexInvocation::Narrated
        };
    };
    match command {
        "resume" | "fork" => CodexInvocation::Narrated,
        "exec" | "e" | "review" | "login" | "logout" | "mcp" | "plugin" | "mcp-server"
        | "app-server" | "remote-control" | "app" | "completion" | "update" | "doctor"
        | "sandbox" | "debug" | "apply" | "a" | "archive" | "delete" | "unarchive" | "cloud"
        | "exec-server" | "features" | "help" => CodexInvocation::Direct,
        // An unrecognized positional is the base TUI's optional initial prompt.
        _ => CodexInvocation::Narrated,
    }
}

fn first_codex_positional(args: &[String]) -> Option<&str> {
    const TAKES_VALUE: &[&str] = &[
        "-c",
        "--config",
        "--enable",
        "--disable",
        "--remote-auth-token-env",
        "-i",
        "--image",
        "-m",
        "--model",
        "--local-provider",
        "-p",
        "--profile",
        "-s",
        "--sandbox",
        "-C",
        "--cd",
        "--add-dir",
        "-a",
        "--ask-for-approval",
    ];
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if arg == "--" {
            return None;
        }
        if TAKES_VALUE.contains(&arg.as_str()) {
            skip_value = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

fn resolve_client_bin(configured: &str, command: &str, paths: &Paths) -> Option<PathBuf> {
    #[cfg(not(windows))]
    let _ = command;
    let configured_path = Path::new(configured);
    if configured_path.is_absolute() || configured_path.components().count() > 1 {
        return configured_path
            .is_file()
            .then(|| configured_path.to_path_buf());
    }
    let mut dirs = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    dirs.extend([
        paths.home.join(".local/bin"),
        paths.home.join(".grok/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ]);
    #[cfg(windows)]
    {
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.home.join("AppData/Roaming"));
        dirs.push(roaming.join("npm"));
        if command == "codex" {
            let target = if cfg!(target_arch = "aarch64") {
                "aarch64-pc-windows-msvc"
            } else {
                "x86_64-pc-windows-msvc"
            };
            dirs.push(
                roaming
                    .join("npm/node_modules/@openai/codex/node_modules")
                    .join(if cfg!(target_arch = "aarch64") {
                        "@openai/codex-win32-arm64"
                    } else {
                        "@openai/codex-win32-x64"
                    })
                    .join("vendor")
                    .join(target)
                    .join("bin"),
            );
        }
    }
    resolve_in_dirs(configured, &dirs)
}

fn resolve_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        #[cfg(windows)]
        for suffix in [".exe", ".com", ".cmd", ".bat", ""] {
            let candidate = dir.join(format!("{name}{suffix}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        #[cfg(not(windows))]
        {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn codex_only_uses_remote_for_interactive_commands() {
        for values in [
            &[][..],
            &["fix the tests"],
            &["resume", "--last"],
            &["-C", "repo", "fork", "--last"],
        ] {
            assert_eq!(
                codex_invocation(&args(values)),
                CodexInvocation::Narrated,
                "{values:?}"
            );
        }
        for values in [
            &["--version"][..],
            &["exec", "echo", "hi"],
            &["-C", "repo", "review"],
            &["mcp", "list"],
            &["archive", "session-id"],
        ] {
            assert_eq!(
                codex_invocation(&args(values)),
                CodexInvocation::Direct,
                "{values:?}"
            );
        }
        assert_eq!(
            codex_invocation(&args(&["--remote", "ws://127.0.0.1:9"])),
            CodexInvocation::CustomRemote
        );
    }

    #[test]
    fn executable_resolution_obeys_directory_order_and_platform_suffixes() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        let filename = "agent.cmd";
        #[cfg(not(windows))]
        let filename = "agent";
        std::fs::write(second.path().join(filename), "fixture").unwrap();
        let dirs = vec![first.path().to_path_buf(), second.path().to_path_buf()];
        let expected = second.path().join(filename);
        assert_eq!(
            resolve_in_dirs("agent", &dirs).as_deref(),
            Some(expected.as_path())
        );
    }
}
