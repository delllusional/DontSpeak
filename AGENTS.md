# DontSpeak

Local voice layer for Claude Code, Codex, Qwen Code, Grok, Kimi Code, and Hermes
Agent: the agent speaks replies aloud; the user dictates with Caps Lock. One native
app per OS (macOS SwiftUI, Windows WinUI, Linux GTK4) hosts the same Rust engine
**in-process** via `ds-core` C ABI — no separate daemon. Hooks and the MCP server
are thin clients over a Unix-domain socket.

- Design: [ARCHITECTURE.md](ARCHITECTURE.md)
- Crates: [rust/README.md](rust/README.md)
- Build prereqs: [CONTRIBUTING.md](CONTRIBUTING.md)

## Workspace layout

- `rust/crates/` — 25 crates (`rust-version = 1.97`). Key: `ds-core` (C ABI),
  `dontspeakd` (engine lib), `dontspeak` (CLI: MCP + hooks + installers),
  `ds-tools` (MCP catalog), `ds-config` (paths + `config.toml` + wire registry),
  `ds-wire` (wire CLI + boot reconcile).
- `apps/macos/` (SwiftUI), `apps/windows/winui/` (.NET 10), `apps/linux/gtk/`
  (GTK4 + libadwaita) — thin host UIs; engine control is via MCP.

Website/`llms.txt`: separate repo `delllusional/dontspeak.org`. This repo only
publishes installer assets the site references.

## Canonical policies (read, don't restate)

| Topic | Doc |
| --- | --- |
| Instruction wrappers + skill mirrors | [docs/AGENT-SKILLS.md](docs/AGENT-SKILLS.md) |
| Fresh `main`, worktrees, land + close | [docs/TASK-BASELINE.md](docs/TASK-BASELINE.md) |
| Reasoning effort | [docs/TASK-EFFORT.md](docs/TASK-EFFORT.md) |
| `Agent:` commit trailer | [docs/COMMIT-ATTRIBUTION.md](docs/COMMIT-ATTRIBUTION.md) |

**Always in force:** start every task from freshly pulled `main` unless the task
names another target. Commit and push work on its feature branch. Land the verified
commits on `main` once that exact head is green, using fast-forward — landing is
the default ending, not a separate request. Keep work on its branch only when the user
explicitly asks or a risk audit returned a finding. After the
`main` push succeeds, remove the clean feature worktree and delete its local and
remote branch. Close related issues only after their fixes reach `main` —
[TASK-BASELINE.md](docs/TASK-BASELINE.md) has the steps and exceptions. Read-only
work (reviews, audits, Q&A over the repo) is not exempt.

## Out-of-scope findings

Small obvious fixes: do inline and note in the report. Otherwise use the
[`create-github-issue`](.agents/skills/create-github-issue/SKILL.md) skill — don't
drop the finding, expand scope, or substitute labels for Type, Priority, and
Effort. Check open issues first; cite the issue number in the final report.

## Commands

Run from `rust/` (workspace root, not repo root):

```sh
cd rust
cargo build --workspace --locked
cargo test --workspace --locked
cargo test -p ds-config --locked
cargo test -p ds-config wire::registry --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check                          # release-only
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked  # release-only
```

GTK fmt: `cd apps/linux/gtk && cargo fmt --all --check`.

On Windows, run repository Bash scripts through
`node scripts/agents/run-bash.mjs <script> [args...]`. Never invoke bare `bash`:
Windows resolves it to the System32 WSL launcher on machines where WSL may be
disabled. The wrapper resolves Git Bash from the Git installation and fails closed
when Git Bash is unavailable.

macOS Swift tests need the FFI staticlib first, pinned to the app's macOS floor —
unpinned, cargo targets the host OS and the link warns "built for newer macOS":
`cd rust && MACOSX_DEPLOYMENT_TARGET=14.0 cargo build --profile release-ffi --locked -p ds-core && cd ../apps/macos && swift test`.

WinUI: `dotnet test apps/windows/winui.tests` (see `.github/actions/dotnet-test-winui`).

Prefer `build-linux` / `build-macos` / `build-windows` skills over raw package
commands — [docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md).

## Invariants

- **No backward compatibility.** Pre-release, no installed user base: don't design
  migrations, config/asset self-heal, retired-file cleanup, or upgrade paths for
  "existing installs" — ship the simplest correct change and assume a fresh
  install. Delete this rule at first public release.
- **Config:** `config.toml` under the OS data dir. Never put DontSpeak settings in
  `~/.claude/settings.json` (Claude's hooks + `voice` block only).
- **No FFI codegen.** uniffi rejected for the 35-fn `ds-core` surface — see
  [ARCHITECTURE.md § FFI](ARCHITECTURE.md#ffi-boundary). For `model_status`: edit
  `ds-status`, hand-update both mirrors (`apps/windows/winui/Native.cs`,
  `apps/macos/Sources/DontSpeakLogic/ModelStatusDTO.swift`), run the round-trip
  tests (Rust `ds-status`, Windows `HealthSnapshotTests`, macOS
  `ModelStatusContractTests`).
- **Three deploy routes.** CLI, engine, and host app update separately — wrong
  rebuild = stale running code. See [docs/BUILD-DEPLOY.md](docs/BUILD-DEPLOY.md).
- **Speech frontend runtimes.** English dictionary misses use a checksum-pinned ONNX
  G2P model via ORT (so Apple MLX TTS still needs the ORT dylib). Kokoro's other
  routed languages go through one checksum-pinned frontend asset: the dynamically
  loaded `espeakng-loader` runtime. Japanese and Mandarin are published by the model
  but not routed — their native pipelines were dropped. Keep downloads and notices in
  sync. Linux also dynamically links the disclosed system libraries in
  [NOTICE.md](NOTICE.md).
- **No Cargo + Git-LFS deps.** libgit2 checkout skips LFS smudge (~132-byte pointer).
  `git-lfs` and `CARGO_NET_GIT_FETCH_WITH_CLI` do not fix checkout. crates.io is fine;
  check `.gitattributes` before any `{ git = ..., rev = ... }`.
- **No hardcoded UI strings.** New user-facing text → `ds-i18n` catalog
  (`rust/crates/ds-i18n/locales/en.yml`) via FFI — [docs/LOCALIZATION.md](docs/LOCALIZATION.md).
- **ASCII-only `.sh` / `.ps1`.** They carry no BOM, so the console decodes them by its
  own codepage: Windows PowerShell 5.1 turned every em dash in installer output into
  mojibake. Plain `--`, `->`, `...`. `node scripts/ci/check-shell-ascii.mjs` gates it at
  release.
- **Tests never touch live resources.** Tempdir, loopback mocks, fake children only —
  not real config/cache/logs, hardware, credentials, audio, or network. `#[ignore]` is
  not an escape hatch. HTTP callers (`ds-model` downloads, update-check) take a base URL
  so tests use `httpmock` (`download.rs` / `update_check.rs` pattern: public entry with
  real URL, `..._at(base_url, ...)` for tests). Live-network tests are bugs.
- **Boundary errors are `Result<_, String>`.** IPC/FFI/MCP carry text. Typed enums only
  where callers branch: `EngineError`, `HooksMergeError` / `CodexMergeError`,
  `PreflightError`. `ds-model` retries via `io::ErrorKind` (`InvalidData` /
  `NotFound` = permanent; `TimedOut` = transient). No anyhow/thiserror.

## Gates

`.github/workflows/ci.yml` is source of truth. On feature branches, `prepush`
runs only attribution, diff, and skill-mirror hygiene locally, then pushes and
monitors the full per-commit CI gate. A branch whose exact head is green is
fast-forwarded onto `main` immediately with no repeated local checks. Direct
unverified `main` pushes run the full gate locally first. `make-release` =
release/hygiene matrix. Don't move checks between them. Scheduled dependency
audit stays separate.

## Code comments

Short. Valuable info only — no filler, no restating clear names/signatures, no
duplicate notes of the same fact.

**Keep:** design rationale; public contracts (wire tokens, error modes, defaults);
SAFETY preconditions; single-source / drift / race notes; why a test exists.

**Strip:** signature restatements; pure "what"; history/changelog asides; the same
explanation restated on module, item, and call site; comments that only say what
something is *not* / does *not* do (rephrase to the positive contract if the
constraint is load-bearing, else delete). Negative form stays only for hard-to-
infer safety, fail-closed, race, or isolation invariants.

One canonical note per concept; cross-ref elsewhere. Update or delete when
behavior changes.
