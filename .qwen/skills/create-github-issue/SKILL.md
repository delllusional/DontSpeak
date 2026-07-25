---
name: create-github-issue
description: Create structured DontSpeak GitHub issues with the native issue Type and organization Priority and Effort fields, without labels. Use whenever the user asks to create/file/open an issue or ticket, and whenever repository work discovers an out-of-scope bug, enhancement, documentation gap, investigation, or follow-up that must be recorded.
---

# Create a GitHub issue

Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
[`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

## Triage

Search open issues before filing and reuse an existing issue when its scope and
acceptance criteria overlap. Write a concise title and a body containing:

- observed or requested behavior and why it matters;
- concrete acceptance criteria;
- relevant evidence, paths, or reproduction details;
- an `Estimate` section naming affected subsystems, expected validation, and risk.

Select the native issue Type:

- `Bug`: incorrect, unexpected, or regressed behavior;
- `Feature`: new user-visible behavior or capability;
- `Task`: maintenance, documentation, investigation, refactoring, or process work.

Estimate Effort from implementation scope, validation, and risk:

- `Low`: localized, routine change in one subsystem;
- `Medium`: coordinated change across a few components or a nontrivial integration;
- `High`: architectural, cross-platform, migration, release-sensitive, or broadly
  coupled work.

Select Priority independently from effort:

- `Urgent`: security, data loss, release blocker, or primary path unusable;
- `High`: significant or broad impact without a practical workaround;
- `Medium`: meaningful but limited impact, or a practical workaround exists;
- `Low`: polish, cleanup, or optional follow-up.

## File

Write the body to a task-local file, then run:

```bash
node .agents/skills/create-github-issue/scripts/create-issue.mjs \
  --repo delllusional/DontSpeak \
  --title "<title>" \
  --body-file "<path>" \
  --type Bug \
  --priority High \
  --effort Medium
```

Do not pass labels. The script verifies the active GitHub identity, repository
permission, enabled issue type, and live organization field options before it
creates anything. Use `--dry-run` to validate the selection without creating an
issue.

After creation, read the issue back and confirm its Type, Priority, Effort, title,
and body. Report its number and URL. If metadata assignment fails after creation,
preserve the issue, report the partial result, and complete its fields before
continuing.
