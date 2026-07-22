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

- **Codex** — hook model slug; turn context for effort. When a tool surface skips
  `PreToolUse`, **`commit-msg` live-resolves** both values from the active transcript
  under `~/.codex/sessions` using `CODEX_THREAD_ID`.
- **Claude** — transcript for model; tool hooks for applied effort.
- **Qwen** — transcript for model; settings for `/effort` (no separate post-provider field).
- **Grok** — session model + effort (`summary.reasoning_effort` / chat turns /
  `~/.grok/active_sessions.json`). `none` only when the catalog says effort is
  unsupported. Tool shells often set only `GROK_AGENT` (no `GROK_SESSION_ID`).
  PreToolUse capture is best-effort; **`commit-msg` also live-resolves** from the
  Grok session store when the cache is missing so trailers stay correct without
  a prior capture. Prefer parent sessions over plan/implement subagents.

Claude auto-`Co-Authored-By` is disabled via `.claude/settings.json` `attribution`.
If Codex/Qwen still emit auto-attribution, the commit hook strips it.

Hook mechanics:

- Merge commits (`git merge`) are captured and stamped like regular commits.
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
