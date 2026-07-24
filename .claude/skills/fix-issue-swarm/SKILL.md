---
name: fix-issue-swarm
description: Audit the DontSpeak GitHub issue queue, select several high-value low-risk issues that need no user input or product decision, fix non-overlapping issues with parallel agents in isolated branches and worktrees, integrate them on a batch bugfix branch, review and verify them, land the exact green batch on main, close the issues, and refresh the queue for the next wave. Use for autonomous issue triage, parallel bugfix batches, issue sweeps, or requests to repeatedly pick and land safe repository fixes.
---

# Fix an issue swarm

Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
[`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md). Use `high` effort or higher.
Use the `start-task` skill for every integration or worker worktree and `prepush`
for publishing, CI monitoring, landing, and cleanup.

## Establish control

1. Run `scripts/audit-issues.mjs --repo delllusional/DontSpeak` from a clean
   freshly based integration worktree. Record its timestamp, active GitHub login,
   permission, main SHA, open pull requests, and open issues.
2. Require `WRITE`, `MAINTAIN`, or `ADMIN`. If the active account lacks it, inspect
   `gh auth status`, select an authenticated account that has it, switch explicitly,
   and rerun the audit. Never print tokens.
3. Inspect `git worktree list`, remote branches, pull requests, issue assignees, and
   recent issue comments. Exclude work already claimed or in progress. Ignore every
   issue linked, referenced as fixed, or otherwise covered by an open pull request,
   even when its files would not overlap the selected wave.
4. Create one dated `bugfix/issue-swarm-<date>-<nonce>` integration branch from
   refreshed `main`. The integrator alone owns it and repository administration.

## Select a wave

Rank open issues by value, clarity, risk, and independence. Prefer bugs affecting
builds, correctness, reliability, accessibility, or developer workflow when the
expected behavior and validation are explicit and the change is localized.

Reject an issue from autonomous execution when it needs any of:

- an already open pull request that links, fixes, or covers the issue;
- user, product, UX, compatibility, architecture, release, or policy choice;
- credentials, hardware, live services, destructive testing, or non-mocked network;
- unclear reproduction or acceptance criteria;
- overlap with active work, another selected issue, or likely files owned by the
  same subsystem;
- broad refactoring, dependency migration, security-sensitive behavior, or a large
  cross-platform contract change.

Select two or three issues whose likely files, crates, apps, tests, and build state
do not intersect. Summarize the reason, expected files, validation, and risk before
dispatch. Assign or comment only when useful to prevent duplicate work, using the
verified GitHub account.

## Dispatch parallel workers

Create one branch and worktree per issue from the integration batch base. One
writing agent owns each. Give every worker:

- the issue URL and raw issue body/comments;
- its branch, worktree, base SHA, and non-overlap boundary;
- instructions to inspect, plan, implement, test proportionately, commit with
  `Fixes #N`, push, and report SHA/checks;
- permission to fix only small obvious adjacent defects and otherwise file an
  issue under the repository policy;
- a prohibition on issue closure, integration, landing, cleanup, rebasing,
  stashing, resetting, or edits outside its worktree.

Run at least two implementation agents concurrently. Keep one integration owner.
Workers must not share writable worktrees or machine-wide build outputs.

## Cross-review and integrate

After implementation, have each worker review another worker's issue and exact
commit read-only. Review the issue fit, diff, tests, regression risk, scope, and
file overlap. Send findings back to the owning worker; require a new exact commit
and repeat review for material changes.

The integrator then:

1. Confirm every worker branch is clean, pushed, based on the recorded batch base,
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

When the user requested continuous autonomous waves, immediately start the next
wave if at least two non-overlapping qualified issues remain. Otherwise stop and
report why the remaining issues require input, carry higher risk, overlap active
work, or lack a parallel partner. Never lower the selection bar merely to keep
agents busy.
