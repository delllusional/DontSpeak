//! Model-path fixtures for tests, delivered by process spawn instead of environment
//! mutation.
//!
//! The gates under test resolve through `ds_config::model_dir()` and the ORT/MLX dylib
//! paths, so a test needs `DONTSPEAK_MODEL_DIR` and friends pointed at a tempdir. Setting
//! them in-process is unsound here: edition 2024 made `set_var` unsafe because `setenv`
//! can replace the whole `environ` array, so ANY concurrent `getenv` — every
//! `tempfile::tempdir()` in this binary reads `TMPDIR` — can read freed memory. A mutex
//! shared by the writers never fixed that, because the readers never took it (#216).
//!
//! `Command::env` writes the child's environment block before it starts, so the fixture
//! costs this process no mutation at all. Each converted test keeps one body: the parent
//! run builds the fixture and re-executes the same test by name, the child run sees the
//! environment and asserts.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Names the phase a child should run; absent in an ordinary parent run.
const PHASE_VAR: &str = "DS_TEST_MODEL_ENV_PHASE";
/// Where the child records that it really ran.
const RAN_VAR: &str = "DS_TEST_MODEL_ENV_RAN";

/// The model environment one child run should see. `ort_dylib: None` clears the variable;
/// all three `DONTSPEAK_{SYS,MLX,FLUID}_DYLIB_PATH` variables are always cleared, so a host
/// with real shim dylibs installed takes the same branch as CI.
pub(crate) struct ChildEnv<'a> {
    pub(crate) phase: &'a str,
    pub(crate) model_dir: &'a Path,
    pub(crate) ort_dylib: Option<&'a Path>,
}

/// Re-runs `test_path` — the full `module::tests::name` path libtest prints — in a child
/// process holding `env`. Panics unless that child both ran and passed.
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
    // Order matters: an empty filter exits 0 with nothing run, so the sentinel is what
    // separates "passed" from "never matched".
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

/// Proof for the parent that a test matched its `--exact` filter. Dropping it records the
/// run — including on panic, where the child's exit status is what reports the failure.
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
        // Panicking in `Drop` during a test panic aborts, which would replace the child's
        // real failure output with a signal.
        let _ = std::fs::write(&self.sentinel, b"ran");
    }
}
