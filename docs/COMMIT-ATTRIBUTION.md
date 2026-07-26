# Commit attribution

Canonical rules for DontSpeak commit attribution. Point here; don't copy.

End every commit with one `Agent:` trailer — no `Co-Authored-By`, `Assisted-by`,
`Generated-by`, or other AI lines:

```text
Agent: <model-id> <effort-level>
```

Full model slug + named effort (e.g. `Agent: claude-sonnet-5 xhigh`,
`Agent: gpt-5.6-sol xhigh`). Hooks capture both at commit time; the private
`commit-msg` hook rewrites a lone hand-written trailer to those values. Never guess
from self-description, UI family label, or defaults. `unknown` / `default` / prose
are not effort levels.

Trust the project hook per client so it can install the private Git hook:
Claude → `.claude/settings.json`; Grok → that file **and**
`.grok/hooks/commit-attribution.json`; Codex → `.codex/hooks.json`; Qwen →
`.qwen/settings.json`. Missing model or effort **blocks** the commit (CI can check
shape later but can't recover which runtime produced it).

Capture sources:

- **Codex** — hook model slug plus matching turn context, or the same turn
  context when the current transcript's latest unfinished `exec_command` is the
  commit for this exact worktree.
- **Claude** — transcript for model; tool hooks for applied effort.
- **Qwen** — transcript for model; settings for `/effort` (no separate post-provider field).
- **Grok** — session model + effort (`summary.reasoning_effort` / chat turns /
  `~/.grok/active_sessions.json`). `none` only when the catalog says effort is
  unsupported. Prefer parent sessions over plan/implement subagents.

Claude auto-`Co-Authored-By` is disabled via `.claude/settings.json` `attribution`.
If Codex/Qwen still emit auto-attribution, the commit hook strips it.

Hook mechanics:

- PreToolUse capture is best-effort. When it is absent, `commit-msg` asks the active
  client's resolver to prove the same model and effort from its session store. The
  commit remains blocked when either value is unavailable.
- Codex project-hook trust is path-scoped, so a newly created managed worktree can
  skip an otherwise trusted checkout hook. The fallback accepts only the latest
  unfinished `exec_command` in the exact active session when it targets the
  committing worktree; completed or unrelated calls fail closed.
- Codex effort is accepted only from the `turn_context` whose session, turn ID, and
  model match the hook payload. Captures are keyed by session in the repository's
  common Git directory so the same worker can commit from a linked worktree without
  sharing metadata with another concurrent session.
- The hook can attribute merge commits found in existing history, but the task
  workflow prohibits creating them; see `docs/TASK-BASELINE.md`.
- `--amend` preserves the existing pair (appending the amending pair if it
  differs) only when the amended message is identical to `HEAD`'s (`--no-edit`,
  unedited editor, reused message); a changed message gets rewrite-lone
  semantics. Accepted residuals: an edited amend drops the prior lone pair;
  committing a byte-identical message inherits `HEAD`'s proven pair.
- Terminal commits with no agent runtime use `Agent: human none`.
- A capture stays valid for 15 minutes under an agent environment; commits made
  without one (human terminal) only inherit captures younger than 5 minutes —
  accepted residual window.

## Squashing

On squash/rebase: keep every distinct `Agent: <model-id> <effort-level>` pair from
combined commits (one line each). Don't drop non-final contributors; don't duplicate
pairs.
