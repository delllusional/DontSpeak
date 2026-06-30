---
name: prepush
description: Run the exact CI gates locally, then push to origin only if they pass — so the per-commit CI on GitHub never goes red. Mirrors .github/workflows/ci.yml (clippy + test). Use when asked to push, prepush, "run CI locally", or verify a change before pushing to main.
---

# DontSpeak — prepush (local CI gate, then push)

> **Runs on:** this box. **Working dir for all cargo commands:** `rust/` (the workspace lives there). **Source of truth:** `.github/workflows/ci.yml` — if a gate changes there, update this skill.

Per-commit CI runs **two Linux jobs**, all in `rust/`. Run the same two locally **in order** and stop at the first failure. Only push when both are green. (The rustfmt gate was dropped from CI — formatting is no longer enforced.)

## The two gates (run in `rust/`)

1. **Clippy (deny warnings)** — `cargo clippy --workspace --all-targets --keep-going -- -D warnings`
   - `--keep-going` surfaces lints from every crate in one run. Any warning fails CI, so fix them — don't `#[allow]` to silence unless the user agrees.
2. **Tests** — `cargo test --workspace`

If both pass, proceed to push. If either fails, fix it and re-run.

## Push

- Confirm there's something to push (`git status`, `git log origin/main..HEAD`). Stage + commit per the user's intent first if there are uncommitted changes (end commit messages with the `Co-Authored-By` trailer).
- `git push origin <branch>` (default `main`).
- `origin` is the project's `delllusional/DontSpeak` GitHub repo.

## Caveats (be honest about these)

- **Platform cfg:** per-commit CI is **Linux-only**. On Windows/macOS, clippy + tests compile *this host's* cfg, so Linux-specific `#[cfg(target_os = "linux")]` code (evdev, PipeWire, uinput) and its lints are **not** exercised locally. For an exact match, run the two gates inside **WSL Ubuntu** (needs `libasound2-dev libpulse-dev pkg-config`). For changes to shared/platform-agnostic code, the local run is sufficient.
- This skill covers the **per-commit** gate only. A tagged **release** also runs the full ubuntu+windows+macOS matrix (`release.yml` → `ci.yml` with `full-matrix: true`); that's out of scope here — use the `build-*` / release path for releases.

## One-liner (when you just want the gate)

```bash
cd rust && cargo clippy --workspace --all-targets --keep-going -- -D warnings && cargo test --workspace
```
Green ⇒ safe to push.
