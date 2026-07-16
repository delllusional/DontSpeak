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
Claude/Grok → `.claude/settings.json`; Codex → `.codex/hooks.json`; Qwen →
`.qwen/settings.json`. Missing model or effort **blocks** the commit (CI can check
shape later but can't recover which runtime produced it).

Capture sources:

- **Codex** — hook model slug; turn context for effort.
- **Claude** — transcript for model; tool hooks for applied effort.
- **Qwen** — transcript for model; settings for `/effort` (no separate post-provider field).
- **Grok** — session model + effort. `none` only when the catalog says effort is unsupported.

Claude auto-`Co-Authored-By` is disabled via `.claude/settings.json` `attribution`.
If Codex/Qwen still emit auto-attribution, the commit hook strips it.

## Squashing

On squash/rebase: keep every distinct `Agent: <model-id> <effort-level>` pair from
combined commits (one line each). Don't drop non-final contributors; don't duplicate
pairs.
