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
   Never implement past a Revise — it returns to `ds-planner` with the findings for
   another round, bounded at two. A revision addresses findings; narrowing the fix
   until a finding stops applying doesn't, and a finding believed wrong gets refuted
   in the plan rather than dropped. Still Revise after two rounds → human.
3. **Implement** — `ds-implementer` on the approved plan (or one slice). Shared
   engine/`ds-platform` across three hosts: one implementer per OS in parallel.
4. **Audit if Risk: yes** — `ds-risk-auditor` for FFI, `ds-ipc`, model pinning, OS
   permissions, licensing, release/signing. Otherwise use `code-review`.
5. **Land** — the default ending, not an extra request: `ds-lander` lands the
   worktree branch on `main` (FF or cherry-pick), pushes, deletes the
   branch/worktree, and closes related issues/PRs after re-running per-commit gates
   — see [docs/TASK-BASELINE.md](docs/TASK-BASELINE.md); stops on conflict. Skip it
   only when the user explicitly asked to keep the work on its branch, or when the
   step-4 audit returned a finding.

Workflow: `.claude/workflows/plan-review-implement.js` (or invoke stages by hand).
