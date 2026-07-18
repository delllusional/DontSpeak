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

Don't leave finished work only on a side branch. Land onto `main`, clean up the
branch, and close related GitHub issues. Don't open a pull request unless the
user asks for one.

1. Fetch `origin/main` again.
2. If the task branch has more than one commit, squash to one landing commit
   (keep every distinct `Agent:` trailer — [COMMIT-ATTRIBUTION.md](COMMIT-ATTRIBUTION.md)).
3. If upstream moved, rebase the clean task branch onto `origin/main`. Stop on conflict.
4. Re-run every applicable verification after rebase/squash.
5. **Land on `main`** (never force-push `main`):
   - Prefer: fast-forward local `main` to the verified task branch, then
     `git push origin main`.
   - If the branch can't fast-forward (diverged history, or the user says to
     pick): from the main worktree, cherry-pick the verified landing commit(s)
     onto up-to-date `main`, then push. Stop on cherry-pick conflict.
   - No merge commits on `main` for agent landings. If a user-requested PR is
     the merge vehicle, finish that PR, then still do the cleanup steps below.
6. **Delete the task branch** locally and on `origin` once `main` has the work
   (`git branch -d` / `-D` after a cherry-pick rewrite; `git push origin --delete
   <branch>`). Remove the task worktree
   (`ExitWorktree` / `git worktree remove`) so it doesn't linger.
7. **Close related GitHub issues** the work fixes. Prefer `Closes #N` (or
   `Fixes #N`) in the landing commit so GitHub auto-closes on push to `main`;
   otherwise `gh issue close N` with a one-line pointer to the main SHA. If a
   PR was opened for the branch, close it after `main` has the change (note the
   main SHA); don't leave the PR or issue open after a successful land.

Report the main SHA and any issue/PR numbers closed.
