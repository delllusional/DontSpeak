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

## Before final verification and publishing

Keep all work on its feature branch, including branches created under the default
workflow and handed-off feature worktrees. Preserve its history unless the user
asks for a rewrite, fetch `origin/main` for comparison, re-run every applicable
verification, commit, and push the feature branch. Do not land on `main`, delete
the branch or worktree, or close related issues or pull requests unless the user
explicitly asks.

Report the feature-branch SHA and any issue or pull request state changes.

## Landing on main

Land only when the user explicitly asks:

1. Fetch `origin/main`, then update the local `main` worktree with
   `git pull --ff-only origin main`.
2. From the `main` worktree, cherry-pick only the requested, verified feature
   commit or commits. Stop and report any conflict.
3. Re-run every applicable verification on the resulting `main`.
4. Push `main` without force. Do not merge or rebase the feature branch into
   `main`.
5. Keep the feature branch and worktree unless the user explicitly asks to delete
   them.
6. Close related GitHub issues only for fixes now present on `main`. Prefer
   `Closes #N` or `Fixes #N` in the cherry-picked commit; otherwise close the issue
   with a one-line pointer to the main SHA. Close a related pull request only when
   the user asks or the requested landing makes it obsolete.

Report the main SHA, the cherry-picked commit SHAs, and any issue or pull request
state changes.
