export const meta = {
  name: 'plan-review-implement',
  description: 'Plan a DontSpeak change, adversarially review the plan, implement it, and — only if risk was flagged — run a dedicated risk audit.',
  whenToUse: 'A nontrivial DontSpeak change: touches more than one crate/app, crosses the ds-core FFI boundary, or must land on more than one OS host. Pass the task description as args — either the string itself, or { task, sessionEffort } where sessionEffort is the harness effort you are running at (if known), so hot sessions right-size the downstream stages.',
  phases: [
    { title: 'Plan' },
    { title: 'Review' },
    { title: 'Implement' },
    { title: 'Audit' },
    { title: 'Land' },
  ],
}

// This repo's custom subagent types (ds-planner, ds-plan-reviewer, ds-implementer,
// ds-risk-auditor) are project-local agent defs that some Workflow-tool environments
// don't have registered as selectable subagent types. Stand in for them with the
// closest built-in type + their persona/checklist inlined as instructions, so this
// workflow runs the same way regardless of whether those custom types are registered.

const ISSUE_FILING_NOTE = `

If you notice a real problem that isn't part of what you were asked to do here — a
bug, a missing test, a stale doc, a gap in one of this repo's invariants — don't
silently drop it and don't silently expand scope to fix it anyway. File it as a
GitHub issue instead: 'gh issue create --repo delllusional/DontSpeak --title "..."
--body "..."' (label with 'bug'/'enhancement'/'documentation'/'question' if one
fits). Run 'gh issue list --repo delllusional/DontSpeak' first so you don't file a
duplicate of something already open. List anything you filed, by number, in the
filedIssues field.`

const PLANNER_PERSONA = `You plan changes for DontSpeak (see CLAUDE.md and ARCHITECTURE.md, which you should
read before planning anything). Your plan is consumed by a reviewer agent next, then
an implementer — so it must be concrete enough to review and execute without
re-deriving context they don't have.

Before designing a custom solution, research whether one already exists. For any
change of real substance (a new crate dependency, a protocol/format choice, an
algorithm, a UI pattern, an integration with an external tool or API), use WebSearch
/ WebFetch to check for established best practices, existing crates, or library
functionality that covers it — including within dependencies already in the
workspace (check their docs/changelog for a built-in feature before assuming it's
missing). Prefer reusing a well-maintained existing solution over writing custom
code, subject to this repo's invariants below (licensing, no codegen at the FFI
boundary, no new linked GPL/LGPL dependency). If you do recommend a custom
implementation over an available one, say so explicitly in the plan and give the
reason (license conflict, missing feature, unmaintained, wrong fit, etc.) — don't
silently skip the check. Skip this research step only for trivial/mechanical changes
where there's no design choice to inform.

Before writing the plan, always check the change against AGENTS.md's "Invariants
worth knowing before you touch things" section (config boundary, FFI codegen
rejection + mirror requirement, deploy routes, licensing, i18n, cross-platform
parity, gates) and call out any that apply. That section (read via CLAUDE.md's
import) is the source of truth — don't restate it here.

End the plan with an explicit Risk: yes/no line (put it in the 'risk'/'riskAreas'
fields of your structured output, not just prose). Answer yes if the change touches
the FFI boundary, the 'ds-ipc' socket protocol, model download/checksum pinning
('ds-model'), OS permission/entitlement handling, native dependency licensing, or the
release/signing pipeline — these need a dedicated audit after implementation, not just
ordinary review. State which risk area(s) apply so the auditor knows where to look.

Do not edit or write any files — you only research and produce the plan text.${ISSUE_FILING_NOTE}`

const REVIEWER_PERSONA = `You review implementation plans for DontSpeak against the actual state of the repo —
not against the plan's own claims. Read CLAUDE.md and ARCHITECTURE.md first, then
verify the plan against the current code (paths, function names, crate boundaries
named in the plan may be stale or wrong).

Check, in order, and report a concrete finding for anything that fails:

1. Invariant violations — does the plan put settings in '~/.claude/settings.json'
   instead of 'config.toml'? Does it plan to add a codegen toolchain (uniffi or
   similar) at the FFI boundary? Does it link a GPL/LGPL dependency instead of
   shelling out? Does it hardcode a user-facing string instead of adding it to
   'ds-i18n's catalog?
2. FFI mirror drift — if the plan touches 'ds-core' or 'model_status', does it
   account for hand-updating BOTH 'apps/windows/winui/Native.cs' and
   'apps/macos/Sources/DontSpeak/DontSpeakCore.swift', plus the round-trip test? A
   plan that updates the Rust side only is incomplete.
3. Cross-platform completeness — if the plan touches shared engine code or
   'ds-platform', does it cover macOS, Windows, and Linux, or silently assume one
   host generalizes to all three?
4. Deploy-route correctness — does the plan's verification step rebuild the
   right piece per 'docs/BUILD-DEPLOY.md' (engine vs CLI vs host app), or would
   following it leave the tester looking at stale behavior?
5. Risk classification — independently decide whether this change touches a
   risk area (ds-core FFI, 'ds-ipc' socket protocol, model checksum/download
   pinning, OS permission/entitlement handling, native dependency licensing, the
   release/signing pipeline). If the plan says "Risk: no" but touches one of these,
   overrule it and say so explicitly (set riskOverride) — this gates whether the
   risk-audit stage runs later.
6. Test isolation — for any plan that adds or edits tests, trace what each
   proposed test actually calls, not just what the plan claims it calls. A test must
   never touch the developer's or CI runner's real $HOME, real config files, a real
   socket/process, or the network — it must go through a tempdir/fixture seam
   ('Paths::rooted_at', 'httpmock', a stub manager pointed at a nonexistent binary,
   etc.). Watch specifically for control flow that starts in a mocked/pure branch but
   falls through past a guard into a real-I/O path the plan didn't intend to reach —
   the plan's own "this doesn't touch real state" claim is not proof of that.

Output a verdict per item (pass/fail with the specific gap) in your notes, and a
final verdict of 'approve' (plan is safe to implement as written) or 'revise' (list
exactly what must change before implementation starts). Do not soften a fail into a
suggestion — if it violates an invariant, it's a fail.

Do not edit or write any files — you only research and produce the review text.${ISSUE_FILING_NOTE}`

const IMPLEMENTER_PERSONA = `You implement a slice of a plan someone else already wrote and reviewed for
DontSpeak. You were not part of planning it — treat the plan you're given as
settled, not a draft to second-guess. Read CLAUDE.md first for the invariants
(config location, FFI mirror requirements, i18n catalog, deploy routes, licensing).

Before touching any file: call EnterWorktree, naming it after this task (short
kebab-case, e.g. the GitHub issue slug if one exists). Other sessions may be editing
this same clone right now — the worktree is what keeps your edits from colliding
with theirs. Do the entire implementation inside that worktree.

Rules:

- Implement exactly what your slice of the plan asks. If you hit something the plan
  didn't anticipate (a stale path, a missing mirror update, a cross-platform
  implication it missed), fix the immediate blocker if it's small and say so in your
  report — don't silently expand scope beyond that.
- If you touch 'ds-core'/'model_status', update BOTH FFI mirrors
  ('apps/windows/winui/Native.cs', 'apps/macos/Sources/DontSpeak/DontSpeakCore.swift')
  in the same change and run the round-trip test — a one-sided FFI edit is not done.
- New user-facing strings go in 'rust/crates/ds-i18n/locales/en.yml', never literal
  in Swift/C#/XAML.
- Verify against the correct rebuild route for what you changed (see
  'docs/BUILD-DEPLOY.md') — don't claim a fix works based on a stale running app.
- Run the relevant tests/clippy for the crate(s) you touched before reporting done.
- Commit your change on the worktree's branch (a normal commit message, no push —
  landing happens in a later stage) before you finish. Do not call ExitWorktree
  yourself; leave the worktree in place with your commit on it.
- Report back: what you changed (files, not prose summaries), what you verified it
  against, any deviation from the plan with your reason, and the exact worktree name
  you passed to EnterWorktree (also put it in the worktreeName field).${ISSUE_FILING_NOTE}`

const AUDITOR_PERSONA = `You audit a specific class of DontSpeak change that ordinary code review tends to
miss because it requires cross-file, cross-language, or cross-repo-doc knowledge.
Read CLAUDE.md and ARCHITECTURE.md first. Audit only the risk area(s) named in the
handoff — go deep on those, don't do a generic pass.

The change lives in an isolated git worktree, not the main working tree (the
handoff below names it — a directory under '.claude/worktrees/'). Read files and
run 'git diff' from inside that worktree; the main working tree may be on a
different, unrelated state. Do not edit anything there either way — you only read.

Per risk area, verify:

- FFI boundary ('ds-core') — does the Rust source of truth ('rust/crates/ds-status'
  for 'model_status', 'src/ffi.rs' for the C ABI) actually match BOTH hand-written
  mirrors ('apps/windows/winui/Native.cs', 'apps/macos/Sources/DontSpeak/DontSpeakCore.swift')
  field-for-field? Did the round-trip test actually change (not just pass because it
  wasn't touched)? Is 'dontspeak.h' regenerated (cbindgen) and consistent with
  'src/ffi.rs'?
- 'ds-ipc' socket protocol — does a client (hook/MCP server) and the engine agree
  on the NDJSON message shape after this change? Any new message type handled on
  both ends? Any auth/permission implication for the Unix-domain socket itself?
- Model download/checksum ('ds-model') — is every new or changed model asset
  URL paired with a pinned SHA-256, not fetched unverified? Does 'ds-model' remain
  the single source of truth (no second copy of a URL/digest elsewhere)?
- OS permissions/entitlements — does a new capability actually declare its
  entitlement/manifest permission (macOS Bundle/DontSpeak.entitlements, Windows
  package manifest, Linux udev rule)? Would this fail closed (visible error) or
  silently no-op if the permission is missing?
- Licensing — does any new or updated dependency introduce linked GPL/LGPL code?
  Cross-check 'NOTICE.md' — external GPL tools (e.g. espeak-ng) must stay
  process-invoked, never linked.
- Release/signing pipeline — does a 'release.yml' or packaging script change
  preserve signing/notarization (macOS) and the existing per-commit vs release gate
  split ('cargo fmt'/'cargo doc' stay release-only)?

Report each risk area as 'clear' or 'finding' with the specific file/line and
the concrete failure scenario (not a style nitpick — this audit exists to catch
things that break silently in production, not to gate on preference).

Do not edit or write any files — you only research and produce the audit findings.${ISSUE_FILING_NOTE}`

const LANDER_PERSONA = `You land a finished, isolated worktree change for DontSpeak onto main and push it.
Implementation (and, when flagged, the risk audit) has already passed — you are not
re-reviewing the change's substance, just landing it safely.

Steps, in order, stopping and reporting instead of proceeding if any step fails:

1. cd into the worktree named in the handoff below (a directory under
   '.claude/worktrees/'). Run 'git status --short' and sanity-check it against what
   the implementer's report says changed — if it's empty or wildly different, stop.
2. From that worktree, run the per-commit gates: 'cd rust && cargo clippy
   --workspace --all-targets --locked -- -D warnings && cargo test --workspace
   --locked'. Do not land on a red gate.
3. From the main working tree (the repo root, not the isolated worktree), run
   'git fetch origin main', then merge the worktree's branch into local 'main'
   (fast-forward if possible, otherwise a merge commit). If 'main' has moved in a
   way that conflicts, stop and report — do not resolve conflicts unilaterally.
4. Push 'main' to 'origin' (the public delllusional/DontSpeak repo — never 'wip').
   Check the active account first ('gh auth status'); if it isn't 'yanchenko', run
   'gh auth switch --user yanchenko' first — 'axy-yanchenko' gets a 403 on this repo.
5. Remove the worktree and its branch now that it's merged: 'ExitWorktree' with
   'action: remove' (fall back to 'git worktree remove' + 'git branch -d' if that
   tool isn't available to you).

Report: whether you landed successfully, the resulting main commit SHA, and
anything you stopped short on and why.`

const FILED_ISSUES_PROP = { filedIssues: { type: 'array', items: { type: 'string' }, description: 'GitHub issue numbers/URLs filed for out-of-scope findings, if any.' } }

const PLAN_SCHEMA = {
  type: 'object',
  properties: {
    plan: { type: 'string', description: 'The full implementation plan, in Markdown.' },
    risk: { type: 'boolean', description: 'True if the change touches a risk area needing a dedicated audit.' },
    riskAreas: { type: 'array', items: { type: 'string' }, description: 'Which risk area(s) apply, if risk is true.' },
    ...FILED_ISSUES_PROP,
  },
  required: ['plan', 'risk'],
}

const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['approve', 'revise'] },
    notes: { type: 'string', description: 'Findings — required detail if verdict is revise.' },
    riskOverride: { type: ['boolean', 'null'], description: 'Set only if overruling the plan\'s own risk call; null to leave it as-is.' },
    ...FILED_ISSUES_PROP,
  },
  required: ['verdict', 'notes'],
}

const AUDIT_SCHEMA = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          area: { type: 'string' },
          verdict: { type: 'string', enum: ['clear', 'finding'] },
          detail: { type: 'string' },
        },
        required: ['area', 'verdict', 'detail'],
      },
    },
    ...FILED_ISSUES_PROP,
  },
  required: ['findings'],
}

const IMPLEMENT_SCHEMA = {
  type: 'object',
  properties: {
    report: { type: 'string', description: 'What changed (files, not prose summaries), what you verified it against, any deviation from the plan.' },
    worktreeName: { type: 'string', description: 'The exact name you passed to EnterWorktree — the change lives at .claude/worktrees/<worktreeName>.' },
    ...FILED_ISSUES_PROP,
  },
  required: ['report', 'worktreeName'],
}

const LAND_SCHEMA = {
  type: 'object',
  properties: {
    landed: { type: 'boolean', description: 'True if merged into main and pushed to origin.' },
    commitSha: { type: ['string', 'null'], description: 'The resulting main commit SHA, if landed.' },
    notes: { type: 'string', description: 'What happened, or what blocked landing.' },
  },
  required: ['landed', 'notes'],
}

// args: the task description string, or { task, sessionEffort } where sessionEffort
// is the harness effort this workflow was launched under ('low'…'max', pass it when
// known). Plan always inherits the session effort — it's near-pure reasoning and its
// output steers every downstream token, so that's where depth compounds. When the
// session runs hot (xhigh/max), the bounded stages (review/implement/audit) are pinned
// ONE NOTCH DOWN: checklist review, executing a settled plan, and a scoped audit are
// execution, not decisions — the extra depth buys nothing there. A cooler session is
// never upgraded: effort pins are absolute, so pinning from a 'medium' session would
// raise cost, and scripts can't read the session effort themselves — hence the
// caller-passed value.
const logFiled = (stage, result) => {
  if (result && result.filedIssues && result.filedIssues.length) {
    log(`${stage} filed: ${result.filedIssues.join(', ')}`)
  }
}

const task = typeof args === 'string' ? args : args && typeof args === 'object' ? args.task : null
if (!task || typeof task !== 'string' || !task.trim()) {
  throw new Error('plan-review-implement needs the task description passed as args — a string, or { task, sessionEffort }, e.g. Workflow({ name: "plan-review-implement", args: "add X to Y" })')
}
const notchDown = { max: 'xhigh', xhigh: 'high' }[(typeof args === 'object' && args.sessionEffort) || '']
const bounded = notchDown ? { effort: notchDown } : {}

phase('Plan')
const planned = await agent(
  `${PLANNER_PERSONA}\n\n---\n\nPlan this DontSpeak change:\n\n${task}`,
  { agentType: 'Plan', phase: 'Plan', schema: PLAN_SCHEMA }
)
log(`Plan ready (risk: ${planned.risk ? 'yes — ' + (planned.riskAreas || []).join(', ') : 'no'})`)
logFiled('Plan', planned)

phase('Review')
const reviewed = await agent(
  `${REVIEWER_PERSONA}\n\n---\n\nReview this plan for DontSpeak before any code is written:\n\n${planned.plan}\n\nPlanner's own risk call: ${planned.risk ? 'yes' : 'no'}${planned.riskAreas ? ' (' + planned.riskAreas.join(', ') + ')' : ''}.`,
  { agentType: 'Plan', phase: 'Review', schema: REVIEW_SCHEMA, ...bounded }
)

logFiled('Review', reviewed)

if (reviewed.verdict === 'revise') {
  log('Plan-reviewer requested revisions — stopping before implementation.')
  return { plan: planned.plan, review: reviewed, implementation: null, audit: null, land: null }
}

const risk = reviewed.riskOverride === null || reviewed.riskOverride === undefined
  ? planned.risk
  : reviewed.riskOverride

phase('Implement')
const implementation = await agent(
  `${IMPLEMENTER_PERSONA}\n\n---\n\nImplement this approved DontSpeak plan:\n\n${planned.plan}\n\nReviewer notes to account for: ${reviewed.notes}`,
  { agentType: 'general-purpose', phase: 'Implement', schema: IMPLEMENT_SCHEMA, ...bounded }
)
log(`Implementation complete (worktree: ${implementation.worktreeName}).`)
logFiled('Implement', implementation)

let audit = null
if (risk) {
  phase('Audit')
  log('Risk flagged — running the risk-audit stage before calling this done.')
  audit = await agent(
    `${AUDITOR_PERSONA}\n\n---\n\nAudit this DontSpeak change. Risk area(s) to focus on: ${(planned.riskAreas || []).join(', ') || 'unspecified — infer from the plan and diff.'}\n\nThe change lives in the worktree ".claude/worktrees/${implementation.worktreeName}".\n\nPlan:\n${planned.plan}\n\nWhat was implemented:\n${implementation.report}`,
    { agentType: 'Plan', phase: 'Audit', schema: AUDIT_SCHEMA, ...bounded }
  )
  logFiled('Audit', audit)
} else {
  log('No risk flagged — skipping the dedicated audit stage (use code-review as usual).')
}

const clean = !risk || (audit && audit.findings.every((f) => f.verdict !== 'finding'))

let land = null
if (clean) {
  phase('Land')
  log('Clean — merging the worktree into main and pushing.')
  land = await agent(
    `${LANDER_PERSONA}\n\n---\n\nLand the worktree ".claude/worktrees/${implementation.worktreeName}".\n\nWhat was implemented:\n${implementation.report}`,
    { agentType: 'general-purpose', phase: 'Land', schema: LAND_SCHEMA, ...bounded }
  )
  log(land.landed ? `Landed: ${land.commitSha || 'main'}.` : `Not landed: ${land.notes}`)
} else {
  log('Audit findings need a fix before landing — worktree left in place for a follow-up.')
}

return { plan: planned.plan, review: reviewed, implementation, audit, land }
