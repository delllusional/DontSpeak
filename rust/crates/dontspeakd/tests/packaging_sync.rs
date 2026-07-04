//! Pins the install/uninstall surface in sync across its copies.
//!
//! NAMING CONSTRAINT — do NOT put `install`/`setup`/`update` in this file's name.
//! Windows' "Installer Detection Technology" force-elevates any unsigned executable whose
//! filename contains one of those words, so `cargo test`'s (unelevated) integration-test
//! binary can't even be launched — it dies with `os error 740 (requires elevation)` BEFORE
//! running a single assertion, failing `cargo test --workspace` on every Windows dev box.
//! This file was `installer_sync.rs` and hit exactly that; hence `packaging_sync.rs`.
//! (`cli_dispatch.rs` runs fine, proving `patch`/`dispatch` is NOT a trigger — only those
//! three words are.) The Linux per-commit CI is unaffected; this is a Windows-local trap.
//!
//! The uninstall logic lives ONCE in `scripts/uninstall.sh` and reaches users two ways:
//! repo checkouts run it (`apps/linux/uninstall.sh` execs it), and the one-command
//! installer embeds it verbatim as `~/.local/bin/dontspeak-uninstall` (heredoc in
//! `web/install.sh`). The site serves only the install scripts — every install already
//! carries its own uninstaller. These tests fail CI the moment any copy drifts, and pin
//! the install-side invariant that macOS has ONE per-user install layout
//! (`~/Applications/DontSpeak.app`, shared by the release and dev flows) — never the
//! system `/Applications` folder, which needs an admin account.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Reads a repo file with line endings normalized to LF — Windows checkouts
/// (core.autocrlf) materialize text files with CRLF, which must not fail the
/// byte-for-byte embed comparisons.
fn repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// Like repo_file, but falls back to the git INDEX when the working-tree copy is
/// missing. Endpoint-security tools (Bitdefender) quarantine web/install.ps1 from the
/// working tree on some dev machines — the staged content is still the real content.
fn repo_file_or_staged(rel: &str) -> String {
    let path = repo_root().join(rel);
    if let Ok(s) = fs::read_to_string(&path) {
        return s;
    }
    let out = Command::new("git")
        .args(["show", &format!(":{rel}")])
        .current_dir(repo_root())
        .output()
        .unwrap_or_else(|e| panic!("git show :{rel}: {e}"));
    assert!(
        out.status.success(),
        "{rel}: missing on disk AND not in the git index"
    );
    String::from_utf8(out.stdout)
        .expect("utf8")
        .replace("\r\n", "\n")
}

/// The `<<'UNINSTALL'` heredoc body in web/install.sh.
fn embedded_uninstaller() -> String {
    let install = repo_file("web/install.sh");
    let lines: Vec<&str> = install.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim_end().ends_with("<<'UNINSTALL'"))
        .expect("web/install.sh: no <<'UNINSTALL' heredoc");
    let end = lines[start + 1..]
        .iter()
        .position(|l| *l == "UNINSTALL")
        .map(|i| i + start + 1)
        .expect("web/install.sh: UNINSTALL heredoc never terminated");
    lines[start + 1..end].join("\n") + "\n"
}

#[test]
fn embedded_uninstaller_matches_canonical() {
    assert_eq!(
        embedded_uninstaller(),
        repo_file("scripts/uninstall.sh"),
        "the uninstaller heredoc in web/install.sh has drifted from scripts/uninstall.sh — \
         edit scripts/uninstall.sh (the single source of truth) and re-embed it verbatim"
    );
}

#[test]
fn repo_uninstall_entry_points_exec_the_canonical_script() {
    let body = repo_file("apps/linux/uninstall.sh");
    // Require BOTH substrings on the SAME line, with `exec` as that line's first token —
    // i.e. `exec` must actually prefix the canonical-script invocation. Checking the two
    // substrings independently (old behavior) would still pass if the script ran
    // scripts/uninstall.sh as a plain subprocess (changing exit-code/signal semantics)
    // and `exec`'d something unrelated elsewhere as a fallback.
    let execs_canonical_script = body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("exec ")
            && trimmed.contains("/../..\" && pwd)/scripts/uninstall.sh\" \"$@\"")
    });
    assert!(
        execs_canonical_script,
        "apps/linux/uninstall.sh must stay a thin wrapper whose `exec` line directly \
         invokes …scripts/uninstall.sh \"$@\" (found no line starting with `exec` that \
         also contains that invocation) — put logic in scripts/uninstall.sh only"
    );
}

#[test]
fn uninstaller_removes_the_macos_app_bundle() {
    let canon = repo_file("scripts/uninstall.sh");
    assert!(
        canon.contains(r#"rm -rf "$H/Applications/DontSpeak.app""#),
        "scripts/uninstall.sh must remove the per-user app bundle \
         (~/Applications/DontSpeak.app) — the ONE macOS install layout"
    );
}

#[test]
fn windows_embedded_uninstaller_matches_canonical() {
    // install.ps1 writes the placed uninstaller from a single-quoted here-string
    // (@' … '@). That body must BE scripts/uninstall.ps1, byte-for-byte.
    let install = repo_file_or_staged("web/install.ps1");
    let lines: Vec<&str> = install.lines().collect();
    let start = lines
        .iter()
        .position(|l| l.trim() == "@'")
        .expect("web/install.ps1: no @' here-string");
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with("'@"))
        .map(|i| i + start + 1)
        .expect("web/install.ps1: @' here-string never terminated");
    let embedded = lines[start + 1..end].join("\n") + "\n";
    assert_eq!(
        embedded,
        repo_file("scripts/uninstall.ps1"),
        "the uninstaller here-string in web/install.ps1 has drifted from scripts/uninstall.ps1 — \
         edit scripts/uninstall.ps1 (the single source of truth) and re-embed it verbatim"
    );
}

#[test]
fn linux_tarball_ships_the_installer_file_not_a_heredoc() {
    let package = repo_file("apps/linux/package.sh");
    assert!(
        package.contains("tarball-install.sh") && !package.contains("<<'INSTALL'"),
        "apps/linux/package.sh must ship apps/linux/tarball-install.sh verbatim as the \
         tarball's install.sh — an inlined heredoc copy drifts"
    );
}

#[test]
fn uninstaller_covers_the_linux_residue() {
    let canon = repo_file("scripts/uninstall.sh");
    for needle in [
        "icons/hicolor/scalable/apps/dontspeak.svg", // menu icon (dev + tarball installs)
        "pipewire.conf.d/99-ds-aec.conf",            // --aec echo-cancel drop-in
    ] {
        assert!(
            canon.contains(needle),
            "scripts/uninstall.sh no longer removes {needle} — a Linux install leaves it behind"
        );
    }
}

#[test]
fn uninstaller_honors_the_app_dir_override() {
    assert!(
        repo_file("scripts/uninstall.sh").contains("DONTSPEAK_APP_DIR"),
        "scripts/uninstall.sh must honor DONTSPEAK_APP_DIR — bundle.sh installs there, so a \
         custom-dir bundle would otherwise never be removed"
    );
}

#[test]
fn dev_installs_place_the_standalone_uninstaller() {
    // Parity with the release installer: install-daemon.sh (shared by bundle.sh and
    // scripts/install.sh) copies scripts/uninstall.sh onto PATH as dontspeak-uninstall.
    assert!(
        repo_file("scripts/install-daemon.sh")
            .contains("scripts/uninstall.sh\" \"$INSTALL_DIR/dontspeak-uninstall\""),
        "scripts/install-daemon.sh must place scripts/uninstall.sh as $INSTALL_DIR/dontspeak-uninstall"
    );
}

#[test]
fn macos_installs_share_the_single_per_user_layout() {
    // Both flows lay down ~/Applications/DontSpeak.app — the ONE install location.
    let web = repo_file("web/install.sh");
    assert!(
        web.contains(r#"APP="$HOME/Applications/DontSpeak.app""#),
        "web/install.sh must install into ~/Applications/DontSpeak.app — the single \
         per-user macOS layout (no admin account, and it matches the dev flow)"
    );
    let bundle = repo_file("apps/macos/bundle.sh");
    assert!(
        bundle.contains(r#"APP="${DONTSPEAK_APP_DIR:-$HOME/Applications/DontSpeak.app}""#),
        "apps/macos/bundle.sh must default to ~/Applications/DontSpeak.app — the same \
         layout as the release install"
    );
    // No script may touch the system folder: a `/Applications/DontSpeak.app` reference
    // NOT prefixed by `$HOME`, `$H`, or `~` (the per-user home spellings actually used
    // across these files, including in doc comments) would resurrect the admin-only
    // layout this repo deliberately has exactly one mechanism instead of.
    // Quote characters are stripped before matching so this catches the forbidden path
    // whether it's written double-quoted (`"/Applications/DontSpeak.app"`), single-quoted
    // (`'/Applications/DontSpeak.app'`), or bare/unquoted — not just the one double-quoted
    // spelling the old substring check happened to look for.
    fn strip_quotes(s: &str) -> String {
        s.chars().filter(|c| *c != '"' && *c != '\'').collect()
    }
    for rel in [
        "web/install.sh",
        "apps/macos/bundle.sh",
        "scripts/uninstall.sh",
    ] {
        let normalized = strip_quotes(&repo_file(rel));
        for (at, _) in normalized.match_indices("/Applications/DontSpeak.app") {
            let prefix = &normalized[..at];
            let is_per_user =
                prefix.ends_with("$HOME") || prefix.ends_with("$H") || prefix.ends_with('~');
            assert!(
                is_per_user,
                "{rel} references the system /Applications folder (found \
                 `/Applications/DontSpeak.app` not prefixed by $HOME, $H, or ~, checked \
                 with quote characters stripped so double-quoted/single-quoted/unquoted \
                 forms all get caught) — macOS installs are per-user (~/Applications) ONLY"
            );
        }
    }
}
