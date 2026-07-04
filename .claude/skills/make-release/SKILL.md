---
name: make-release
description: Cut a DontSpeak release — tag the single-source version, push the tag to trigger release.yml on GitHub, MONITOR the ~25-30-min run (it can and does fail), verify the published assets, then deploy the site. Also covers re-cutting a failed release. Use when asked to release, cut/publish a version, re-release, or when a release build failed.
---

# DontSpeak — make a release

> A release is **tag-triggered CI**: pushing `v<version>` runs `.github/workflows/release.yml`,
> which gates, builds all platforms, and publishes the GitHub Release with binaries. Nothing
> builds locally. The run takes **~25-30 minutes even on a warm cache**. A same-
> `Cargo.lock` cache hit does make the gate materially faster (Linux clippy 2m2s→35s, tests
> 2m32s→50s), but that's a small slice of the critical path — the `macos` build job (signing +
> notarization + both Apple targets, ~18-19 min) and the slowest `tests` matrix legs (macos-26 /
> windows-2025, ~9-10 min each) dominate total wall time regardless of Rust cache warmth, so
> don't expect a warm cache alone to pull the total toward 20 min. Can fail — treat "tag pushed"
> as the start, not the end: monitor it to completion and verify the assets.

## 1 — Preconditions

- **Version**: the single source is `rust/Cargo.toml` → `[workspace.package] version` (read by
  `scripts/version.sh`). The tag must be `v` + exactly that version — the `check` job fails the
  run fast otherwise. Bump it (+ commit) for a new release.
- **Green main, pushed**: run the `prepush` skill first (clippy + tests + cargo-deny — the
  same suite the release re-runs); the tagged commit must be on `origin/main`.
- **Hygiene clean — run `cargo fmt` locally before tagging.** The release (unlike per-commit CI
  and `prepush`, neither of which runs rustfmt/rustdoc) also gates on rustfmt + rustdoc
  (`ci.yml`'s `hygiene` job, full-matrix only) — **the single most common re-cut cause**. Format
  both workspaces, verify clean, and rebuild docs before tagging:
  ```bash
  (cd rust && cargo fmt) && (cd apps/linux/gtk && cargo fmt)     # apply
  (cd rust && cargo fmt --check) && (cd apps/linux/gtk && cargo fmt --check)   # must be clean
  (cd rust && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked)   # must pass
  ```
  Commit whatever `cargo fmt` changed — a release is where the codebase gets cleaned up.
- **macOS Swift tests (run on a Mac).** The release matrix runs `swift test` on the macos-26
  leg, but neither `prepush` nor the gate above covers it — a Swift-logic regression would
  otherwise surface only ~30 min into the release. Run it here first (builds the FFI staticlib
  the SwiftPM package force-loads, then tests):
  ```bash
  (cd rust && cargo build --profile release-ffi --locked -p ds-core)
  (cd apps/macos && MACOSX_DEPLOYMENT_TARGET=14.0 swift test)   # must pass, 0 failures
  ```
  (Windows also runs `dotnet test` in the release, but it's `continue-on-error` — non-blocking.)
- **`--locked` catches Cargo.lock drift.** CI runs every cargo gate with `--locked`; `prepush`
  and the fmt commands above don't, so a `Cargo.toml` dep bump with a stale `Cargo.lock` passes
  locally but fails the release. Re-run the two prepush gates locked before tagging:
  `(cd rust && cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings && cargo test --workspace --locked)`.
- Push with a GitHub account that has write access to `delllusional/DontSpeak`.

## 2 — Tag and trigger

```bash
ver="$(bash scripts/version.sh)"
git tag "v$ver"
git push origin main "v$ver"
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
   - `linux` — per-arch tarballs (ubuntu-26.04 + -arm). **`continue-on-error`**: a Linux hiccup
     doesn't block the release, but ships it without Linux assets — check they're there.
4. **`publish release`** — `gh release create` with all artifacts + `checksums.txt`.

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

## 5 — Verify

```bash
gh release view "v$ver" --repo delllusional/DontSpeak --json assets --jq '[.assets[].name]'
```
Expect **7 assets**: `checksums.txt` + linux `{x86_64,aarch64}.tar.gz` + macos
`{aarch64,x86_64}.app.zip` + windows `{x86_64,aarch64}.zip`. Missing Linux assets means the
best-effort Linux job failed — decide whether to re-cut or ship without.

## 6 — Re-cut a failed (or same-version test) release

Only while pre-1.0, or explicitly told backward-compat doesn't matter — normally bump the
version instead. Fix the cause, commit, then move the tag:
```bash
gh release delete "v$ver" --repo delllusional/DontSpeak --yes   # only if it was published
git push origin ":refs/tags/v$ver" && git tag -d "v$ver"
git tag "v$ver" && git push origin main "v$ver"                 # re-triggers the run
```
Then monitor again (step 4).

## 7 — After the release: deploy the site

Republishing dontspeak.org is a mandatory final step (the served install scripts must match the
release): run the `deploy-site` skill **in the `dontspeak.org` repo checkout** — the skill lives
there, not here. The one-command installers resolve the latest GitHub release via the API at
run time, so a brief site lag degrades gracefully — but don't skip the deploy.

## Caveats

- A pushed tag on a commit whose tests later fail leaves a **tag without a release** — clean it
  up (step 6) or the next attempt's tag push is rejected as already existing.
- `workflow_dispatch` runs the build for testing without a tag — it does not publish.
- Don't `git tag -f` over a pushed tag without deleting the remote one first; GitHub keeps the
  old object and Actions may not re-trigger.
