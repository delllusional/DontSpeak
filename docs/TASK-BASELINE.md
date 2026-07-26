# Fresh task baseline

Canonical source for starting, publishing, landing, cleaning up, and closing related
issues. Other instructions link here instead of copying these rules.

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
- The integrator alone consolidates task history, rebases the prepared task commit,
  updates `main`, lands it, and cleans up worktrees and branches.
- Namespace build outputs, ports, databases, processes, and other external state per
  task. Git worktrees isolate tracked files, not machine-wide resources.

## Before final verification and publishing

Keep all work on its feature branch, including branches created under the default
workflow and handed-off feature worktrees. Before publishing, reduce the complete
task diff to exactly one intentional commit on top of `origin/main`; preserve every
distinct `Agent:` trailer under `docs/COMMIT-ATTRIBUTION.md`. Amend that commit for
subsequent fixes instead of accumulating task commits. Verify that
`git rev-list --count origin/main..HEAD` is `1` and that
`git rev-list --merges origin/main..HEAD` is empty, then push using `prepush`.
Feature branches run only the minimum local gate; remote CI owns full per-commit
verification. Preserve the feature branch and worktree through landing.

Report the feature-branch SHA and any issue or pull request state changes.

## Landing on main

Every task contributes exactly one non-merge commit to `main`. Do not wait for a
separate landing request and do not repeat full local checks:

1. Confirm the branch is exactly one non-merge commit ahead of its current base,
   push it, and require every applicable non-release CI check for that exact SHA to
   reach a successful terminal state. Pending, skipped-required, cancelled, or
   failed checks do not qualify.
2. Fetch `origin/main`. If it advanced, the integrator rebases the single task
   commit onto it, verifies the one-commit/no-merge shape and attribution again,
   updates the remote task branch with `--force-with-lease`, and reruns remote CI
   for the new exact SHA. Never land a rebased SHA using results from its parent.
3. Inspect open pull requests for the task branch. When one exists, land through
   that pull request using a rebase or squash method that produces exactly one
   non-merge commit, then verify the PR is merged and `main` advanced by that one
   prepared change. Never bypass an open task PR with a direct push.
4. Without an open PR, update the dedicated `main` worktree with
   `git pull --ff-only origin main`, require `origin/main` to be the prepared
   commit's direct parent, run `git merge --ff-only <feature-sha>`, and push `main`
   without force. Stop on any ancestry or fast-forward failure.
5. Verify the landed range contains exactly one commit and no merge commit. Then
   confirm the feature worktree is clean, remove
   that exact worktree, delete the exact local feature branch, and delete its remote
   branch if present. Stop without deleting anything if the worktree is dirty or a
   path, branch name, or expected feature SHA is ambiguous.
6. Close related GitHub issues only for fixes now present on `main`. Prefer
   `Closes #N` or `Fixes #N` in the landed commit; otherwise close the issue
   with a one-line pointer to the main SHA. Close a related pull request only when
   it is already merged or the landing makes it obsolete.

Report the prepared feature SHA, landed main SHA, branch/worktree cleanup, and any
issue or pull request state changes.
