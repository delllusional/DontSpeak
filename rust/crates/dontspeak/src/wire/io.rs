//! Shared IO core for the wire writers — the three flows every mechanism was repeating
//! verbatim before this module existed: resolving the deployed `dontspeak` binary
//! ([`resolve_dontspeak_bin`]), the lenient-but-never-clobbering JSON read
//! ([`read_json_or_bail`]), and the backup → atomic-write → report tail
//! ([`backup_then_write`]). The writers in [`hooks`](super::hooks) and [`mcp`](super::mcp)
//! compose these; a new mechanism writer starts from here instead of copy-pasting them a
//! fourth time.

use ds_config::Paths;
use serde_json::Value;
use std::path::Path;

/// Resolve the deployed `dontspeak` binary — the ONE path hook commands and the MCP
/// `command` field register.
///
/// Prefer the stable install location (`~/.local/bin/dontspeak`) so wired configs survive
/// rebuilds at the same path — this also lets a dev build run from `target/` wire configs at
/// the DEPLOYED binary, not the build dir. NOT on Windows: the portable zip extracts the
/// binary beside THIS exe, so a stale dev-deploy copy in `~/.local/bin` must not SHADOW the
/// installed one. Fall back to this exe's own directory (the package lays the binaries down
/// together, so the sibling path is correct even before it exists).
///
/// Real-env entry point used by production callers (`hooks::claude_json_hooks`/
/// `claude_toml_hooks` via their own `paths: &Paths` param, and `mcp.rs`'s own call which
/// stays unparameterized — out of scope here). Resolves `Paths` fresh from the real `$HOME`.
pub(crate) fn resolve_dontspeak_bin() -> Option<String> {
    resolve_dontspeak_bin_at(Paths::resolve().as_ref())
}

/// Injectable core: same resolution, but a caller (a test) can pass a tempdir-rooted `Paths`
/// instead of ever touching the real `$HOME`/`BaseDirs`. `None` (no `$HOME`, or the caller
/// deliberately withholds it) skips the unix stable-install-path check and falls straight to
/// the sibling-of-this-exe fallback.
pub(crate) fn resolve_dontspeak_bin_at(paths: Option<&Paths>) -> Option<String> {
    let file = format!("dontspeak{}", std::env::consts::EXE_SUFFIX);
    #[cfg(unix)]
    if let Some(p) = paths {
        let cand = p.home.join(".local/bin").join(&file);
        if cand.exists() {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    #[cfg(not(unix))]
    let _ = paths; // unused on non-unix builds (Windows) — avoid an unused-variable warning
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join(&file).to_string_lossy().into_owned())
}

/// Read a client's JSON config for editing. Missing or empty → `Value::Null` (treated as
/// `{}` by every shaper). A present but MALFORMED file is the CLIENT's own config — report
/// and `Err(())` so the caller bails with exit 1, leaving the file recoverable rather than
/// clobbered.
pub(crate) fn read_json_or_bail(tool: &str, cfg: &Path) -> Result<Value, ()> {
    match std::fs::read_to_string(cfg) {
        Err(_) => Ok(Value::Null),
        Ok(s) if s.trim().is_empty() => Ok(Value::Null),
        Ok(s) => serde_json::from_str::<Value>(&s).map_err(|_| {
            eprintln!(
                "{tool}: existing {} is not valid JSON; leaving it unchanged",
                cfg.display()
            );
        }),
    }
}

/// What [`backup_then_write`] puts on disk — pretty JSON or a pre-rendered string (the
/// format-preserving TOML the `toml_edit` shaper produced).
pub(crate) enum WriteBody<'a> {
    Json(&'a Value),
    Str(&'a str),
}

/// The write tail every mechanism shares: best-effort timestamped backup (a copy failure is
/// surfaced, not swallowed — the user is warned the overwrite has no recoverable copy, rather
/// than the write being silently blocked), then the atomic write, then the report line
/// `"{tool}: {action} {file}"` — `action` is the caller's composed verb phrase (e.g.
/// `"wired DontSpeak hooks ->"`). Returns the process exit code (0 ok, 1 write failure).
pub(crate) fn backup_then_write(
    tool: &str,
    cfg: &Path,
    ext: &str,
    body: &WriteBody,
    action: &str,
) -> i32 {
    if let Err(e) = ds_config::backup_before_write(cfg, ext) {
        eprintln!(
            "{tool}: WARNING: could not back up {} before writing ({e}); proceeding without a backup",
            cfg.display()
        );
    }
    let written = match body {
        WriteBody::Json(v) => ds_config::atomic_write_json(cfg, v),
        WriteBody::Str(s) => ds_config::atomic_write_str(cfg, s),
    };
    match written {
        Ok(()) => {
            eprintln!("{tool}: {action} {}", cfg.display());
            0
        }
        Err(e) => {
            eprintln!("{tool}: write failed: {e}");
            1
        }
    }
}
