//! CLI front-door dispatch: an UNRECOGNIZED first argument must FAIL FAST (exit 2), never
//! fall through to the stdio MCP server — that mode blocks on stdin forever. Regression test
//! for the hang where `dontspeak <typo>` (or an OLD binary handed the newer `wire` subcommand)
//! silently became the stdin-blocking server instead of erroring.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Spawn the built `dontspeak` binary with `args` (stdin closed), waiting up to `timeout`.
/// Returns its exit code, or PANICS if it does not exit in time — that panic IS the hang
/// regression (the server never returns). stdin is `null` so a regressed build that falls
/// into `mcp::serve()` reads EOF and exits (code != 2), which the assertions still catch.
fn run_bounded(args: &[&str], timeout: Duration) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dontspeak"))
        .args(args)
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
