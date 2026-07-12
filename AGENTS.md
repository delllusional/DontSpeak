# DontSpeak

A local voice layer for Claude Code, Codex, and Qwen Code: the agent speaks its
replies aloud, the user dictates back with one key (Caps Lock). One native app per OS
(macOS SwiftUI, Windows WinUI, Linux GTK4) hosts the same Rust engine **in-process**
over a C ABI (`ds-core`) — there is no separate daemon. The Claude Code hooks and the
MCP server are thin clients that talk to the engine over a Unix-domain socket.

Full design: [ARCHITECTURE.md](ARCHITECTURE.md). Crate-by-crate roles: [rust/README.md](rust/README.md).
Build prerequisites per OS: [CONTRIBUTING.md](CONTRIBUTING.md).

## Workspace layout

- `rust/crates/` — 23 single-purpose crates (Rust workspace, `rust-version = 1.97`).
  Notable ones: `ds-core` (the stable C ABI the apps link), `dontspeakd` (the engine
  library), `dontspeak` (the one CLI: MCP server + hook entries + installers),
  `ds-tools` (the single MCP tool catalog), `ds-config` (paths + `config.toml` +
  `~/.claude/settings.json` merge).
- `apps/macos/` (SwiftUI, most polished host), `apps/windows/winui/` (.NET 10),
  `apps/linux/gtk/` (GTK4 + libadwaita) — each a thin menu-bar/health/permissions UI
  that hosts the engine and is the login item. Voice/engine control is via MCP, not
  the app UI.
- `web/` — the dontspeak.org site (deployed locally via the `deploy-site` skill, not
  CI — see git log for `site deploys run locally`).

## Reading these instructions with a different coding agent

This file is the single source of truth for repo/product knowledge. Codex CLI,
Gemini CLI, and Grok Build read it natively; Claude Code gets it via `CLAUDE.md`'s
`@AGENTS.md` import and Qwen Code via `QWEN.md`'s identical import — both wrapper
files are one line. Edit AGENTS.md itself, never the wrappers, and don't restate
its content (the invariants below, especially) inside a tool-specific file — point
at this file instead, so there's one place to update.

Skills live physically under `.agents/skills/` — the location Codex CLI and Gemini
CLI/Qwen Code scan natively. `.claude/skills/` holds a same-named symlink per skill
so Claude Code and Grok Build resolve the identical file with no copy. See
CONTRIBUTING.md's Windows prereqs for the one-time `git config core.symlinks true`
this needs on Windows.

`.claude/agents/*.md` (the `ds-planner` / `ds-plan-reviewer` / `ds-implementer` /
`ds-risk-auditor` / `ds-lander` pipeline, `.claude/workflows/plan-review-implement.js`)
is a Claude-Code-specific subagent mechanism with no confirmed equivalent discovery
path across tools yet. Those files reference this file's invariants rather than
restating them — keep it that way when editing either side.

## Concurrent sessions: work in a worktree

More than one Claude Code session (yours, a background workflow's implement stage,
someone else's terminal) can be active in this same clone at once. To keep one
session's edits from clobbering another's uncommitted changes in the shared working
tree, **every session must call `EnterWorktree` before its first file edit** in this
repo — no matter how small the change is. Read-only sessions (answering a question,
looking something up) don't need one.

- Before starting, check `.claude/worktrees/` and `git worktree list` — an existing
  worktree already named after your task means another session already started it;
  don't open a second one for the same work.
- Name the worktree after the task/issue (short kebab-case, e.g. the GitHub issue
  slug), so it's identifiable in `git worktree list`.
- Commit your change on the worktree's branch before finishing — don't leave
  uncommitted edits as the only copy of the work.
- When the change is done (and, for risky changes, audited clear): merge the
  branch into `main`, push to `origin` (the public repo; never `wip`), then
  `ExitWorktree` with `action: remove` so `.claude/worktrees/` doesn't accumulate
  stale directories.

The `plan-review-implement` workflow (see CLAUDE.md) already does this for its
Implement/Land stages — you don't need to wrap it yourself when using that pipeline.

## Out-of-scope findings: file a GitHub issue

Working a task often turns up a second real problem — a bug, a missing test, a
stale doc, a gap in one of the invariants below. If it's small and obviously
correct, fix it inline and say so in your report. If it isn't part of what you were
actually asked to do, don't silently drop it and don't silently expand scope to fix
it anyway: file it as a GitHub issue instead —
`gh issue create --repo delllusional/DontSpeak --title "..." --body "..."`, with a
label if one fits (`bug`, `enhancement`, `documentation`, `question` are in use —
`gh label list --repo delllusional/DontSpeak` for the full set). Check
`gh issue list --repo delllusional/DontSpeak` first so you don't file a duplicate of
something already open. Mention the issue number in your final report so it isn't
lost.

## Commit attribution

End every commit message with a single `AI:` trailer instead of any built-in
AI-attribution line — no `Co-Authored-By`, `Assisted-by`, `Generated-by`, or similar,
and no other attribution beyond this one line. Format:

```
AI: <model-id> <effort-level>
```

e.g. `AI: claude-sonnet-5 xhigh`. State your own model id and the current
session's reasoning-effort level (Claude Code: read `$CLAUDE_EFFORT`) — don't omit
the field if the effort level is unclear, give your best description instead.
Claude Code's own automatic `Co-Authored-By` trailer is disabled repo-wide via
`.claude/settings.json`'s `attribution` key so it can't duplicate this line; Codex
and Qwen Code don't read that file, so if either tool's own auto-attribution can't be
suppressed from here, drop it manually before finishing the commit message.

**Squashing.** When combining several commits into one (interactive rebase,
squash-merge), carry forward every distinct `AI: <model-id> <effort-level>` pair
from the commits being combined — one line per distinct pair, inherited into the
result. Don't drop a contributor's line just because its commit wasn't the last one
before the squash; don't repeat a pair that already appears.

## Commands

All Rust commands run from `rust/` (the workspace root — not the repo root).

```sh
cd rust
cargo build --workspace --locked                 # build everything
cargo test --workspace --locked                  # run the real test suite (this is what CI runs)
cargo test -p ds-config --locked                 # test one crate
cargo test -p ds-config wire::registry --locked  # run tests matching a name/path in one crate
cargo clippy --workspace --all-targets --locked -- -D warnings   # the per-commit lint gate
cargo fmt --all --check                          # release-only gate — not enforced per commit
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked  # release-only gate
```

The GTK host is a separate workspace and needs its own fmt check:
`cd apps/linux/gtk && cargo fmt --all --check`.

The macOS SwiftPM tests need the FFI staticlib built first (`apps/macos/build.sh` does
this): `cd rust && cargo build --profile release-ffi --locked -p ds-core && cd ../apps/macos && swift test`.
The WinUI app's xunit tests: `dotnet test apps/windows/winui.tests` (has no runnable
`dotnet build`/`test` shortcut outside CI otherwise — see `.github/actions/dotnet-test-winui`).

Use the `build-linux` / `build-macos` / `build-windows` skills rather than raw
build/package commands per OS — see [docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md) for why
the three runtime pieces (CLI, engine, host app) need different rebuild routes.

## Invariants worth knowing before you touch things

- **Config lives in `config.toml`** under the OS data dir, never in
  `~/.claude/settings.json` — that file stays purely Claude Code's own (hooks + its
  own `voice` block).
- **No codegen for the FFI boundary.** uniffi was evaluated and deliberately rejected
  for the ~29-function `ds-core` surface — see
  [ARCHITECTURE.md § FFI boundary](ARCHITECTURE.md#ffi-boundary).
  If you touch `model_status`, edit the Rust source of truth in `ds-status` then
  hand-update the two mirrors (`apps/windows/winui/Native.cs`,
  `apps/macos/Sources/DontSpeak/DontSpeakCore.swift`) and run its round-trip test.
  Don't reintroduce a generated-bindings toolchain here.
- **Three runtime pieces deploy by three different routes** — see
  [docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md). Rebuilding the wrong piece (e.g. just
  the CLI when you changed the engine or helper) leaves the running app on stale code
  that *looks* installed. Always check that doc before concluding a fix works.
- **No statically linked/bundled GPL or LGPL code.** `espeak-ng` (GPLv3) is invoked
  only as an optional external process, never linked. The Linux build *dynamically*
  links a small, disclosed set of LGPL system libraries (GTK4, libadwaita, ALSA,
  PulseAudio) — allowed under LGPL's dynamic-linking exception — see
  [NOTICE.md](NOTICE.md)'s "Linux build: LGPL system libraries" section before
  changing how any of those are linked, and before adding any new native dependency.
- **No hardcoded UI strings.** Every new user-facing string goes in the shared
  `ds-i18n` catalog (`rust/crates/ds-i18n/locales/en.yml`), rendered through the FFI
  — never literal text in Swift/C#/XAML — see [docs/LOCALIZATION.md](docs/LOCALIZATION.md).
- **Tests never touch a real network endpoint.** Any code that makes an outbound HTTP
  call (model downloads in `ds-model`, the GitHub releases update-check) must be
  structured so its tests point at a local mock instead of the real service —
  `httpmock` is already a dev-dependency of `ds-model` for exactly this; parameterize
  the function under test by base URL (see `ds-model`'s `download.rs`/`update_check.rs`
  for the pattern: a `pub(crate)`/`pub` `..._at(base_url, ...)` inner function the
  public entry point calls with the real URL, and tests call directly with a
  `MockServer`'s URL). A test that hits the live internet is a CI-flake and
  rate-limit risk, not just a style nit — code review should treat it as a bug.
- **Error handling is `Result<_, String>` at the boundaries.** Error messages cross
  the IPC/FFI/MCP-tool boundaries as text (NDJSON lines, the C ABI, tool replies), so
  `String` is the boundary error type. Typed error enums exist only where a caller
  actually branches on the failure kind — currently four: `EngineError`
  (`dontspeakd::boot`), `HooksMergeError` / `CodexMergeError` (`ds-config::wire`),
  and `PreflightError` (`ds-platform`). `ds-model`'s download engine instead encodes
  its retry classification in `io::ErrorKind` (`InvalidData` = checksum mismatch and
  `NotFound` = HTTP 4xx, both permanent/fail-fast; `TimedOut` = transport/5xx/
  truncation, transient/retried). No anyhow/thiserror — follow the layer's existing
  style.

## Gates — per-commit vs release (deliberate split, not an oversight)

Per-commit CI (`.github/workflows/ci.yml`, Linux-only) runs **only** `cargo clippy
--workspace --all-targets --locked -- -D warnings` and `cargo test --workspace --locked`
— fast, on purpose. `cargo fmt --check`, `RUSTDOCFLAGS="-D warnings" cargo doc`, and
`cargo deny check` (`rust/deny.toml` — advisories + bans + licenses + sources) are all
**release-only** gates (the `hygiene` and `cargo-deny` jobs, gated on full-matrix), so
formatting/doc drift and dependency-graph issues (a new advisory, a new license
entering the graph) accumulate between releases by design rather than nagging every
commit — fix them once before tagging (`.claude/skills/make-release` does this). A
separate scheduled workflow (`.github/workflows/dependency-audit.yml`) re-runs `cargo
deny check advisories` daily, since a newly-disclosed RustSec advisory against an
unchanged `Cargo.lock` has no commit — and, now, no imminent release — to attach the
check to. Use `.claude/skills/prepush` to run the exact per-commit gates locally before
pushing (cargo-deny is no longer one of them — see the skill for the release-only
caveat).

## Code Comments

Comments must be **valuable and concise**. They exist to record what the code + names do not make obvious to a careful reader.

**Keep (high value):**
- `//!` module docs that capture design rationale, historical bugs fixed, invariants, "why this shape", and non-obvious decisions.
- `///` docs on public APIs that pin exact contracts (wire tokens, error modes, version-skew fallbacks with `#[serde(default)]`, session scoping, "HANDLE-FREE").
- SAFETY comments that justify the precise preconditions making an `unsafe` block sound (lifetimes of data, app-signed ABI, Once serialization, intentional leaks for in-flight callbacks, etc.).
- Notes on "single source of truth", drift/parity guards, cross-crate sharing, "LOAD-BEARING" behavior, and gotchas/races/ordering.
- Test comments that explain *why the test exists* (the regression or invariant it pins).

**Strip or shorten (low or no value):**
- Restatements of the obvious ("Returns the X", "increments the counter", "The foo pidfile", repeating the function signature).
- Duplicated boilerplate across files or sites (centralize once + cross-reference).
- Verbose re-explanations of the same concept when one canonical location + links suffices.
- Pure "what" docs on leaf accessors, simple consts, or data variants when the name + surrounding context is clear.

**Practical rules:**
- One canonical explanation per concept. Use "see X" or cross-refs liberally.
- Module `//!` for big picture + evolution. Public `///` for contracts.
- Explicitly document non-obvious choices and the bugs they prevent.
- Update or delete notes when the underlying behavior or rationale changes.
- These rules were produced by a full-crate comment audit (see commit history).
