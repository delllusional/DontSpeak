# Commit attribution

This file is the single source of truth for DontSpeak commit attribution. Agent
instructions and push workflows must reference it instead of copying its rules.

End every commit message with a single `Agent:` trailer instead of any built-in
AI-attribution line — no `Co-Authored-By`, `Assisted-by`, `Generated-by`, or similar,
and no other attribution beyond this one line. Format:

```text
Agent: <model-id> <effort-level>
```

Use the full active model slug for every provider, not a family shorthand or generic
product name. Examples: `Agent: claude-sonnet-5 xhigh` and
`Agent: gpt-5.6-sol xhigh`. The repository hooks capture the active model and the
CLI's named reasoning-effort level immediately before `git commit`; the private
`commit-msg` hook then replaces a lone hand-written trailer with those values. Do not
guess from the model's self-description, the UI's family label, or a configured default.
`unknown`, `default`, and explanatory prose are not effort levels.

The project hook must be trusted in each client before it can install the private Git
hook for that worktree. Claude Code and Grok load `.claude/settings.json`, Codex loads
`.codex/hooks.json`, and Qwen Code loads `.qwen/settings.json`. If either runtime value
is unavailable, the commit is blocked; select an explicit model and effort in the CLI
and retry. This fail-closed behavior is deliberate because CI can validate the trailer's
shape after the fact, but cannot reconstruct which runtime produced a commit.

The capture sources are client-specific:

- Codex supplies the exact model slug to hooks; the current turn context supplies effort.
- Claude Code supplies applied effort to tool hooks; its current transcript supplies model.
- Qwen Code's current transcript supplies model and its settings supply the persisted
  `/effort` selection. Qwen's hook payload does not expose a separate post-provider value.
- Grok's current session supplies model and effort. `none` is used only when the model
  catalog explicitly says reasoning effort is unsupported; otherwise absent effort blocks.

Claude Code's own automatic `Co-Authored-By` trailer is disabled repo-wide via
`.claude/settings.json`'s `attribution` key so it can't duplicate this line. Codex and
Qwen Code don't read that file, so if either tool's automatic attribution can't be
suppressed from here, the commit hook removes it before finishing the commit message.

## Squashing

When combining several commits into one (interactive rebase or squash-merge), carry
forward every distinct `Agent: <model-id> <effort-level>` pair from the commits being
combined — one line per distinct pair, inherited into the result. Don't drop a
contributor's line just because its commit wasn't the last one before the squash;
don't repeat a pair that already appears.
