# Fresh task baseline

Start and land work from a current baseline.

## Default workflow

Before planning, checking, reviewing, building, testing, implementing, auditing,
releasing, or landing:

1. Inspect `git worktree list` and the task-worktree directory. Reuse only for the
   same task.
2. Find the worktree where `main` is checked out (`git worktree list`) and `cd` into
   it. Confirm no tracked changes would block an update.
3. **From that main worktree** — not wherever you started — run
   `git pull --ff-only origin main` (no merge commit). `git pull ... origin main` only
   advances the local `main` ref when `main` is the checked-out branch in your current
   directory; run it from any other branch/worktree and it silently updates nothing,
   leaving local `main` stale for step 4.
4. Create a named task branch + worktree from refreshed `main`
   (`EnterWorktree` if available, else
   `git worktree add -b <task-branch> <task-worktree> main`).
5. Do all work, checks, and commits in that worktree.

If `main` has conflicting changes, has diverged, or can't fast-forward: stop and
report. Don't reset, force, discard, or overwrite others' work.

## Explicit-target exceptions

Task names a PR, branch, tag, commit, existing worktree, or uncommitted changes →
work that target. Still fetch `origin/main` for comparisons. Handed-off worktree =
explicit target.

Pure Q&A that doesn't inspect the repo needs no worktree.

## Before final verification and landing

1. Fetch `origin/main` again.
2. If upstream moved, rebase the clean task branch onto `origin/main`. Stop on conflict.
3. Re-run every applicable verification after rebase.
4. Fast-forward local `main` to the verified branch; push `main` normally (never force).
5. Remove completed worktree/branch when safe.
