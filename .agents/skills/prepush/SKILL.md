---
name: prepush
description: On feature branches, run only fast local hygiene, push, monitor CI, then immediately fast-forward a green exact head onto main without local retesting. Run full gates locally only for a direct unverified main push or when explicitly requested. Use when asked to push, prepush, check CI, or land.
---

# DontSpeak — prepush

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Source of truth: `.github/workflows/ci.yml`. Feature branches use CI for the full
per-commit gate. Cargo commands run from `rust/`. rustfmt/rustdoc/deny are
release-only (`make-release`).

## Attribution (before commit rewrite and before push)

Read [`docs/COMMIT-ATTRIBUTION.md`](../../../docs/COMMIT-ATTRIBUTION.md). After any
squash/rebase/amend:

```bash
node scripts/agents/check-commit-attribution.mjs origin/main
git log --format=full origin/main..HEAD
```

Don't push until both pass; re-run after message rewrites.

## Feature branch default: minimum local, full CI

Before pushing any branch other than `main`:

```bash
node scripts/agents/check-commit-attribution.mjs origin/main
git log --format=full origin/main..HEAD
git diff --check origin/main...HEAD
node scripts/agents/sync-agent-skills.mjs --check
git push origin "$(git branch --show-current)"
```

Do not duplicate the full script, clippy, or workspace-test suites locally unless
the user explicitly requests local CI or a CI failure needs focused reproduction.
After pushing, monitor the pull request until all non-release checks reach a
terminal state:

```bash
gh pr checks --watch --interval 10
```

If the branch has no pull request yet, use `gh run list --branch <branch>` and
`gh run watch <run-id>`. On failure, inspect with `gh run view <run-id>
--log-failed`, fix, repeat the minimum local gate, push, and monitor again. Do not
report the push as complete while required CI is still pending unless the user
explicitly asks not to wait.

When every applicable non-release check for the exact current head is green, land
it immediately using [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md):
update the dedicated `main` worktree with `git pull --ff-only`, require
`origin/main` to be an ancestor of the recorded feature SHA, run `git merge
--ff-only <feature-sha>` there, and push `main` without force. Do not re-run local
checks, cherry-pick, rebase, or create a merge commit. If fast-forward is
impossible, stop and report.

## Full local gates: direct main push or explicit request

1. **Script tests**
   ```bash
   node --test scripts/agents/agent-attribution.test.mjs scripts/agents/run-bash.test.mjs scripts/agents/task-worktree.test.mjs scripts/ci/check-shell-ascii.test.mjs scripts/ci/merge-crate-coverage.test.js scripts/install/web/install.test.mjs
   python3 scripts/release/release-stats.test.py
   node scripts/agents/run-bash.mjs apps/macos/bundle-lib.test.sh
   ```
2. **Clippy**
   ```bash
   cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings
   ```
   Fix warnings; don't `#[allow]` without agreement. `--locked` fails on lock drift.
3. **Tests** (match `ci.yml`: fetch, then offline so Cargo/HTTP cannot reach the net;
   loopback stays up for httpmock)
   ```bash
   cargo fetch --locked
   CARGO_NET_OFFLINE=true \
   HTTP_PROXY=http://127.0.0.1:9 HTTPS_PROXY=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9 \
   NO_PROXY=127.0.0.1,localhost,::1 HF_HUB_OFFLINE=1 \
     cargo test --workspace --locked --offline
   ```
   PowerShell: set the same env vars, then `cargo test --workspace --locked --offline`
   after `cargo fetch --locked`.

All green → push `main`. Any failure → fix and re-run. This section applies only
to a direct unverified `main` push or an explicit request for local CI. A green
feature branch never duplicates the checks locally before fast-forward landing.

## cargo-deny (optional here; required at release)

If you touched `Cargo.toml` / lock / `deny.toml`:

```bash
cargo deny --manifest-path rust/Cargo.toml --all-features --config rust/deny.toml check
cargo deny --manifest-path apps/linux/gtk/Cargo.toml --all-features --config rust/deny.toml check
```

`cargo install cargo-deny --locked`. New advisories/licenses need a dated scoped
exception in `deny.toml`, not a reflexive widen.

## Skill mirrors

```bash
node scripts/agents/sync-agent-skills.mjs --check
```

Drift → run without `--check`, review, re-check. See [AGENT-SKILLS.md](../../../docs/AGENT-SKILLS.md).

## Push

Confirm commits to push and attribution. Feature branch: push first, then monitor
CI and fast-forward a green exact head onto `main` immediately. Direct unverified
`main`: run the full local gates first. Push to `delllusional/DontSpeak`.

## Caveats

- Per-commit CI is **Linux-only**. Local Windows/macOS won't exercise Linux cfg
  (evdev, PipeWire, uinput). Exact match: WSL/VM with `libasound2-dev libpulse-dev
  pkg-config`. Shared code: local OS is fine.
- Full OS matrix + hygiene = release only (`make-release` / `build-*`).

## Feature-branch one-liner

```bash
node scripts/agents/check-commit-attribution.mjs origin/main && git log --format=full origin/main..HEAD
git diff --check origin/main...HEAD
node scripts/agents/sync-agent-skills.mjs --check
git push origin "$(git branch --show-current)"
gh pr checks --watch --interval 10
```

Green CI continues directly into the ff-only landing procedure in
`docs/TASK-BASELINE.md`; there is no local test step between CI and the `main`
push.

## Direct-main full gate

```bash
node scripts/agents/check-commit-attribution.mjs origin/main && git log --format=full origin/main..HEAD
node --test scripts/agents/agent-attribution.test.mjs scripts/agents/run-bash.test.mjs scripts/agents/task-worktree.test.mjs scripts/ci/check-shell-ascii.test.mjs scripts/ci/merge-crate-coverage.test.js scripts/install/web/install.test.mjs
python3 scripts/release/release-stats.test.py
node scripts/agents/run-bash.mjs apps/macos/bundle-lib.test.sh
cd rust && cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings
cargo fetch --locked
CARGO_NET_OFFLINE=true \
HTTP_PROXY=http://127.0.0.1:9 HTTPS_PROXY=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9 \
NO_PROXY=127.0.0.1,localhost,::1 HF_HUB_OFFLINE=1 \
  cargo test --workspace --locked --offline
cd .. && node scripts/agents/sync-agent-skills.mjs --check
```
