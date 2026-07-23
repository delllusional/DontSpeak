//! Unrecognized first argument must fail fast (exit 2), never fall through to the stdio MCP
//! server (blocks on stdin forever). Regression for `dontspeak <typo>` / old binary + newer
//! `wire` subcommand silently becoming the stdin-blocking server.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Spawn built `dontspeak` with `args` (stdin closed), wait up to `timeout`. Panic if it
/// does not exit — that panic *is* the hang regression. stdin null so a regressed build that
/// falls into `mcp::serve()` reads EOF and exits (code != 2); assertions still catch it.
///
/// Real subprocess → production `main()` → `ds_log::init()`. `DONTSPEAK_LOG_FILE` redirects
/// the log into a tempdir (HOME/LOCALAPPDATA overrides don't work cross-platform — Windows
/// known-folder API ignores child env for LOCALAPPDATA). See issue #26.
fn run_bounded(args: &[&str], timeout: Duration) -> i32 {
    let home = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_dontspeak"))
        .args(args)
        .env("DONTSPEAK_LOG_FILE", home.path().join("dontspeak.log"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn dontspeak");
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status.code().unwrap_or(-1);
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "`dontspeak {}` did not exit within {timeout:?} — it HUNG (regression: an \
                 unrecognized subcommand fell through to the stdin-blocking MCP server)",
                args.join(" ")
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Result of a bare `dontspeak` spawn. Keeps `home` alive so callers can inspect redirected
/// paths (`GROK_HOME` AGENTS.md, log file) after the child exits.
struct BareLaunch {
    output: Output,
    /// Absolute `GROK_HOME` when the Grok hook arm ran; None for MCP.
    grok_home: Option<PathBuf>,
    /// Owns the temp tree for the lifetime of assertions.
    _home: tempfile::TempDir,
}

/// No-arg binary with one stdin document. Pins the Grok env-only hook discriminator without
/// a real engine: same JSON-RPC ping is an MCP reply unmarked, a no-op unknown hook when
/// GROK_HOOK_EVENT is set.
///
/// Grok bare-hook also runs `sync_grok_narrate_from_config`, which writes `AGENTS.md` under
/// the resolved grok dir. Absolute `GROK_HOME` isolates that write (issue #187); without it
/// the child targets the real `~/.grok`.
fn run_no_args_with_input(grok_hook: bool, input: &str) -> BareLaunch {
    let home = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dontspeak"));
    cmd.env("DONTSPEAK_LOG_FILE", home.path().join("dontspeak.log"))
        .env_remove("GROK_HOOK_EVENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let grok_home = if grok_hook {
        let grok_home = home.path().join(ds_config::WiredClient::Grok.as_str());
        std::fs::create_dir_all(&grok_home).expect("create temp GROK_HOME");
        // Canary outside the managed section survives digests on/off; "OLD" inside must go
        // either way (rewrite or strip) so we prove sync targeted this file.
        let seed = format!(
            "# isolation-canary-r15f01\n\n{}\nOLD\n{}\n",
            ds_config::GROK_NARRATE_BEGIN,
            ds_config::GROK_NARRATE_END
        );
        std::fs::write(grok_home.join("AGENTS.md"), seed).expect("seed temp AGENTS.md");
        cmd.env("GROK_HOOK_EVENT", "stop")
            .env("GROK_HOME", &grok_home);
        Some(grok_home)
    } else {
        None
    };
    let mut child = cmd.spawn().expect("spawn dontspeak");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("write child stdin");
    // Close stdin so MCP and hook both see EOF after the one document.
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for dontspeak");
    BareLaunch {
        output,
        grok_home,
        _home: home,
    }
}

#[test]
fn unknown_subcommand_exits_fast_not_hangs() {
    // Exit 2, quickly. Pre-fix fell through to mcp::serve() and blocked on stdin.
    assert_eq!(
        run_bounded(&["definitely-not-a-subcommand"], Duration::from_secs(10)),
        2
    );
    assert_eq!(run_bounded(&["typo"], Duration::from_secs(10)), 2);
}

#[test]
fn recognized_subcommand_still_dispatches() {
    // Leftover-arg check must not shadow real subcommands: wire --help → 0.
    assert_eq!(run_bounded(&["wire", "--help"], Duration::from_secs(10)), 0);
}

#[test]
fn version_help_and_status_probes_exit_zero_not_error() {
    // Host/shell probes must exit 0, not unknown-subcommand 2.
    for args in [
        &["--version"][..],
        &["-V"],
        &["version"],
        &["--help"],
        &["-h"],
        &["help"],
        &["status"],
    ] {
        assert_eq!(
            run_bounded(args, Duration::from_secs(10)),
            0,
            "probe {:?} must exit 0",
            args
        );
    }
}

#[test]
fn grok_marker_routes_bare_launch_to_hook_while_unmarked_bare_launch_stays_mcp() {
    let ping = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";

    let mcp = run_no_args_with_input(false, ping);
    assert!(
        mcp.output.status.success(),
        "MCP launch failed: {:?}",
        mcp.output
    );
    let reply: serde_json::Value = serde_json::from_slice(&mcp.output.stdout).unwrap_or_else(|e| {
        panic!(
            "unmarked no-arg launch did not return MCP JSON ({e}): {:?}",
            String::from_utf8_lossy(&mcp.output.stdout)
        )
    });
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"], serde_json::json!({}));

    let hook = run_no_args_with_input(true, ping);
    assert!(
        hook.output.status.success(),
        "Grok hook launch failed: {:?}",
        hook.output
    );
    assert!(
        hook.output.stdout.is_empty(),
        "unknown Grok hook event must not emit an MCP reply: {:?}",
        String::from_utf8_lossy(&hook.output.stdout)
    );

    // Issue #187: AGENTS.md sync must hit temp GROK_HOME, not the real ~/.grok.
    let grok_home = hook.grok_home.expect("Grok arm sets GROK_HOME");
    let agents = std::fs::read_to_string(grok_home.join("AGENTS.md"))
        .expect("temp GROK_HOME AGENTS.md after bare hook");
    assert!(
        agents.contains("isolation-canary-r15f01"),
        "user content outside managed section must survive: {agents:?}"
    );
    assert!(
        !agents.contains("OLD"),
        "managed section must have been rewritten or stripped under GROK_HOME: {agents:?}"
    );
}
