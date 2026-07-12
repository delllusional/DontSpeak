---
name: generate-coverage
description: Generate an LLVM-based Rust test coverage report for the whole workspace or one crate, open it locally, and optionally publish a self-contained crate report as an Artifact. Use when asked for test coverage, a coverage report, or "how much of X is tested".
---

# DontSpeak — Rust test coverage

Uses `cargo-llvm-cov` (LLVM source-based instrumentation) — the current standard
Rust coverage tool, and the only mainstream one with real Windows support (unlike
`tarpaulin`, which is Linux-only by default). All commands run from `rust/`.

## Setup (once per machine)

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov --locked
```

Skip if `cargo llvm-cov --version` already succeeds.

## Run

Whole workspace:
```bash
cd rust && cargo llvm-cov --workspace --locked
```

One or more specific crates — `-p`/`--package` is repeatable, so this combines them
into ONE report directly; no need to run `--workspace` and filter afterward:
```bash
cd rust && cargo llvm-cov -p <crate> [-p <crate2> ...] --locked
```

Whole workspace minus a few crates: `cargo llvm-cov --workspace --exclude <crate> --locked`.

All forms print a per-file table (Regions/Functions/Lines/Branches, each with Cover%)
to the terminal — that alone answers most "how well tested is X" questions.

## Platform scope — read before interpreting numbers

`ds-platform` (and any other `cfg(target_os = "...")`-gated code) only compiles the
current host's OS impl. On Windows, `macos.rs`/`linux.rs` don't exist in the build at
all — they're not "0% covered", they're absent. Don't report a cross-platform
coverage number from a single-OS run; scope the claim to "on `<this OS>`".

Similarly, syscall-heavy modules (real key injection, warm audio processes, IPC
sockets) will show low coverage even when well-designed, because they need a live
session/hardware to exercise — that's a testability ceiling, not necessarily a gap
worth chasing. Don't recommend chasing 100% there; note it and move on.

## HTML report

Generate from the same collected profile data (no test re-run):
```bash
cargo llvm-cov report --html --output-dir "$TEMP/ds-cov-html"   # bash
cargo llvm-cov report --html --output-dir "$env:TEMP\ds-cov-html"  # pwsh
```

Open the native multi-page report locally — this is the right choice for a
whole-workspace run (100+ files; don't try to flatten that into one page):
```bash
start "" "$TEMP/ds-cov-html/html/index.html"        # Windows
open "$TEMP/ds-cov-html/html/index.html"            # macOS
xdg-open "$TEMP/ds-cov-html/html/index.html"        # Linux
```

## Artifact (a focused handful of crates only)

The Artifact tool needs one self-contained file with no external references — the
native report is a directory of linked pages, so it can't be published as-is. For a
**focused** report (one crate, or a few related ones — a handful of files total),
flatten it with the bundled script, then publish via the Artifact tool:

```bash
node scripts/merge-crate-coverage.js \
  "$TEMP/ds-cov-html/html" <crate-name>[,<crate-name2>,...] <scratchpad>/coverage.html
```

This pulls the matching crate(s)' rows out of the summary table, rewrites their
links to in-page anchors, and inlines each per-file source view as a collapsible
`<details>` section with the original green/red line highlighting intact. Don't run
this for a full-workspace report or more than a handful of crates — the page gets
unwieldy; fall back to opening the native report locally instead.

## Notes

- This is a manual/on-demand tool, not a CI gate — coverage numbers have no natural
  pass/fail threshold here and platform code will always show artificially low
  numbers, so it isn't wired into `prepush` or `.github/workflows/ci.yml`.
- `cargo llvm-cov` has no built-in markdown export (json/lcov/cobertura/codecov/html
  only — see `cargo llvm-cov report --help`). If markdown is specifically needed,
  parse the terminal table or `--json` output by hand rather than expecting a flag.
