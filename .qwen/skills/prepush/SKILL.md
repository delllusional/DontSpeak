---
name: prepush
description: Run the exact CI gates locally, then push to origin only if they pass — so the per-commit CI on GitHub never goes red. Mirrors .github/workflows/ci.yml (script tests + clippy + Rust tests). Use when asked to push, prepush, "run CI locally", or verify a change before pushing to main.
---

# DontSpeak — prepush (local CI gate, then push)

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

**Source of truth:** `.github/workflows/ci.yml` — if a gate changes there, update this
skill. Runs on any dev machine; all cargo commands run in `rust/`.

Per-commit CI runs three Linux jobs. Run the same three locally, in order, and push only
once all are green. (rustfmt + rustdoc + cargo-deny are release-only — `ci.yml`'s
full-matrix `hygiene` and `cargo-deny` jobs, cleaned up / cleared by `make-release`
before tagging. swift-format is not enforced anywhere.)

## Commit attribution gate (run first and again immediately before push)

Read the canonical [commit-attribution policy](../../../docs/COMMIT-ATTRIBUTION.md)
in full before creating or rewriting a commit. This is a required pre-push gate, not
a commit-style reminder; the policy lives only in that shared file.

After the final squash, rebase, cherry-pick, or amend, inspect every outgoing commit:
```bash
node scripts/agents/check-commit-attribution.mjs origin/main
git log --format=full origin/main..HEAD
```
The script enforces the mechanical trailer rules; compare the displayed model and
effort values against the canonical policy as well. Do not push until every message
conforms. Re-run both commands after any operation that rewrites commit messages.

## The three gates

1. **Repository script tests**
   ```bash
   node --test scripts/agents/agent-attribution.test.mjs scripts/ci/merge-crate-coverage.test.js
   ```
2. **Clippy (deny warnings)**
   ```bash
   cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings
   ```
   `--keep-going` surfaces lints from every crate in one run — fix any warning rather
   than `#[allow]`-ing it away unless the user agrees. `--locked` matches CI: fail on
   `Cargo.lock` drift instead of silently updating it.
3. **Rust tests** (match `ci.yml`: fetch first, then offline test so Cargo and
   HTTP clients cannot reach the network; loopback stays available for httpmock)
   ```bash
   cargo fetch --locked
   CARGO_NET_OFFLINE=true \
   HTTP_PROXY=http://127.0.0.1:9 HTTPS_PROXY=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9 \
   NO_PROXY=127.0.0.1,localhost,::1 HF_HUB_OFFLINE=1 \
     cargo test --workspace --locked --offline
   ```
   On PowerShell, set the same env vars then run
   `cargo test --workspace --locked --offline` after `cargo fetch --locked`.

If all three pass, push. If any fails, fix it and re-run.

## cargo-deny (release-only, not a per-commit gate)

The two all-feature cargo-deny graphs (advisories + bans + licenses + sources against
`deny.toml` / both lockfiles) only run in the release pipeline now — see the
`make-release` skill.
It's still worth running here if you touched `Cargo.toml`/`Cargo.lock` or `deny.toml`,
so a new advisory or license doesn't surface for the first time at tag time:
```bash
cargo deny --manifest-path rust/Cargo.toml --all-features check --config rust/deny.toml
cargo deny --manifest-path apps/linux/gtk/Cargo.toml --all-features check --config rust/deny.toml
```
Install once with `cargo install cargo-deny --locked` (not part of the default
toolchain). A new advisory or a new license entering the graph needs a considered
`deny.toml` change (dated reason, scoped exception), not a reflexive `#[allow]`-style
widening — see `rust/deny.toml`'s existing entries for the expected rigor.

## Skill duplication check

The canonical and vendor-discovery skill trees must be byte-identical; see
[`docs/AGENT-SKILLS.md`](../../../docs/AGENT-SKILLS.md). Run before every push:
```bash
node scripts/agents/sync-agent-skills.mjs --check
```
Drift means the canonical tree changed without regenerating every mirror. Run the
script without `--check`, review the generated diff, then re-run the check.

## Push

- Confirm there's something to push (`git status`, `git log origin/main..HEAD`); stage
  and commit per the user's intent first if there are uncommitted changes. The required
  attribution inspection above must be clean before continuing.
- `git push origin <branch>` (default `main`). `origin` is `delllusional/DontSpeak`.

## Caveats

- **Linux-only gate.** Per-commit CI runs only on `ubuntu-latest`. On Windows/macOS,
  clippy + tests compile *that host's* cfg, so Linux-only code (evdev, PipeWire,
  uinput) isn't exercised locally. For an exact match, run both gates in a Linux
  environment (WSL/VM/container; needs `libasound2-dev libpulse-dev pkg-config`). For
  changes to shared/platform-agnostic code, the local run on any OS is sufficient.
- **Per-commit scope only.** A tagged release also runs the full ubuntu+windows+macOS
  matrix (`release.yml` → `ci.yml` with `full-matrix: true`) plus the hygiene gate;
  that's out of scope here — use `build-*` / `make-release` for releases.

## One-liner

```bash
node scripts/agents/check-commit-attribution.mjs origin/main && git log --format=full origin/main..HEAD
node --test scripts/agents/agent-attribution.test.mjs scripts/ci/merge-crate-coverage.test.js
cd rust && cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings
cargo fetch --locked
CARGO_NET_OFFLINE=true \
HTTP_PROXY=http://127.0.0.1:9 HTTPS_PROXY=http://127.0.0.1:9 ALL_PROXY=http://127.0.0.1:9 \
NO_PROXY=127.0.0.1,localhost,::1 HF_HUB_OFFLINE=1 \
  cargo test --workspace --locked --offline
cd .. && node scripts/agents/sync-agent-skills.mjs --check
```
Green gates plus a conforming attribution inspection ⇒ safe to push.
