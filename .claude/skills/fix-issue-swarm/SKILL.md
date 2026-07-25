---
name: fix-issue-swarm
description: Audit the DontSpeak GitHub issue queue, select high-value low-risk issues that need no user input or product decision, fix non-overlapping issues with parallel agents in isolated branches and worktrees, integrate them on a batch bugfix branch, review and verify them, land the exact green batch on main, close the issues, and repeat until the qualified queue is exhausted. Use for autonomous issue triage, parallel bugfix batches, issue sweeps, or requests to repeatedly pick and land every safe repository fix.
---

# Fix an issue swarm

Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
[`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md). Use `high` effort or higher.
Use the `start-task` skill for every integration or worker worktree and `prepush`
for publishing, CI monitoring, landing, and cleanup.

## Establish control

1. Require the exact active GitHub login `yanchenko` for every DontSpeak read or
   write. `axy-yanchenko` is reserved for AMBOSS repositories and is prohibited
   here, regardless of its permission. Run `gh auth switch --hostname github.com
   --user yanchenko`, verify with `gh api user --jq .login`, and never print tokens.
2. Run `scripts/audit-issues.mjs --repo delllusional/DontSpeak` from a clean
   freshly based integration worktree. The script fails closed unless the active
   login is `yanchenko`. Record its timestamp, login, permission, main SHA, open
   pull requests, and open issues. Require `WRITE`, `MAINTAIN`, or `ADMIN`.
3. Inspect `git worktree list`, remote branches, pull requests, issue assignees, and
   recent issue comments. Exclude work already claimed or in progress. Ignore every
   issue linked, referenced as fixed, or otherwise covered by an open pull request,
   even when its files would not overlap the selected wave.
4. Create one dated `bugfix/issue-swarm-<date>-<nonce>` integration branch from
   refreshed `main`. The integrator alone owns it and repository administration.

## Select a wave

Rank open issues by value, clarity, risk, and independence. Treat their native
Type, Priority, and Effort as triage inputs, then verify those estimates against
the issue evidence. Prefer bugs affecting builds, correctness, reliability,
accessibility, or developer workflow when the expected behavior and validation
are explicit and the change is localized.

Reject an issue from autonomous execution when it needs any of:

- an already open pull request that links, fixes, or covers the issue;
- user, product, UX, compatibility, architecture, release, or policy choice;
- credentials, hardware, live services, destructive testing, or non-mocked network;
- unclear reproduction or acceptance criteria;
- overlap with active work, another selected issue, or likely files owned by the
  same subsystem;
- broad refactoring, dependency migration, security-sensitive behavior, or a large
  cross-platform contract change.

Select up to three issues whose likely files, crates, apps, tests, and build state
do not intersect. Prefer waves of two or three, but select a final singleton when it
is the only qualified issue; lack of a parallel partner is not an exclusion.
Summarize the reason, expected files, validation, and risk before dispatch. Assign
or comment only when useful to prevent duplicate work, using the verified GitHub
account.

## Dispatch parallel workers

Create one branch and worktree per issue from the integration batch base. One
writing agent owns each. Give every worker:

- the issue URL and raw issue body/comments;
- its branch, worktree, base SHA, and non-overlap boundary;
- instructions to inspect, plan, implement, test proportionately, commit with
  `Fixes #N`, and report SHA/checks;
- permission to fix only small obvious adjacent defects and otherwise file an
  issue under the repository policy;
- a prohibition on pushing, issue closure, integration, landing, cleanup,
  rebasing, stashing, resetting, or edits outside its worktree.

Run at least two implementation agents concurrently whenever the wave contains two
or more issues. For a singleton wave, use one implementation agent and a different
agent for independent read-only review. Keep one integration owner. Workers must
not share writable worktrees or machine-wide build outputs. Treat worker branches
as local staging branches; publish only the combined integration branch so one
exact CI head gates the batch.

## Cross-review and integrate

After implementation, have each worker review another worker's issue and exact
commit read-only. Review the issue fit, diff, tests, regression risk, scope, and
file overlap. Send findings back to the owning worker; require a new exact commit
and repeat review for material changes.

The integrator then:

1. Confirm every worker branch is clean, based on the recorded batch base,
   independently reviewed, and limited to its assigned scope.
2. Cherry-pick the reviewed commits onto the integration branch in the least
   dependency-sensitive order. Stop on ambiguity or conflicts; do not improvise a
   semantic resolution without re-review.
3. Run the narrow integration checks needed to catch combined-state failures.
4. Invoke `prepush` on the integration branch. Require successful terminal status
   for every applicable non-release check on its exact head.
5. Fast-forward that exact green head to `main`, push without force, and verify
   `origin/main` equals it. Do not repeat local tests after green CI.
6. Verify each `Fixes #N` issue closed only after the commit reached `main`; close
   manually with the main SHA only if GitHub did not.
7. Remove only clean, exact worker and integration worktrees and delete their exact
   local and remote branches.

## Refresh the queue

Rerun `scripts/audit-issues.mjs` after landing. Compare issue numbers and
`createdAt` values with the starting snapshot, call out issues opened during the
wave, and rank the safest next candidates.

When the user requests continuous or exhaustive operation, immediately start the
next wave while any qualified issue remains, including a final singleton. Take a
fresh live snapshot after every landing so issues created during the run enter the
same selection process. Stop only when zero open issues meet every selection
criterion. Report the remaining issues by exclusion reason: user decision, higher
risk, active work or open PR, unclear acceptance criteria, broad scope, or live
resource dependency. Never lower the selection bar merely to keep agents busy.
