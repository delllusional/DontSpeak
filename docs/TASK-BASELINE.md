# Fresh task baseline

Start and land work from a current baseline.

## Default workflow

Before planning, checking, reviewing, building, testing, implementing, auditing,
releasing, or landing:

1. Use the `start-task` skill. Inspect `git worktree list`; reuse only an explicit
   same-task handoff.
2. For a new task, use `EnterWorktree` when available. The repository hook performs
   the remaining setup. Otherwise run:
   `node scripts/agents/task-worktree.mjs create <task-branch> --name <task-name>`.
3. The shared starter finds the worktree where `main` is checked out, refuses tracked
   changes there, runs `git pull --ff-only origin main` from that worktree, and creates
   `.worktrees/<task-name>` on the requested branch at the refreshed commit.
4. Confirm `git status --short --branch` in the returned worktree before writing.
   Do all work, checks, and commits there.

If the shared starter is unavailable, perform the same steps manually. `git pull
... origin main` only advances local `main` when run from the worktree where `main`
is checked out; running it from a feature branch leaves local `main` stale.

If `main` has conflicting changes, has diverged, or can't fast-forward: stop and
report. Don't reset, force, discard, or overwrite others' work.

## Explicit-target exceptions

Task names a PR, branch, tag, commit, existing worktree, or uncommitted changes →
work that target. Still fetch `origin/main` for comparisons. Handed-off worktree =
explicit target.

Pure Q&A that doesn't inspect the repo needs no worktree.

## Parallel isolation

- One writing agent owns one task worktree and branch. Read-only review may share the
  assigned worktree.
- Workers never edit the `main` worktree or another task worktree and do not stash,
  rebase, reset, prune, remove worktrees, or rewrite shared branches.
- The integrator alone updates `main`, lands verified commits, and performs worktree
  cleanup when the user explicitly requests it.
- Namespace build outputs, ports, databases, processes, and other external state per
  task. Git worktrees isolate tracked files, not machine-wide resources.

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
