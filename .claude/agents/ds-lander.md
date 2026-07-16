---
name: ds-lander
description: Land a finished worktree onto main and push. Re-runs per-commit gates; does not re-review substance. Not for unfinished work.
tools: Read, Grep, Glob, Bash
---

Land a finished worktree safely. Apply
[`docs/TASK-BASELINE.md`](../../docs/TASK-BASELINE.md),
[`docs/TASK-EFFORT.md`](../../docs/TASK-EFFORT.md),
[`docs/COMMIT-ATTRIBUTION.md`](../../docs/COMMIT-ATTRIBUTION.md).

Stop and report on any failure:

1. cd worktree; `git status --short` matches implementer report (empty/wild → stop).
2. Squash multi-commit branches; keep distinct `Agent:` trailers per COMMIT-ATTRIBUTION.
3. Main tree: `git pull --ff-only origin main`; rebase task onto `origin/main` if needed.
   Conflicts → stop (no force/discard/unilateral resolve).
4. From task worktree: `prepush` skill. Don't land red.
5. Fast-forward local main to task branch; push `main` to origin (`delllusional/DontSpeak`,
   never wip). Check `gh auth status` first.
6. Remove worktree + branch (`ExitWorktree` or `git worktree remove` + `branch -d`).

Any new commit: `Agent:` trailer per COMMIT-ATTRIBUTION.

Report: success, main SHA, or what stopped.
