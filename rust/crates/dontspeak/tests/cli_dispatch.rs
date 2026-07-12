//! CLI front-door dispatch: an UNRECOGNIZED first argument must FAIL FAST (exit 2), never
//! fall through to the stdio MCP server — that mode blocks on stdin forever. Regression test
//! for the hang where `dontspeak <typo>` (or an OLD binary handed the newer `wire` subcommand)
//! silently became the stdin-blocking server instead of erroring.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Spawn the built `dontspeak` binary with `args` (stdin closed), waiting up to `timeout`.
/// Returns its exit code, or PANICS if it does not exit in time — that panic IS the hang
/// regression (the server never returns). stdin is `null` so a regressed build that falls
/// into `mcp::serve()` reads EOF and exits (code != 2), which the assertions still catch.
///
/// This is a REAL subprocess running the production `main()`, which unconditionally calls
/// `ds_log::init()` and logs the "unknown subcommand" error — unlike in-process unit tests,
/// there is no seam to hand it a tempdir `log_file`, so it falls through to `log_cached`'s
/// real per-OS path (see issue #26). `DONTSPEAK_LOG_FILE` redirects that path straight into a
/// tempdir for the child; a `HOME`/`LOCALAPPDATA` env override doesn't work cross-platform —
/// Windows resolves `%LOCALAPPDATA%` via the native known-folder API, which ignores an
/// overridden env var for a child process.
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

/// Run the no-argument binary with one stdin document. This pins the environment-only Grok
/// hook discriminator without touching a real engine: the same JSON-RPC ping is an MCP reply
/// in normal mode and a no-op unknown hook event when Grok's reserved marker is present.
fn run_no_args_with_input(grok_hook: bool, input: &str) -> Output {
    let home = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_dontspeak"));
    cmd.env("DONTSPEAK_LOG_FILE", home.path().join("dontspeak.log"))
        .env_remove("GROK_HOOK_EVENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if grok_hook {
        cmd.env("GROK_HOOK_EVENT", "stop");
    }
    let mut child = cmd.spawn().expect("spawn dontspeak");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input.as_bytes())
        .expect("write child stdin");
    child.wait_with_output().expect("wait for dontspeak")
}

#[test]
fn unknown_subcommand_exits_fast_not_hangs() {
    // Exit 2, quickly. The pre-fix bug fell through to `mcp::serve()` and blocked on stdin.
    assert_eq!(
        run_bounded(&["definitely-not-a-subcommand"], Duration::from_secs(10)),
        2
    );
    assert_eq!(run_bounded(&["typo"], Duration::from_secs(10)), 2);
}

#[test]
fn recognized_subcommand_still_dispatches() {
    // Guard that the new leftover-argument check didn't shadow the real subcommands:
    // `wire --help` prints usage and exits 0.
    assert_eq!(run_bounded(&["wire", "--help"], Duration::from_secs(10)), 0);
}

#[test]
fn grok_marker_routes_bare_launch_to_hook_while_unmarked_bare_launch_stays_mcp() {
    let ping = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";

    let mcp = run_no_args_with_input(false, ping);
    assert!(mcp.status.success(), "MCP launch failed: {mcp:?}");
    let reply: serde_json::Value = serde_json::from_slice(&mcp.stdout).unwrap_or_else(|e| {
        panic!(
            "unmarked no-arg launch did not return MCP JSON ({e}): {:?}",
            String::from_utf8_lossy(&mcp.stdout)
        )
    });
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"], serde_json::json!({}));

    let hook = run_no_args_with_input(true, ping);
    assert!(hook.status.success(), "Grok hook launch failed: {hook:?}");
    assert!(
        hook.stdout.is_empty(),
        "unknown Grok hook event must not emit an MCP reply: {:?}",
        String::from_utf8_lossy(&hook.stdout)
    );
}
