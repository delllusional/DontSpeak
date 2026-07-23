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
asks for a rewrite, fetch `origin/main` for comparison, commit, and push the feature
branch using `prepush`. Feature branches run only the minimum local gate; CI owns
the full per-commit verification. Preserve the feature branch and worktree until
its current head is green and the fast-forward landing is pushed successfully,
then clean them up as part of landing.

Report the feature-branch SHA and any issue or pull request state changes.

## Landing on main

Every feature branch lands immediately when the required CI checks for its exact
current head are green. Do not wait for a separate landing request and do not
repeat local checks:

1. Record the exact feature SHA and confirm every applicable non-release CI check
   for that SHA reached a successful terminal state. Pending, skipped-required,
   cancelled, or failed checks do not qualify.
2. Fetch `origin/main`, then update the local `main` worktree with
   `git pull --ff-only origin main`.
3. Confirm `origin/main` is an ancestor of the recorded feature SHA. From the
   `main` worktree run `git merge --ff-only <feature-sha>`. If either ancestry or
   the ff-only merge fails, stop and report; do not rebase, cherry-pick, create a
   merge commit, or rewrite either branch.
4. Do not re-run local verification after green CI. Push `main` without force and
   verify `origin/main` equals the recorded feature SHA.
5. After the `main` push succeeds, confirm the feature worktree is clean, remove
   that exact worktree, delete the exact local feature branch, and delete its remote
   branch if present. Stop without deleting anything if the worktree is dirty or a
   path, branch name, or expected feature SHA is ambiguous.
6. Close related GitHub issues only for fixes now present on `main`. Prefer
   `Closes #N` or `Fixes #N` in the fast-forwarded commits; otherwise close the issue
   with a one-line pointer to the main SHA. Close a related pull request only when
   the fast-forward landing makes it obsolete.

Report the main SHA, the fast-forwarded feature SHA, branch/worktree cleanup, and
any issue or pull request state changes.
