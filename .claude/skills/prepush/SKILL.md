---
name: prepush
description: Run the exact CI gates locally, then push to origin only if they pass — so the per-commit CI on GitHub never goes red. Mirrors .github/workflows/ci.yml (script tests + clippy + Rust tests). Use when asked to push, prepush, "run CI locally", or verify a change before pushing to main.
---

# DontSpeak — prepush

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Source of truth: `.github/workflows/ci.yml`. Cargo from `rust/`. rustfmt/rustdoc/deny
are release-only (`make-release`).

## Attribution (before commit rewrite and before push)

Read [`docs/COMMIT-ATTRIBUTION.md`](../../../docs/COMMIT-ATTRIBUTION.md). After any
squash/rebase/amend:

```bash
node scripts/agents/check-commit-attribution.mjs origin/main
git log --format=full origin/main..HEAD
```

Don't push until both pass; re-run after message rewrites.

## Three gates

1. **Script tests**
   ```bash
   node --test scripts/agents/agent-attribution.test.mjs scripts/agents/run-bash.test.mjs scripts/agents/task-worktree.test.mjs scripts/ci/merge-crate-coverage.test.js scripts/install/web/install.test.mjs
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

All green → push. Any fail → fix and re-run.

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

Confirm commits to push; attribution clean; `git push origin <branch>` (default `main`)
to `delllusional/DontSpeak`.

## Caveats

- Per-commit CI is **Linux-only**. Local Windows/macOS won't exercise Linux cfg
  (evdev, PipeWire, uinput). Exact match: WSL/VM with `libasound2-dev libpulse-dev
  pkg-config`. Shared code: local OS is fine.
- Full OS matrix + hygiene = release only (`make-release` / `build-*`).

## One-liner

```bash
node scripts/agents/check-commit-attribution.mjs origin/main && git log --format=full origin/main..HEAD
node --test scripts/agents/agent-attribution.test.mjs scripts/agents/run-bash.test.mjs scripts/agents/task-worktree.test.mjs scripts/ci/merge-crate-coverage.test.js scripts/install/web/install.test.mjs
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
