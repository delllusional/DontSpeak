//! Real Windows sharing semantics for the installer's destination lock (#198).
//!
//! NAMING CONSTRAINT — do NOT put `install`/`setup`/`update` in this file's name; see the
//! module doc of `packaging_sync.rs` for why (Windows Installer Detection force-elevates such
//! test binaries, which then can't launch at all).
//!
//! `packaging_sync.rs` can only pin the installer's *text*. This runs the shipped
//! `Enter-DestinationLock` bytes in two real processes, which is the only check that a waiter
//! actually waits — a filter reading the `MethodInvocationException` wrapper's `HResult`
//! instead of the inner `IOException`'s keeps every pinned substring and still aborts
//! instantly. Per-commit CI is Linux-only (`.github/workflows/ci.yml` selects `ubuntu-latest`
//! unless `full-matrix: true`), so this guard runs on the release matrix; treat
//! `Enter-DestinationLock` as effectively unpinned per-commit when reviewing changes to it.
#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::thread::sleep;
use std::time::{Duration, Instant};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The shipped lock function plus a driver, so the test exercises installer bytes.
fn harness(dir: &Path) -> PathBuf {
    let source = fs::read_to_string(repo_root().join("scripts/install/web/install.ps1"))
        .expect("read install.ps1")
        .replace("\r\n", "\n");
    let begin = source
        .find("# --- BEGIN destination lock ---")
        .expect("install.ps1 has no destination-lock BEGIN marker");
    let end = source
        .find("# --- END destination lock ---")
        .expect("install.ps1 has no destination-lock END marker");
    assert!(end > begin, "destination-lock markers are reversed");
    let block = &source[begin..end];

    let path = dir.join("lock-harness.ps1");
    fs::write(
        &path,
        format!(
            r#"param(
  [Parameter(Mandatory = $true)][string]$Destination,
  [string]$Mode = 'enter',
  [string]$Ready,
  [string]$Release
)
$ErrorActionPreference = 'Stop'

{block}

$lock = Enter-DestinationLock -Destination $Destination
if ($Mode -eq 'hold') {{
  New-Item -ItemType File -Path $Ready -Force | Out-Null
  while (-not (Test-Path -LiteralPath $Release)) {{ Start-Sleep -Milliseconds 100 }}
}} else {{
  Write-Host 'entered'
}}
$lock.Dispose()
"#
        ),
    )
    .expect("write lock harness");
    path
}

fn powershell(harness: &Path, destination: &Path, wait_seconds: &str, extra: &[&str]) -> Command {
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(harness)
        .arg("-Destination")
        .arg(destination)
        .args(extra)
        .env("DONTSPEAK_INSTALL_LOCK_WAIT", wait_seconds);
    command
}

fn wait_for(path: &Path, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        sleep(Duration::from_millis(50));
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn destination_lock_serializes_separate_installer_processes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = harness(dir.path());
    // Inside the tempdir, never %LOCALAPPDATA%\Programs\DontSpeak: the lock file is
    // deliberately never deleted, so a real destination would plant a permanent artifact in
    // the runner's actual install location. The parent exists already — Enter-DestinationLock
    // does not create it, and a missing one is a DirectoryNotFoundException it rethrows.
    let destination = dir.path().join("DontSpeak");
    let lock = dir.path().join(".DontSpeak.ds-install.lock");
    let ready = dir.path().join("ready");
    let release = dir.path().join("release");

    let mut holder: Child = powershell(&script, &destination, "30", &[])
        .args(["-Mode", "hold", "-Ready"])
        .arg(&ready)
        .arg("-Release")
        .arg(&release)
        .spawn()
        .expect("spawn the holding installer");
    wait_for(&ready, "the holder to take the lock");

    let blocked = powershell(&script, &destination, "1", &[])
        .output()
        .expect("run the blocked installer");
    assert!(
        !blocked.status.success(),
        "a second installer entered while the lock was held: {}",
        text(&blocked)
    );
    // NOT merely "exits non-zero": reading the wrapper's HResult also fails, but with the raw
    // "used by another process" message and no wait at all. This string is what proves the
    // waiter waited and then failed closed on its own deadline.
    assert!(
        text(&blocked).contains("still finalizing"),
        "blocked installer did not report a concurrent installer: {}",
        text(&blocked)
    );

    fs::write(&release, "").expect("signal release");
    let held = holder.wait().expect("wait for the holding installer");
    assert!(held.success(), "the holding installer failed");

    let entered = powershell(&script, &destination, "30", &[])
        .output()
        .expect("run the installer after release");
    assert!(
        entered.status.success() && text(&entered).contains("entered"),
        "the lock was not released: {}",
        text(&entered)
    );

    assert!(
        lock.exists(),
        "the Windows lock file must survive release — a waiter may still hold a handle to it"
    );
}
