//! Executable form of the rule that closed #216: nothing in this crate mutates the process
//! environment.
//!
//! Five tests used to point `DONTSPEAK_MODEL_DIR` and the dylib paths at tempdirs behind a
//! shared mutex. The mutex only ever covered the writers — every other test in the same
//! binary calls `getenv` (a `tempfile::tempdir()` reads `TMPDIR`) with nothing held, which
//! is exactly the overlap edition 2024 made `set_var` unsafe for. They now take their
//! fixture from a child process spawned with an explicit environment (`src/test_env.rs`),
//! so a writer coming back is the regression to catch here.

use std::path::{Path, PathBuf};

#[test]
fn crate_sources_never_mutate_the_process_environment() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut writers: Vec<String> = Vec::new();
    collect_env_writers(&src, &src, &mut writers);
    writers.sort_unstable();
    let empty: [String; 0] = [];
    assert_eq!(
        writers, empty,
        "dontspeakd/src mutates the process environment — a test that needs a model dir \
         takes one from `test_env::run_child` instead (#216)"
    );
}

/// Paths under `dir`, relative to `root`, that call `env::set_var`/`env::remove_var`.
fn collect_env_writers(root: &Path, dir: &Path, writers: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", dir.display()));
    for entry in entries {
        let path: PathBuf = entry.expect("readable directory entry").path();
        if path.is_dir() {
            collect_env_writers(root, &path, writers);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        if source.contains("env::set_var(") || source.contains("env::remove_var(") {
            writers.push(relative.into_owned());
        }
    }
}
