# Fresh task baseline

This file is the single source of truth for starting and landing repository work
from a current baseline.

## Default workflow

Use this workflow before planning, checking, reviewing, building, testing,
implementing, auditing, releasing, or landing repository work:

1. Inspect `git worktree list` and the repository's task-worktree directory. Reuse
   an existing worktree only when it belongs to the same task.
2. In the worktree where `main` is checked out, confirm that `main` has no tracked
   changes that would interfere with an update.
3. Run `git pull --ff-only origin main`. This must update local `main` without a
   merge commit.
4. Create a named task branch and worktree from the refreshed `main`. Use the
   repository's `EnterWorktree` helper when it is available; otherwise use
   `git worktree add -b <task-branch> <task-worktree> main`.
5. Perform the entire task, including its checks and commits, in that task
   worktree.

If `main` has conflicting tracked changes, has diverged, or cannot be
fast-forwarded, stop and report the condition. Do not reset, force, discard, or
overwrite another person's work.

## Explicit-target exceptions

When the task explicitly targets a pull request, branch, tag, commit, existing task
worktree, or current uncommitted changes, inspect or work from that target instead
of silently switching to `main`. Fetch `origin/main` first so comparisons use the
current upstream baseline. A handed-off task worktree is an explicit target.

A conversational or documentation-only answer that does not inspect repository
state does not require a worktree.

## Before final verification and landing

1. Fetch `origin/main` again.
2. If upstream `main` moved, rebase the clean task branch onto `origin/main`. Stop
   and report conflicts instead of forcing a result.
3. Rerun every applicable verification step after the rebase.
4. Fast-forward local `main` to the verified task branch, then push `main`
   normally. Never force-push.
5. Remove the completed task worktree and branch when cleanup is safe.
