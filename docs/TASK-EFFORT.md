# Task effort

This file is the single source of truth for matching reasoning effort to repository
work across coding agents.

## Portable rule

Before starting a task or a materially different phase of one:

1. Check whether the user, launcher, session, or handoff explicitly selected an
   effort level. An explicit choice wins; never silently lower it to save time or
   tokens.
2. Classify the work using the guidance below. If the current effort is visible,
   compare it with the task. If it is not visible, do not claim to know or have
   changed it.
3. When the current level is below the task's recommended minimum, use a supported
   host control only when that control is available to the agent and changing it is
   authorized. If the user explicitly chose the lower level, report the mismatch
   but do not override it without consent. Otherwise tell the user which level is
   recommended. Do not invent a slash command, frontmatter field, or configuration
   key for a host that does not support it.
4. When a named level is unsupported, use the nearest supported level, preferring
   one at or above the recommendation and otherwise using the highest available. If
   only an on/off thinking control exists, enable thinking for medium-or-higher work.
5. Do not reduce required tests, review, research, or safety checks because effort
   is low. Workflow gates are independent of model effort.

Do not change effort merely because a later step looks easier. Preserve an explicit
or already-suitable session level through the task; suggest a cheaper level for a
future task instead of silently downshifting the current one.

## Recommended minimums

| Work | Minimum effort |
| --- | --- |
| Short, bounded, read-only lookup or deterministic mechanical edit | `low` |
| Routine documentation, build, test, formatting, or narrowly scoped maintenance | `medium` |
| Bug diagnosis and fixes, substantive implementation, code or plan review, pre-push verification, release, landing, and multi-file changes | `high` |
| Ambiguous architecture, difficult root-cause analysis, security-sensitive or safety-critical work, complex migrations, and novel cross-system design | `xhigh`, or the highest supported level |

Use `max` only for exceptional problems where the host documents it and the expected
benefit justifies extra cost and latency. It can produce diminishing returns or
overthinking; it is not a routine default.

## Why the policy is instruction-based

The supported control surface differs by host, so no single skill field or command
is portable:

- Codex exposes model and plan-mode reasoning-effort configuration.
- Claude Code exposes session controls and supports an `effort` override in skill
  and subagent frontmatter.
- Gemini CLI exposes model-specific thinking configuration.
- Qwen Code exposes a unified `/effort` control that maps to each provider.
- Grok exposes interactive and command-line effort controls.

The shared skill therefore describes the decision and leaves the actual setting to
a verified host capability. See [`AGENT-SKILLS.md`](AGENT-SKILLS.md) for current
official compatibility references.
