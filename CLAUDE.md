@AGENTS.md

# Claude Code

Claude-only material. Product layout, commands, invariants, gates, and comment
rules live in [AGENTS.md](AGENTS.md) — don't restate them here.

## Editing AGENTS.md or this file

Both load every session; keep them high-signal (same bar as zed's `.rules`):

- **High bar.** Non-obvious, repeatedly hit, and actionable. Crate-local rules stay
  in that crate's docs.
- **Traps, not maps.** Don't paste architecture here — it goes stale. Record the
  mistake a careful reader still makes.
- **No drive-by rules.** If a task surfaces a pattern, note it in the PR/commit;
  land AGENTS.md changes on their own review.

## Agentic flow (nontrivial changes)

Bigger than a one-file mechanical edit — catch invariant breaks *before* code:

1. **Plan** — `ds-planner`: concrete plan + `Risk: yes/no`.
2. **Review plan** — `ds-plan-reviewer` vs repo reality → **Approve** or **Revise**.
   Don't implement past Revise.
3. **Implement** — `ds-implementer` on the approved plan (or one slice). Shared
   engine/`ds-platform` across three hosts: one implementer per OS in parallel.
4. **Audit if Risk: yes** — `ds-risk-auditor` for FFI, `ds-ipc`, model pinning, OS
   permissions, licensing, release/signing. Otherwise use `code-review`.
5. **Land** — `ds-lander` merges the worktree branch to `main` and pushes after
   re-running per-commit gates; stops on conflict.

Workflow: `.claude/workflows/plan-review-implement.js` (or invoke stages by hand).
