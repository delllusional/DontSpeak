---
name: prepush
description: Run the exact CI gates locally, then push to origin only if they pass — so the per-commit CI on GitHub never goes red. Mirrors .github/workflows/ci.yml (clippy + test). Use when asked to push, prepush, "run CI locally", or verify a change before pushing to main.
---

# DontSpeak — prepush (local CI gate, then push)

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

**Source of truth:** `.github/workflows/ci.yml` — if a gate changes there, update this
skill. Runs on any dev machine; all cargo commands run in `rust/`.

Per-commit CI runs two Linux jobs. Run the same two locally, in order, and push only
once both are green. (rustfmt + rustdoc + cargo-deny are release-only — `ci.yml`'s
full-matrix `hygiene` and `cargo-deny` jobs, cleaned up / cleared by `make-release`
before tagging. swift-format is not enforced anywhere.)

## Commit attribution gate (run first and again immediately before push)

Read the canonical [commit-attribution policy](../../../docs/COMMIT-ATTRIBUTION.md)
in full before creating or rewriting a commit. This is a required pre-push gate, not
a commit-style reminder; the policy lives only in that shared file.

After the final squash, rebase, cherry-pick, or amend, inspect every outgoing commit:
```bash
node scripts/check-commit-attribution.mjs origin/main
git log --format=full origin/main..HEAD
```
The script enforces the mechanical trailer rules; compare the displayed model and
effort values against the canonical policy as well. Do not push until every message
conforms. Re-run both commands after any operation that rewrites commit messages.

## The two gates

1. **Clippy (deny warnings)**
   ```bash
   cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings
   ```
   `--keep-going` surfaces lints from every crate in one run — fix any warning rather
   than `#[allow]`-ing it away unless the user agrees. `--locked` matches CI: fail on
   `Cargo.lock` drift instead of silently updating it.
2. **Tests**
   ```bash
   cargo test --workspace --locked
   ```

If both pass, push. If either fails, fix it and re-run.

## cargo-deny (release-only, not a per-commit gate)

`cargo deny --all-features check` (advisories + bans + licenses + sources against `deny.toml` /
`Cargo.lock`) only runs in the release pipeline now — see the `make-release` skill.
It's still worth running here if you touched `Cargo.toml`/`Cargo.lock` or `deny.toml`,
so a new advisory or license doesn't surface for the first time at tag time:
```bash
cargo deny --all-features check
```
Install once with `cargo install cargo-deny --locked` (not part of the default
toolchain). A new advisory or a new license entering the graph needs a considered
`deny.toml` change (dated reason, scoped exception), not a reflexive `#[allow]`-style
widening — see `rust/deny.toml`'s existing entries for the expected rigor.

## Skill duplication check

The canonical and vendor-discovery skill trees must be byte-identical; see
[`docs/AGENT-SKILLS.md`](../../../docs/AGENT-SKILLS.md). Run before every push:
```bash
node scripts/sync-agent-skills.mjs --check
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
node scripts/check-commit-attribution.mjs origin/main && git log --format=full origin/main..HEAD
cd rust && cargo clippy --workspace --all-targets --keep-going --locked -- -D warnings && cargo test --workspace --locked && cargo deny --all-features check
cd .. && node scripts/sync-agent-skills.mjs --check
```
Green gates plus a conforming attribution inspection ⇒ safe to push.
