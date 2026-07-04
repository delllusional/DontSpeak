@AGENTS.md

# Claude Code

Claude Code– and Claude-agent–specific material only. See [AGENTS.md](AGENTS.md)
(imported above) for the product description, workspace layout, commands,
invariants, and CI gates — that file is the source of truth; don't duplicate it
here.

## Skills

`build-linux` / `build-macos` / `build-windows` — build+install/package per OS.
`prepush` — run CI gates locally before pushing. `make-release` — cut and monitor a
tagged release. `verify-wiring` — re-check the client MCP/hook wiring registry
(`ds-config/src/wire/registry.rs`) against current client versions.

## Agentic flow for nontrivial changes

For anything bigger than a one-file mechanical edit, use this pipeline rather than
planning and implementing in one pass — the point is to catch invariant violations
(wrong config file, missing FFI mirror, GPL linkage, hardcoded strings) *before* code
is written, not after:

1. **Plan** — `ds-planner` produces a concrete plan grounded in this repo's actual
   invariants and deploy routes, ending with an explicit `Risk: yes/no` line.
2. **Review the plan** — `ds-plan-reviewer` checks the plan against the repo (not
   against its own claims), independently confirms or overrules the risk call, and
   returns **Approve** or **Revise**. Don't implement past a **Revise**.
3. **Implement** — `ds-implementer` executes the approved plan (or one slice of it).
   For a change that touches shared engine/`ds-platform` code and must land on all
   three OS hosts, fan this out — one `ds-implementer` per platform in parallel, e.g.
   via the Workflow tool — rather than one linear pass; the repo is already split
   cleanly along that boundary.
4. **Audit, conditionally** — only if step 1 or 2 flagged `Risk: yes` (FFI boundary,
   `ds-ipc` protocol, model checksum/download pinning, OS permissions/entitlements,
   dependency licensing, or the release/signing pipeline), run `ds-risk-auditor` on
   the implemented change before calling it done. Skip this step for ordinary changes
   — use `code-review` instead.

A saved workflow encoding these four stages (with the risk gate) lives at
`.claude/workflows/plan-review-implement.js` — invoke it by name via the Workflow
tool when full automation is wanted; each stage also works fine invoked by hand via
the Agent tool.
