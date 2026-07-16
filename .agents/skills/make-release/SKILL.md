---
name: make-release
description: Cut a DontSpeak release — tag the single-source version, push the tag to trigger release.yml on GitHub, MONITOR the ~25-30-min run (it can and does fail), verify the published assets, then deploy the site. Also covers re-cutting a failed release, and cutting/replacing an on-demand DRAFT release of the current `-dev` version for real installable dev binaries without officially shipping. Use when asked to release, cut/publish a version, re-release, cut a dev/preview/draft build, or when a release build failed.
---

# DontSpeak — make a release

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

> A release is **tag-triggered CI**: pushing `v<version>` runs `.github/workflows/release.yml`,
> which gates, builds all platforms, and publishes the GitHub Release with binaries. Treat
> "tag pushed" as the start: monitor the run to completion and verify every asset.

## 1 — Preconditions

- **Version**: the single source is `rust/Cargo.toml` → `[workspace.package] version` (read by
  `scripts/release/version.sh` / `scripts/release/sync-workspace-version.py`). The tag must be
  `v` + exactly that version — the `check` job fails the run fast otherwise. Bump it (+ commit)
  for a new release. Going from a `-dev` suffix to a real release version needs no judgment
  call — the version number was already decided whenever `main` was last bumped (step 9) — it's
  a mechanical strip of the suffix, not a version choice, unless you're deliberately overriding
  the preset bump size (e.g. escalating a preset patch to a minor/major release because more
  accumulated on `main` than expected). **Do the strip + lock sync with the portable script**
  (not a hand edit of four files, and not `cargo generate-lockfile` — see lockfile note below):
  ```bash
  python3 scripts/release/sync-workspace-version.py --strip-dev
  # verify locks still resolve without rewriting registry pins:
  (cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
  (cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
  ```
- **Green main, pushed**: run the `prepush` skill first (clippy + tests — the same suite
  the release re-runs); the tagged commit must be on `origin/main`. **This is a fail-fast
  optimization, not the actual correctness gate** — `release.yml`'s own `tests` job
  (full-matrix) reruns clippy + tests + hygiene regardless, and IS the real gate. Running it
  locally catches a broken `main` before starting the slower release matrix.
- **Hygiene clean — run `cargo fmt` + both all-feature cargo-deny graphs locally before
  tagging.** The
  release (unlike per-commit CI and `prepush`) also gates on rustfmt + rustdoc AND
  cargo-deny (`ci.yml`'s `hygiene` and `cargo-deny` jobs, full-matrix only) — **the
  single most common re-cut cause**. Format both workspaces, verify clean, rebuild docs,
  and clear cargo-deny before tagging:
  ```bash
  (cd rust && cargo fmt) && (cd apps/linux/gtk && cargo fmt)     # apply
  (cd rust && cargo fmt --check) && (cd apps/linux/gtk && cargo fmt --check)   # must be clean
  (cd rust && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked)   # must pass
  cargo deny --manifest-path rust/Cargo.toml --all-features check --config rust/deny.toml
  cargo deny --manifest-path apps/linux/gtk/Cargo.toml --all-features check --config rust/deny.toml
  ```
  Run those cargo-deny commands from the repository root. They exactly match the two
  `ci.yml` release graphs; a default-feature check can miss build-dependency duplicates,
  and checking only `rust/Cargo.toml` misses the standalone GTK lockfile.
  Commit whatever `cargo fmt` (or a `cargo update` for a flagged advisory) changed — a
  release is where the codebase gets cleaned up.
- **macOS Swift tests (run on a Mac).** The release matrix runs `swift test` on the macos-26
  leg, but neither `prepush` nor the gate above covers it — a Swift-logic regression would
  otherwise surface only ~30 min into the release. Run it here first (builds the FFI staticlib
  the SwiftPM package force-loads, then tests):
  ```bash
  (cd rust && cargo build --profile release-ffi --locked -p ds-core)
  (cd apps/macos && MACOSX_DEPLOYMENT_TARGET=14.0 swift test)   # must pass, 0 failures
  ```
  The release matrix also runs the WinUI xunit tests on Windows.
- **`--locked` catches Cargo.lock drift — in BOTH workspaces.** The prepush gates are locked;
  formatting is not. A marketing-version bump changes every workspace crate's version string in
  `rust/Cargo.lock` **and** `apps/linux/gtk/Cargo.lock` (the GTK host is a SEPARATE workspace
  that path-depends on the shared crates). **Do not run `cargo generate-lockfile` for a version
  bump.** That command re-resolves every registry dependency to the latest compatible version and
  has produced ~300-line unrelated lock churn on Windows release cuts; path packages have no
  registry checksum tied to the marketing version — only their `version =` field must match
  `Cargo.toml`. `scripts/release/sync-workspace-version.py` updates those fields surgically
  (workspace members only; never a third-party crate that happens to share a semver like
  `dispatch2 0.3.1`). After it runs, both `cargo metadata --locked` checks above must pass, and
  `grep -R -- '-dev"' rust/Cargo.lock apps/linux/gtk/Cargo.lock` must return nothing for a
  non-dev release. Diff the locks: expect only workspace package `version =` lines to change
  (~one line per crate), matching prior release commits — not a full re-resolve.
- **Shell scripts on Windows:** `scripts/release/*.sh` need a real Bash. Prefer Git for Windows
  (`"C:/Program Files/Git/bin/bash.exe"`) over the Windows Store/WSL `bash` shim when WSL is
  not installed — the shim fails with `HCS_E_HYPERV_NOT_INSTALLED` and looks like a broken
  script. The Python sync script needs no Bash.
- Push with a GitHub account that has write access to `delllusional/DontSpeak`.

## 2 — Tag and trigger

```bash
ver="$(bash scripts/release/version.sh)"
git tag "v$ver"
git push origin main "v$ver"
```
Any `-dev` draft release(s) cut via step 10 are now superseded — clean up ALL of them (there
should only ever be one, but query rather than guess the exact tag string: a mid-cycle version
escalation, per step 1, can leave a draft tagged under an EARLIER `-dev` string than the one
that just shipped). Safe to run unconditionally (no-op if none exist):
```bash
gh api --paginate "repos/delllusional/DontSpeak/releases" --jq '.[] | select(.draft==true and (.tag_name | test("-dev"))) | "\(.id) \(.tag_name)"' |
while read -r id tag; do
  echo "Removing stale dev draft: $tag (id=$id)"
  gh api -X DELETE "repos/delllusional/DontSpeak/releases/$id"
  git push origin ":refs/tags/$tag" 2>/dev/null || true
  git tag -d "$tag" 2>/dev/null || true
done
```

## 3 — What runs on GitHub (release.yml)

1. **`check`** (tag = version guard) — fails fast if the tag ≠ `rust/Cargo.toml` version.
2. **`tests`** — `ci.yml` with `full-matrix: true`: the full three-OS matrix plus the `hygiene`
   gate (rustfmt both workspaces + rustdoc `-D warnings`), neither of which runs per-commit. Any
   OS failing — or hygiene drift — blocks the whole release.
3. **Builds** (parallel, after 1+2):
   - `windows` — self-contained portable zips, x64 + arm64 (unsigned; SmartScreen note).
   - `macos` — `DontSpeak.app` zips, arm64 + x86_64; Developer-ID sign + notarize + staple when
     the `APPLE_*` secrets exist, else ad-hoc (first launch hits Gatekeeper).
   - `linux` — per-arch tarballs (ubuntu-26.04 + -arm). Setup and tests must pass; tarball
     creation and upload are best-effort, so a packaging failure may leave an asset absent.
4. **`publish release`** — `gh release create` with all artifacts, fixed-name
   `install.sh` / `install.ps1`, and `checksums.txt` + `--generate-notes`. The installer
   assets come from the tagged commit and power the stable `releases/latest/download/...`
   one-liners. That flag only lists merged PULL REQUESTS — this repo pushes straight
   to `main` with no PRs, so it has nothing to draw from and renders as a bare compare link
   with no real content. Write the actual notes yourself (step 6).

## 4 — Monitor (mandatory)

```bash
run_id=$(gh run list --repo delllusional/DontSpeak --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$run_id" --repo delllusional/DontSpeak --exit-status   # long: run in background
```
On failure: `gh run view "$run_id" --log-failed` and read the actual error. Known failure
classes: the tag≠version guard; a platform-specific test failure the Linux-only per-commit CI
never exercised (e.g. Windows CRLF checkouts once broke a byte-for-byte test); runner-image/
toolchain drift; notarization secret expiry. If the run failed before `publish release`, no
release exists — the tag sits there without one.

**`gh run watch` network reliability:** the watcher polls GitHub every ~3s for 25+ minutes and
can drop mid-run with `wsarecv: An existing connection was forcibly closed by the remote host`
on long-haul connections, or with intermittent `HTTP 503` from the Actions API. If the
background watcher exits with a network/5xx error (not a build failure), restart it or poll
manually with `gh run view "$run_id" --json status,conclusion` on a 30–60s interval — the CI
run itself is unaffected.

## 5 — Verify

```bash
gh release view "v$ver" --repo delllusional/DontSpeak --json assets --jq '[.assets[].name]'
```
Expect **9 assets**: `checksums.txt` + `install.sh` + `install.ps1` + linux
`{x86_64,aarch64}.tar.gz` + macos `{aarch64,x86_64}.app.zip` + windows
`{x86_64,aarch64}.zip`. Missing Linux assets means the best-effort Linux job failed — decide
whether to re-cut or ship without. Missing either installer is a release failure: the README
and site use the fixed-name latest-release URLs.

Verify the installer assets are byte-identical to the tagged sources:
```bash
tmp="$(mktemp -d)"
gh release download "v$ver" --repo delllusional/DontSpeak \
  --pattern install.sh --pattern install.ps1 --dir "$tmp"
cmp "$tmp/install.sh" scripts/install/web/install.sh
cmp "$tmp/install.ps1" scripts/install/web/install.ps1
rm -rf "$tmp"
```

For a published release, also verify the stable latest-release endpoints resolve:
```bash
curl -fsSLI https://github.com/delllusional/DontSpeak/releases/latest/download/install.sh
curl -fsSLI https://github.com/delllusional/DontSpeak/releases/latest/download/install.ps1
```
Skip this endpoint check for a dev draft: drafts deliberately do not change
`releases/latest`, but still require both installer assets in their nine-asset set.

## 6 — Write real release notes

The summary prose has no script — write it directly (this whole release process already
runs through an agent capable of writing it, so a keyword-matching heuristic script was
tried and removed as strictly worse than just doing it). The change-stats table (below) is
scripted, since it's arithmetic, not judgment:
1. Find the previous release tag: `gh release list --limit 2` (the one before the one you
   just cut). List what changed: `git log <prev-tag>..v$ver --oneline`.
2. Group into sections that fit what ACTUALLY changed — don't force categories that end up
   empty. Typical shape: `## Bug fixes`, `## Features`, `## Shared changes` (rust/ engine,
   cross-platform), plus one section per platform that got platform-specific work this
   release (`## macOS`, `## Windows`, `## Linux`). One brief, plain-English line per change
   (not the raw commit subject — write what it means for the user, kept to a single line),
   linking to its commit with the link in square brackets, not wrapped in parens:
   `- <brief description>. [`<short-sha>`](https://github.com/delllusional/DontSpeak/commit/<sha>)`.
3. Append the change-stats table under its own `## Lines` heading — same level as the summary
   sections above, not a `---` divider — so it reads as one more section, not an appendix.
   The diff link (step 4) goes first, bare (no `**Diff**:` label), then the table under it:
   `scripts/release/release-stats.py <prev-tag> v$ver` prints the ready-to-paste markdown table
   splitting code vs. test vs. comment-only line changes across the `rust` (shared) /
   `apps/macos` / `apps/windows` / `apps/linux` buckets.
   ```
   ## Lines

   https://github.com/delllusional/DontSpeak/compare/<prev-tag>...v$ver

   <script output>
   ```
4. The diff link (GitHub's own compare view — every commit, not just the ones summarized
   above) is the bare URL at the top of the `## Lines` section, per step 3.
5. Submit it: `gh release edit "v$ver" --repo delllusional/DontSpeak --notes-file <file>`
   (or `--notes "..."` inline for something short).

## 7 — Re-cut a failed (or same-version test) release

Only while pre-1.0, or explicitly told backward-compat doesn't matter — normally bump the
version instead. Fix the cause, commit, then move the tag:
```bash
gh release delete "v$ver" --repo delllusional/DontSpeak --yes   # only if it was published
git push origin ":refs/tags/v$ver" && git tag -d "v$ver"
git tag "v$ver" && git push origin main "v$ver"                 # re-triggers the run
```
Then monitor again (step 4).

## 8 — After the release: deploy the site

Republishing dontspeak.org is a mandatory final step (the served install scripts must match the
release): run the `deploy-site` skill **in the `dontspeak.org` repo checkout** — the skill lives
there, not here. The one-command installers resolve the latest GitHub release via the API at
run time, so a brief site lag degrades gracefully — but don't skip the deploy.

## 9 — Bump to the next dev version

Immediately after the release publishes (step 5), bump to the next patch with a `-dev`
suffix (e.g. `0.3.1` → `0.3.2-dev`), or choose a higher minor/major `-dev` if you already
know the next release is larger. **One command does Cargo.toml + both locks** (GTK can't
inherit `version.workspace` — see its header; locks must not be full-re-resolved — see
step 1):
```bash
python3 scripts/release/sync-workspace-version.py --bump-dev
# or an explicit target:  python3 scripts/release/sync-workspace-version.py --set 0.4.0-dev
(cd rust && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
(cd apps/linux/gtk && cargo metadata --format-version 1 --locked --no-deps >/dev/null)
```
Commit all four files (`rust/Cargo.toml`, `rust/Cargo.lock`, `apps/linux/gtk/Cargo.toml`,
`apps/linux/gtk/Cargo.lock`). Skipping the sync doesn't fail fast: the tag/version guard
(step 3.1) only compares them at the NEXT tag push, so a missed sync here silently sits
stale for a whole release cycle. This is a small, code-free commit whose only job is to
make `main` visibly "ahead of the last release" — the exact-string tag/version guard never
sees this suffix since nothing ever tags a `-dev` version. When it's time to cut the NEXT
release, run `python3 scripts/release/sync-workspace-version.py --strip-dev` (or `--set`
with a higher non-dev version if what accumulated warrants minor/major), commit, then tag
as usual (step 2).

The older `bash scripts/release/sync-gtk-version.sh` only rewrites the GTK `Cargo.toml` and
is kept for callers that already use it; prefer the Python script for full four-file sync.

## 10 — Cut (or replace) an on-demand `-dev` draft release

For real installable dev binaries without officially shipping — e.g. someone needs to test a
fix before the next numbered release. Publishes the CURRENT `-dev` version already on `main`
(`rust/Cargo.toml`'s `[workspace.package] version`, e.g. `0.2.10-dev`) as a **draft** GitHub
Release with the full three-OS build matrix — same gates, same signing, nothing skipped (see
`release.yml`'s comment) — just marked `--draft` so GitHub's `releases/latest` resolution (used
by the one-command installers and the in-app update check) never picks it up.

Safe to run repeatedly as `main` moves, on the same `-dev` string: **replaces the previous
draft's tag and release object in place**, mirroring step 7's re-cut recipe — delete the remote
tag first, or GitHub keeps the old tag object and Actions may not re-trigger:
```bash
ver="$(bash scripts/release/version.sh)"
case "$ver" in
  *-dev*) ;;
  *) echo "current version '$ver' has no -dev suffix — this is for on-demand DEV drafts only; use step 2 for a real release" >&2; exit 1 ;;
esac
git push origin ":refs/tags/v$ver" 2>/dev/null || true   # drop any prior remote tag for this dev version
git tag -d "v$ver" 2>/dev/null || true
git tag "v$ver"
git push origin main "v$ver"                              # (re-)triggers release.yml
```
`release.yml`'s `release` job detects `-dev` in the tag and passes `--draft` (never
`--prerelease` — draft alone already excludes it from `releases/latest`), replacing any
existing release for that tag first via `gh api` list+delete-by-id (`gh release view`/`delete
<tag>` are unreliable for drafts — see the workflow's inline comment).

Monitor (step 4) and verify (step 5) exactly as for a real release, plus confirm draft status:
```bash
gh release view "v$ver" --repo delllusional/DontSpeak --json isDraft --jq .isDraft   # expect: true
```
Skip step 6 (hand-written release notes — disposable, the auto-generated compare link is fine)
and steps 8–9 (site deploy, version bump) — the `-dev` version doesn't change and nothing about
it is user-facing until a real release supersedes it (step 2 cleans it up automatically).

**Known cosmetic quirk:** the release's web URL may briefly show `untagged-<hash>` instead of
`v$ver` while draft (a known GitHub CLI/API display quirk) — the underlying `tag_name` is
correct, so this recipe's replace-in-place logic, asset attachment, and `--json
isDraft`/`tagName` all still work; it's display-only.

## Caveats

- A pushed tag on a commit whose tests later fail leaves a **tag without a release** — clean it
  up (step 7) or the next attempt's tag push is rejected as already existing.
- `workflow_dispatch` runs the build for testing without a tag — it does not publish.
- Don't `git tag -f` over a pushed tag without deleting the remote one first; GitHub keeps the
  old object and Actions may not re-trigger.
- **Don't skip step 9** (why: see there). One consequence worth calling out separately:
  re-cutting (step 7) must move the tag back to the ACTUAL release commit, never forward
  onto whatever unreleased work has since landed on `main` — a commit isn't part of a
  release just because `Cargo.toml`'s version string hasn't changed yet.
