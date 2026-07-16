# Task effort

Match reasoning effort to the work. Portable across hosts.

## Rule

Before a task or a new phase:

1. Explicit user/launcher/session effort wins — never silently lower it.
2. Classify with the table below. If current effort isn't visible, don't claim it.
3. Below recommended minimum: raise via a supported host control when authorized;
   if the user chose lower, report mismatch and don't override. Don't invent host
   knobs that don't exist.
4. Unsupported named level → nearest supported (≥ recommendation, else highest).
   On/off thinking only → enable for medium+.
5. Low effort never skips required tests, review, research, or safety checks.

Don't downshift mid-task because a later step looks easier. Suggest cheaper effort
for a *future* task instead.

## Recommended minimums

| Work | Minimum |
| --- | --- |
| Short read-only lookup or mechanical edit | `low` |
| Docs, build, test, format, narrow maintenance | `medium` |
| Bugs, real implementation, review, prepush, release, land, multi-file | `high` |
| Ambiguous architecture, hard RCA, security/safety, complex migration, novel design | `xhigh` (or host max) |

`max` only when the host documents it and cost is justified — not a default.

## Why instruction-based

Hosts differ (Codex effort config, Claude skill/subagent frontmatter, Gemini thinking,
Qwen `/effort`, Grok CLI/interactive). Shared skills describe the decision; setting
is host-specific. See [AGENT-SKILLS.md](AGENT-SKILLS.md).
