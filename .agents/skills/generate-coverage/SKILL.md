---
name: generate-coverage
description: Generate an LLVM-based Rust test coverage report for the whole workspace or one crate, open it locally, and optionally publish a self-contained crate report as an Artifact. Use when asked for test coverage, a coverage report, or "how much of X is tested".
---

# DontSpeak — Rust test coverage

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

`cargo-llvm-cov` from `rust/`. (Mainstream tool with real Windows support.)

## Setup (once)

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked
```

## Run

```bash
cd rust && cargo llvm-cov --workspace --locked
cd rust && cargo llvm-cov -p <crate> [-p <crate2> ...] --locked
cd rust && cargo llvm-cov --workspace --exclude <crate> --locked
```

Terminal table (Regions/Functions/Lines/Branches %) answers most questions.

## Platform scope

`cfg(target_os)` code only exists for the **current host** — other OS files are
absent, not 0%. Scope claims to "on `<this OS>`". Syscall/hardware modules stay low
even when well-designed — note the ceiling, don't chase 100%.

## HTML

```bash
cargo llvm-cov report --html --output-dir "$TEMP/ds-cov-html"   # bash
cargo llvm-cov report --html --output-dir "$env:TEMP\ds-cov-html"  # pwsh
# open …/html/index.html via start/open/xdg-open
```

## Artifact (few crates only)

Native report is multi-page. For a focused crate set, flatten then publish:

```bash
node scripts/ci/merge-crate-coverage.js \
  "$TEMP/ds-cov-html/html" <crate>[,<crate2>] <scratchpad>/coverage.html
```

Don't flatten full workspace — use local HTML instead.

## Notes

- On-demand only; not a CI gate.
- No markdown export — parse terminal table or `--json` if needed.
