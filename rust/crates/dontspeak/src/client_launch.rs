//! Registry-driven `dontspeak <client>` launchers. Makes the resident host available before
//! start; preserves the child's argv, cwd, stdio, and exit status. Codex interactive commands
//! are the exception: they ask the engine to attach its narration subscriber, then receive the
//! endpoint via `--remote`. A host that never comes up degrades the integration for this launch
//! only — never gates the wrapped client; thin passthrough, not a health check.

use std::path::Path;
use std::process::Command;

use ds_config::{ClientSpec, LaunchMode, Paths, VoiceConfig};
use ds_ipc::{Request, Response};

use crate::engine_launch::ensure_engine;

/// Launch registry client; wrapper exit code.
pub(crate) fn run(spec: &ClientSpec, args: &[String]) -> i32 {
    let Some(paths) = Paths::resolve() else {
        eprintln!("dontspeak: could not resolve the user data directory");
        return 1;
    };
    let configured_bin = match spec.launch.mode {
        LaunchMode::Direct => spec.target.as_str(),
        LaunchMode::CodexRemote => {
            let cfg = VoiceConfig::load(&paths);
            return run_codex(spec, args, &paths, &cfg.codex_bin);
        }
    };
    run_direct(spec, args, &paths, configured_bin, true)
}

fn run_codex(spec: &ClientSpec, args: &[String], paths: &Paths, configured_bin: &str) -> i32 {
    let invocation = codex_invocation(args);
    if invocation == CodexInvocation::CustomRemote {
        eprintln!(
            "dontspeak codex: `--remote` is managed by DontSpeak; use `codex` directly for a custom remote endpoint"
        );
        return 2;
    }
    let Some((bin, app_server_bin)) = resolve_codex_launch_bins(spec, paths, configured_bin) else {
        eprintln!(
            "dontspeak {}: client executable {configured_bin:?} was not found",
            spec.target.as_str()
        );
        return 127;
    };
    match invocation {
        CodexInvocation::Direct => run_resolved(spec, args, &bin),
        CodexInvocation::CustomRemote => {
            unreachable!("custom remotes return before executable resolution")
        }
        CodexInvocation::Narrated => match prepare_codex_stream(paths, &app_server_bin) {
            Some(endpoint) => {
                let mut remote_args = vec!["--remote".to_string(), endpoint];
                remote_args.extend_from_slice(args);
                run_resolved(spec, &remote_args, &bin)
            }
            // Host/engine failure never blocks Codex — Stop-only narration, same as bare `codex`.
            None => run_resolved(spec, args, &bin),
        },
    }
}

fn resolve_codex_launch_bins(
    spec: &ClientSpec,
    paths: &Paths,
    configured_bin: &str,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let bin = ds_config::resolve_configured_client_binary(spec.target, paths, configured_bin)?;
    let app_server_bin =
        ds_config::resolve_native_client_binary(spec.target, paths, configured_bin)
            .unwrap_or_else(|| bin.clone());
    Some((bin, app_server_bin))
}

/// Best-effort Codex narration prep; failure does not block launch.
fn prepare_codex_stream(paths: &Paths, codex_bin: &Path) -> Option<String> {
    if !ensure_engine(&paths.engine_sock) {
        eprintln!(
            "dontspeak codex: the DontSpeak host did not become ready; launching without narration"
        );
        return None;
    }
    let request = Request::EnsureCodexStream {
        codex_bin: codex_bin.to_string_lossy().into_owned(),
    };
    match ds_ipc::request(&paths.engine_sock, &request) {
        Ok(Response::CodexStreamReady { endpoint }) => Some(endpoint),
        Ok(Response::Error { message }) => {
            eprintln!("dontspeak codex: {message}; launching without narration");
            None
        }
        Ok(other) => {
            eprintln!(
                "dontspeak codex: engine returned an unexpected response: {other:?}; launching without narration"
            );
            None
        }
        Err(error) => {
            eprintln!(
                "dontspeak codex: could not prepare narration: {error}; launching without narration"
            );
            None
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
    // Host failure degrades integration only; never keeps the wrapped client from starting.
    if ensure_host && !information_only(args) && !ensure_engine(&paths.engine_sock) {
        eprintln!(
            "dontspeak {}: the DontSpeak host did not become ready; launching without DontSpeak integration",
            spec.target.as_str()
        );
    }
    let Some(bin) = ds_config::resolve_configured_client_binary(spec.target, paths, configured_bin)
    else {
        eprintln!(
            "dontspeak {}: client executable {configured_bin:?} was not found",
            spec.target.as_str()
        );
        return 127;
    };
    run_resolved(spec, args, &bin)
}

fn run_resolved(spec: &ClientSpec, args: &[String], bin: &Path) -> i32 {
    match client_command(bin, args, spec.target).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "dontspeak {}: could not start {}: {error}",
                spec.target.as_str(),
                bin.display()
            );
            1
        }
    }
}

fn client_command(bin: &Path, args: &[String], client: ds_config::WiredAgent) -> Command {
    let mut command = Command::new(bin);
    command.args(args).env(
        crate::session_scope::DONTSPEAK_SESSION_ID,
        crate::session_scope::new_launcher_session(client),
    );
    command
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

/// Interactive TUI only accepts `--remote` for base TUI, `resume`, and `fork`. Management /
/// noninteractive commands pass through. Global option values are skipped so
/// `codex -C path exec …` classifies on `exec`, not `path`.
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
        // Unrecognized positional = base TUI's optional initial prompt.
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

#[cfg(test)]
mod tests {
    use super::*;
    use ds_config::WiredAgent;
    use std::ffi::OsString;

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
    fn client_command_preserves_program_and_argument_boundaries() {
        let values = args(&["prompt with spaces", "--flag", "value", ""]);
        let command = client_command(
            Path::new("client-bin"),
            &values,
            ds_config::WiredAgent::Codex,
        );

        assert_eq!(command.get_program(), "client-bin");
        assert_eq!(
            command.get_args().map(OsString::from).collect::<Vec<_>>(),
            values
                .iter()
                .map(|arg| OsString::from(arg.as_str()))
                .collect::<Vec<_>>()
        );
        let session = command
            .get_envs()
            .find(|(name, _)| *name == crate::session_scope::DONTSPEAK_SESSION_ID)
            .and_then(|(_, value)| value)
            .expect("launcher must give hooks and MCP a shared session")
            .to_string_lossy();
        assert!(session.starts_with("dontspeak:launch:"));
    }

    #[test]
    fn direct_launch_propagates_exit_status_and_missing_binary_is_127() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(temp.path());
        let spec = ds_config::client_spec(WiredAgent::Codex);

        #[cfg(windows)]
        let (shell, shell_args) = (
            std::path::PathBuf::from(std::env::var_os("COMSPEC").expect("COMSPEC")),
            args(&["/C", "exit 23"]),
        );
        #[cfg(not(windows))]
        let (shell, shell_args) = (
            std::path::PathBuf::from("/bin/sh"),
            args(&["-c", "exit 23"]),
        );

        assert_eq!(
            run_direct(spec, &shell_args, &paths, shell.to_str().unwrap(), false,),
            23
        );
        assert_eq!(
            run_direct(
                spec,
                &[],
                &paths,
                temp.path().join("missing-client").to_str().unwrap(),
                false,
            ),
            127
        );
    }

    #[test]
    fn custom_remote_rejection_precedes_binary_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::rooted_at(temp.path());
        let spec = ds_config::client_spec(WiredAgent::Codex);

        assert_eq!(
            run_codex(
                spec,
                &args(&["--remote", "ws://127.0.0.1:9"]),
                &paths,
                "missing-codex",
            ),
            2
        );
    }

    #[cfg(windows)]
    #[test]
    fn codex_app_server_uses_native_payload_instead_of_npm_shim() {
        let temp = tempfile::tempdir().unwrap();
        let mut paths = Paths::rooted_at(temp.path());
        let shim_dir = temp.path().join("npm");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("codex.cmd");
        std::fs::write(&shim, b"fixture").unwrap();
        paths.path_env = Some(std::env::join_paths([&shim_dir]).unwrap());

        let native = paths
            .home
            .join("AppData/Roaming")
            .join(ds_config::codex_native_windows_dir(std::env::consts::ARCH))
            .join("codex.exe");
        std::fs::create_dir_all(native.parent().unwrap()).unwrap();
        std::fs::write(&native, b"fixture").unwrap();

        let spec = ds_config::client_spec(WiredAgent::Codex);
        let (direct, app_server) = resolve_codex_launch_bins(spec, &paths, "codex").unwrap();
        assert_eq!(direct, shim);
        assert_eq!(app_server, native);
    }
}
