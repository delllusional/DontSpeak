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
//! Uninstall logic lives only in `scripts/install/bundle/uninstall.sh` (macOS/Linux) and
//! `scripts/install/bundle/uninstall.ps1` (Windows). Platform packages copy those canonical files as
//! payloads; installers require and place/register the payload instead of embedding a
//! second script body. These tests pin that three-platform route and the install-side
//! invariant that macOS has ONE per-user install layout
//! (`~/Applications/DontSpeak.app`, shared by the release and dev flows) — never the
//! system `/Applications` folder, which needs an admin account.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// Reads a repo file with line endings normalized to LF.
fn repo_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .replace("\r\n", "\n")
}

#[test]
fn repo_uninstall_entry_points_exec_the_canonical_script() {
    let body = repo_file("apps/linux/uninstall.sh");
    // Require BOTH substrings on the SAME line, with `exec` as that line's first token —
    // i.e. `exec` must actually prefix the canonical-script invocation. Checking the two
    // substrings independently (old behavior) would still pass if the script ran
    // scripts/install/bundle/uninstall.sh as a plain subprocess (changing exit-code/signal semantics)
    // and `exec`'d something unrelated elsewhere as a fallback.
    let execs_canonical_script = body.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("exec ")
            && trimmed.contains("/../..\" && pwd)/scripts/install/bundle/uninstall.sh\" \"$@\"")
    });
    assert!(
        execs_canonical_script,
        "apps/linux/uninstall.sh must stay a thin wrapper whose `exec` line directly \
         invokes …scripts/install/bundle/uninstall.sh \"$@\" (found no line starting with `exec` that \
         also contains that invocation) — put logic in scripts/install/bundle/uninstall.sh only"
    );
}

#[test]
fn uninstaller_removes_the_macos_app_bundle() {
    let canon = repo_file("scripts/install/bundle/uninstall.sh");
    assert!(
        canon.contains(r#"rm -rf "$H/Applications/DontSpeak.app""#),
        "scripts/install/bundle/uninstall.sh must remove the per-user app bundle \
         (~/Applications/DontSpeak.app) — the ONE macOS install layout"
    );
}

#[test]
fn every_platform_package_ships_its_canonical_uninstaller_file() {
    let routes = [
        (
            "Windows",
            "apps/windows/installer/build-portable.ps1",
            r#"Copy-Item "$repo\scripts\install\bundle\uninstall.ps1" "$stage\uninstall.ps1""#,
        ),
        (
            "macOS",
            "apps/macos/bundle-lib.sh",
            r#"install -m0755 "$repo/scripts/install/bundle/uninstall.sh" "$app/Contents/Resources/uninstall.sh""#,
        ),
        (
            "Linux",
            "apps/linux/package.sh",
            r#"install -m0755 "$REPO/scripts/install/bundle/uninstall.sh" "$ROOT/uninstall.sh""#,
        ),
    ];
    for (platform, packager, canonical_copy) in routes {
        assert!(
            repo_file(packager).contains(canonical_copy),
            "{platform} package must copy its canonical uninstaller file as payload ({packager})"
        );
    }
}

#[test]
fn every_platform_installer_requires_the_packaged_uninstaller() {
    let checks = [
        (
            "Windows",
            "scripts/install/web/install.ps1",
            r#"Test-Path -LiteralPath (Join-Path $stagedApp 'uninstall.ps1') -PathType Leaf"#,
            "incomplete archive: missing canonical uninstall.ps1 payload",
        ),
        (
            "macOS",
            "scripts/install/web/install.sh",
            r#"[ -f "$out/DontSpeak.app/Contents/Resources/uninstall.sh" ]"#,
            "incomplete archive (no canonical uninstall.sh payload)",
        ),
        (
            "Linux",
            "apps/linux/tarball-install.sh",
            r#"[ -f "$HERE/uninstall.sh" ]"#,
            "install: package is missing uninstall.sh",
        ),
    ];
    for (platform, installer, payload_check, missing_payload_error) in checks {
        let body = repo_file(installer);
        assert!(
            body.contains(payload_check) && body.contains(missing_payload_error),
            "{platform} installer must reject an artifact missing its canonical uninstaller ({installer})"
        );
    }
}

#[test]
fn web_installers_do_not_embed_uninstaller_script_bodies() {
    for (installer, old_embed_delimiter) in [
        ("scripts/install/web/install.sh", "<<'UNINSTALL'"),
        ("scripts/install/web/install.ps1", "@'\n# uninstall.ps1"),
    ] {
        let body = repo_file(installer);
        assert!(
            !body.contains(old_embed_delimiter),
            "{installer} contains its old embedded-uninstaller delimiter"
        );
        for canonical_header in [
            "# uninstall.sh — THE DontSpeak uninstaller",
            "# uninstall.ps1 — THE DontSpeak uninstaller",
        ] {
            assert!(
                !body.contains(canonical_header),
                "{installer} embeds an uninstaller body; packages must copy the canonical file instead"
            );
        }
    }
}

#[test]
fn release_publishes_fixed_name_web_installers() {
    let workflow = repo_file(".github/workflows/release.yml");
    for needle in [
        "actions/checkout@v7",
        "scripts/install/web/install.sh",
        "scripts/install/web/install.ps1",
        r#""${FILES[@]}" "${INSTALLERS[@]}" checksums.txt"#,
    ] {
        assert!(
            workflow.contains(needle),
            "release workflow must publish fixed-name web installers from the tagged commit ({needle})"
        );
    }
}

#[test]
fn downloaded_installers_require_published_checksums() {
    let unix = repo_file("scripts/install/web/install.sh");
    for required in [
        "release is missing checksums.txt",
        "is not listed in checksums.txt",
        "need sha256sum or shasum",
    ] {
        assert!(
            unix.contains(required),
            "Unix web installer must fail closed when checksum verification is unavailable ({required})"
        );
    }
    assert!(
        !unix.contains("skipping integrity check"),
        "Unix web installer must not install an unverified download"
    );

    let windows = repo_file("scripts/install/web/install.ps1");
    for required in [
        "release is missing checksums.txt",
        "is not listed in checksums.txt",
        "Get-FileHash -Algorithm SHA256",
    ] {
        assert!(
            windows.contains(required),
            "Windows web installer must fail closed when checksum verification is unavailable ({required})"
        );
    }
    assert!(
        !windows.contains("skipping integrity check") && !windows.contains("checksum step skipped"),
        "Windows web installer must not install an unverified download"
    );
}

#[test]
fn windows_uninstaller_reports_partial_cleanup() {
    let uninstaller = repo_file("scripts/install/bundle/uninstall.ps1");
    for required in [
        "$ErrorActionPreference = 'Stop'",
        "Invoke-CleanupStep",
        "DontSpeak was only partially removed",
        "exit 1",
    ] {
        assert!(
            uninstaller.contains(required),
            "Windows uninstaller must report rather than hide cleanup failures ({required})"
        );
    }
    assert!(
        !uninstaller.contains("$ErrorActionPreference = 'SilentlyContinue'"),
        "Windows uninstaller must not globally suppress cleanup failures"
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
fn release_bundles_ship_the_legal_notice_and_embedded_data_license() {
    for rel in [
        "licenses/Apache-2.0.txt",
        "licenses/voice-g2p-MIT.txt",
        // Boson §1.b.i(A) requires both agreements with the Higgs-derived decoder.
        "licenses/Boson-Higgs-Audio-2-Community-License.txt",
        "licenses/Meta-Llama-3-Community-License.txt",
    ] {
        assert!(
            repo_root().join(rel).is_file(),
            "missing legal payload {rel}"
        );
    }

    let windows = repo_file("apps/windows/installer/build-portable.ps1");
    for needle in ["NOTICE.md", "LICENSE", r#"Copy-Item "$repo\licenses\*""#] {
        assert!(
            windows.contains(needle),
            "Windows portable builder must ship {needle}"
        );
    }

    let linux = repo_file("apps/linux/package.sh");
    for needle in [
        "NOTICE.md",
        "LICENSE",
        "licenses/Apache-2.0.txt",
        "licenses/voice-g2p-MIT.txt",
        "licenses/Boson-Higgs-Audio-2-Community-License.txt",
        "licenses/Meta-Llama-3-Community-License.txt",
    ] {
        assert!(
            linux.contains(needle),
            "Linux tarball builder must ship {needle}"
        );
    }

    let macos = repo_file("apps/macos/bundle-lib.sh");
    for needle in ["NOTICE.md", "LICENSE", r#"cp "$repo/licenses/"*"#] {
        assert!(
            macos.contains(needle),
            "macOS app assembler must ship {needle}"
        );
    }
}

#[test]
fn uninstaller_covers_the_linux_residue() {
    let canon = repo_file("scripts/install/bundle/uninstall.sh");
    for needle in [
        "icons/hicolor/scalable/apps/dontspeak.svg", // menu icon (dev + tarball installs)
        "pipewire.conf.d/99-ds-aec.conf",            // --aec echo-cancel drop-in
    ] {
        assert!(
            canon.contains(needle),
            "scripts/install/bundle/uninstall.sh no longer removes {needle} — a Linux install leaves it behind"
        );
    }
}

#[test]
fn uninstaller_honors_the_app_dir_override() {
    assert!(
        repo_file("scripts/install/bundle/uninstall.sh").contains("DONTSPEAK_APP_DIR"),
        "scripts/install/bundle/uninstall.sh must honor DONTSPEAK_APP_DIR — bundle.sh installs there, so a \
         custom-dir bundle would otherwise never be removed"
    );
}

#[test]
fn dev_installs_place_the_standalone_uninstaller() {
    // macOS/Linux share install-engine.sh; Windows runs the exact public installer against
    // its local archive instead of recreating registration as skill documentation.
    assert!(
        repo_file("scripts/install/local/install-engine.sh")
            .contains("scripts/install/bundle/uninstall.sh\" \"$INSTALL_DIR/dontspeak-uninstall\""),
        "scripts/install/local/install-engine.sh must place scripts/install/bundle/uninstall.sh as $INSTALL_DIR/dontspeak-uninstall"
    );
    let windows_skill = repo_file(".agents/skills/build-windows/SKILL.md");
    for needle in [
        "DONTSPEAK_ARCHIVE",
        r#"& .\scripts\install\web\install.ps1"#,
    ] {
        assert!(
            windows_skill.contains(needle),
            "Windows dev install must route its local archive through scripts/install/web/install.ps1 ({needle})"
        );
    }
    assert!(
        !windows_skill.contains("Expand-Archive"),
        "Windows dev skill must not reimplement archive installation outside scripts/install/web/install.ps1"
    );
}

#[test]
fn macos_installs_share_the_single_per_user_layout() {
    // Both flows lay down ~/Applications/DontSpeak.app — the ONE install location.
    let web = repo_file("scripts/install/web/install.sh");
    assert!(
        web.contains(r#"APP="$HOME/Applications/DontSpeak.app""#),
        "scripts/install/web/install.sh must install into ~/Applications/DontSpeak.app — the single \
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
        "scripts/install/web/install.sh",
        "apps/macos/bundle.sh",
        "scripts/install/bundle/uninstall.sh",
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

/// The POSIX destination lock ships duplicated: `install.sh` arrives through `curl | sh` and
/// `tarball-install.sh` inside the tarball, so neither can source a shared file. Pin the two
/// copies equal — a fix applied to one only would leave the other platform racing.
#[test]
fn posix_installers_share_one_destination_lock_block() {
    fn block(rel: &str) -> String {
        let body = repo_file(rel);
        let begin = body
            .find("# ── BEGIN destination lock")
            .unwrap_or_else(|| panic!("{rel} has no destination-lock BEGIN marker"));
        let end = body
            .find("# ── END destination lock")
            .unwrap_or_else(|| panic!("{rel} has no destination-lock END marker"));
        assert!(
            end > begin,
            "{rel} has its destination-lock markers reversed"
        );
        body[begin..end].to_string()
    }
    let web = block("scripts/install/web/install.sh");
    let tarball = block("apps/linux/tarball-install.sh");
    assert!(!web.is_empty(), "the destination-lock block is empty");
    assert_eq!(
        web, tarball,
        "scripts/install/web/install.sh and apps/linux/tarball-install.sh carry different \
         destination-lock blocks — they must stay byte-identical"
    );
}

/// Every platform must take its destination lock BEFORE the first destructive step, and give
/// it back. An acquire that drifts below the stop/replace steps serializes nothing.
#[test]
fn installers_lock_before_replacing_the_destination() {
    fn precedes(body: &str, first: &str, second: &str, what: &str) {
        let lock = body
            .find(first)
            .unwrap_or_else(|| panic!("{what}: no `{first}`"));
        let destructive = body
            .find(second)
            .unwrap_or_else(|| panic!("{what}: no `{second}`"));
        assert!(
            lock < destructive,
            "{what}: `{first}` must come before `{second}` or the destructive step runs unserialized"
        );
    }

    let macos = repo_file("scripts/install/web/install.sh");
    precedes(
        &macos,
        "ds_lock_acquire \"$APP\"",
        "osascript -e 'quit app",
        "macOS",
    );
    precedes(
        &macos,
        "ds_lock_acquire \"$APP\"",
        "rm -rf \"$APP\"",
        "macOS",
    );
    assert!(
        macos.contains("cleanup() { ds_lock_release;"),
        "macOS: cleanup() must release the destination lock first"
    );

    let linux = repo_file("apps/linux/tarball-install.sh");
    precedes(
        &linux,
        "ds_lock_acquire \"$BIN/dontspeak\"",
        "pkill -x ds-gtk",
        "Linux",
    );
    precedes(
        &linux,
        "ds_lock_acquire \"$BIN/dontspeak\"",
        "install -m0755 \"$HERE\"/bin/*",
        "Linux",
    );
    assert!(
        linux.contains("trap ds_lock_release EXIT"),
        "Linux: the bundled installer must release the destination lock on exit"
    );

    // The call site, not the function definition — `Enter-DestinationLock` alone also matches
    // the definition above it, so that pin would survive deleting the call.
    let windows = repo_file("scripts/install/web/install.ps1");
    precedes(
        &windows,
        "Enter-DestinationLock -Destination $dest",
        "$installed = @(Get-Process",
        "Windows",
    );
    precedes(
        &windows,
        "Enter-DestinationLock -Destination $dest",
        "Remove-Item $dest",
        "Windows",
    );
    assert!(
        windows.contains("if ($lock) { $lock.Dispose() }"),
        "Windows: the finally block must dispose the destination lock handle"
    );

    // POSIX release DELETES the dir (the Windows file must survive — waiters hold handles to
    // it). A release that only unlocks would leave every later install force-breaking.
    assert!(
        macos.contains("rm -rf \"$DS_LOCK_DIR\" || :"),
        "the POSIX lock block must delete its dir on release"
    );
}

/// A killed installer leaves its lock behind, and Windows never deletes its lock file at all —
/// so full removal is the uninstaller's job.
#[test]
fn uninstallers_remove_the_destination_lock_artifacts() {
    let posix = repo_file("scripts/install/bundle/uninstall.sh");
    for needle in [
        ".DontSpeak.app.ds-install.lock",
        ".DontSpeak.app.ds-install.lock.breaker",
        ".dontspeak.ds-install.lock",
        ".dontspeak.ds-install.lock.breaker",
    ] {
        assert!(
            posix.contains(needle),
            "scripts/install/bundle/uninstall.sh no longer removes {needle} — an interrupted \
             install leaves it behind"
        );
    }
    assert!(
        repo_file("scripts/install/bundle/uninstall.ps1").contains(".ds-install.lock"),
        "scripts/install/bundle/uninstall.ps1 must remove the destination lock file — the \
         installer deliberately never deletes it"
    );
}
