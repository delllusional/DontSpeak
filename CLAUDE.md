@AGENTS.md

# Claude Code

Claude Code– and Claude-agent–specific material only. See [AGENTS.md](AGENTS.md)
(imported above) for the product description, workspace layout, commands,
invariants, CI gates, and **Code Comments** guidelines — that file is the source of truth; don't duplicate it
here.

## Editing AGENTS.md or this file

Both are read into every session, so keep them high-signal (this mirrors how
zed-industries/zed maintains its own `.rules` file, which `CLAUDE.md` and `AGENTS.md`
there both resolve to):

- **High bar for new rules.** A rule earns a place here only if it's non-obvious
  (someone who knows the repo would still get it wrong), repeatedly encountered (came
  up more than once, including more than once in a single session), and specific
  enough to act on. Rules scoped to one crate/app belong in that crate's own docs, not
  here.
- **Traps, not maps.** Don't add architectural descriptions (module layout, data flow,
  key types) — those go stale fast and are already legible from the source. Record the
  mistake a careful reader would still make, not a summary of the code.
- **No drive-by additions.** If a task surfaces a pattern worth recording, call it out
  in the commit/PR description rather than folding it into AGENTS.md as part of an
  unrelated change, so it gets reviewed on its own before becoming a standing rule
  every future session reads.

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
5. **Land** — once implementation (and, if flagged, the audit) is clean, `ds-lander`
   merges the worktree's branch into `main` and pushes to `origin`. It re-runs the
   per-commit gates first and stops rather than resolving a conflict itself.

A saved workflow encoding these five stages (with the risk gate) lives at
`.claude/workflows/plan-review-implement.js` — invoke it by name via the Workflow
tool when full automation is wanted; each stage also works fine invoked by hand via
the Agent tool.
