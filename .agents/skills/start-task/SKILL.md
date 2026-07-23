---
name: start-task
description: Start DontSpeak repository work from the correct Git baseline in an isolated branch and worktree. Use whenever beginning or resuming a task that will inspect, plan, review, build, test, implement, audit, release, or land repository work, especially when multiple agents or tools may run concurrently.
---

# Start a DontSpeak task

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

## Select the target

- A named branch, pull request, tag, commit, worktree, or uncommitted handoff is an
  explicit target. Use that target and fetch `origin/main` only for comparison.
- Pure Q&A that does not inspect the repository needs no worktree.
- Otherwise create a new task worktree from refreshed `main`.

## Start new work

With Claude, call `EnterWorktree` once with a unique short task name. The repository's
`WorktreeCreate` hook refreshes the dedicated `main` worktree, creates a unique branch
under `.worktrees/`, and moves the session into it.

With another host, run from any DontSpeak worktree:

```bash
node scripts/agents/task-worktree.mjs create <task-branch> --name <task-name>
```

Use the returned absolute `worktree` path for every edit, command, build, and test.
Before writing, run `git status --short --branch` and record `git rev-parse HEAD` as
the task's base commit.

## Preserve isolation

- One writing agent owns one worktree and branch. Read-only agents may inspect it.
- Workers never edit the `main` worktree or another task worktree.
- Workers do not stash, rebase, reset, prune, remove worktrees, or rewrite shared
  branches. The integrator owns repository administration and landing.
- The integrator immediately fast-forwards an exact CI-green feature head onto
  `main` without repeated local checks; see `docs/TASK-BASELINE.md`.
- Keep build outputs, ports, databases, and live processes task-local. A worktree
  isolates tracked files, not external resources.
- Commit and report the branch, absolute worktree path, base commit, and checks run.
