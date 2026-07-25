//! Model-path fixtures via child spawn — not in-process env mutation.
//!
//! Gates under test need `DONTSPEAK_MODEL_DIR` (and friends) on a tempdir. Edition 2024
//! made `set_var` unsafe: `setenv` can replace `environ` while concurrent `getenv`
//! (every `tempfile::tempdir()` reads `TMPDIR`) races (#216). `Command::env` mutates only
//! the child. Parent builds fixture + re-exec by name; child asserts under the env.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Names the phase a child should run; absent in an ordinary parent run.
const PHASE_VAR: &str = "DS_TEST_MODEL_ENV_PHASE";
/// Where the child records that it really ran.
const RAN_VAR: &str = "DS_TEST_MODEL_ENV_RAN";

/// Child model env. `ort_dylib: None` clears ORT; SYS/MLX/FLUID dylib paths always cleared.
pub(crate) struct ChildEnv<'a> {
    pub(crate) phase: &'a str,
    pub(crate) model_dir: &'a Path,
    pub(crate) ort_dylib: Option<&'a Path>,
}

/// Re-exec `test_path` (`module::tests::name`) under `env`. Panics if child didn't run/pass.
pub(crate) fn run_child(test_path: &str, env: ChildEnv<'_>) {
    let sentinel_dir = tempfile::tempdir().expect("sentinel dir");
    let sentinel = sentinel_dir.path().join("child-ran");
    let mut command = Command::new(std::env::current_exe().expect("test binary path"));
    command
        .arg("--exact")
        .arg(test_path)
        .arg("--nocapture")
        .env(PHASE_VAR, env.phase)
        .env(RAN_VAR, &sentinel)
        .env("DONTSPEAK_MODEL_DIR", env.model_dir)
        .env_remove("DONTSPEAK_SYS_DYLIB_PATH")
        .env_remove("DONTSPEAK_MLX_DYLIB_PATH")
        .env_remove("DONTSPEAK_FLUID_DYLIB_PATH");
    match env.ort_dylib {
        Some(path) => command.env("ORT_DYLIB_PATH", path),
        None => command.env_remove("ORT_DYLIB_PATH"),
    };
    let status = command.status().expect("spawn the child test run");
    // Empty filter exits 0 with nothing run — sentinel separates pass from no-match.
    assert!(
        sentinel.is_file(),
        "no test matched `{test_path}` — the child ran nothing, so the path is stale"
    );
    assert!(
        status.success(),
        "child run of `{test_path}` (phase `{}`) failed: {status}",
        env.phase
    );
}

/// `Some` in a child started by [`run_child`], `None` in the ordinary parent run.
pub(crate) fn child_run() -> Option<ChildRun> {
    let phase = std::env::var(PHASE_VAR).ok()?;
    let sentinel = std::env::var_os(RAN_VAR)
        .map(PathBuf::from)
        .expect("child run started without a sentinel path");
    Some(ChildRun { phase, sentinel })
}

/// Sentinel that the `--exact` filter matched. Drop records the run (also on panic).
pub(crate) struct ChildRun {
    phase: String,
    sentinel: PathBuf,
}

impl ChildRun {
    pub(crate) fn phase(&self) -> &str {
        &self.phase
    }
}

impl Drop for ChildRun {
    fn drop(&mut self) {
        // Don't panic in Drop during a test panic (would abort over the real failure).
        let _ = std::fs::write(&self.sentinel, b"ran");
    }
}
