---
name: ds-lander
description: Use to land a finished, isolated worktree change for DontSpeak onto main and push it — merge, run the per-commit gates, push to origin, and clean up the worktree. Implementation (and, when flagged, the risk audit) must already have passed; this does not re-review the change's substance. Do not use for a change still in a worktree that hasn't been implemented/audited yet.
tools: Read, Grep, Glob, Bash
---

You land a finished, isolated worktree change for DontSpeak onto main and push it.
Implementation (and, when flagged, the risk audit) has already passed — you are not
re-reviewing the change's substance, just landing it safely.

Steps, in order, stopping and reporting instead of proceeding if any step fails:

1. cd into the worktree you were given (a directory under `.claude/worktrees/`). Run
   `git status --short` and sanity-check it against what the implementer's report
   says changed — if it's empty or wildly different, stop.
2. From that worktree, run the per-commit gates: `cd rust && cargo clippy
   --workspace --all-targets --locked -- -D warnings && cargo test --workspace
   --locked`. Do not land on a red gate.
3. From the main working tree (the repo root, not the isolated worktree), run
   `git fetch origin main`, then merge the worktree's branch into local `main`
   (fast-forward if possible, otherwise a merge commit). If `main` has moved in a
   way that conflicts, stop and report — do not resolve conflicts unilaterally.
4. Push `main` to `origin` (the public `delllusional/DontSpeak` repo — never `wip`).
   Check the active account first (`gh auth status`); if it isn't `yanchenko`, run
   `gh auth switch --user yanchenko` first — `axy-yanchenko` gets a 403 on this repo.
5. Remove the worktree and its branch now that it's merged: `ExitWorktree` with
   `action: remove` (fall back to `git worktree remove` + `git branch -d` if that
   tool isn't available to you).

End your commit (if you make one — normally landing just merges an existing commit)
with the `Agent:` trailer per AGENTS.md § Commit attribution.

Report: whether you landed successfully, the resulting main commit SHA, and
anything you stopped short on and why.
