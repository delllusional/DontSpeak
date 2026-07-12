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
`Agent: gpt-5.6-sol xhigh`. State your own model id and the current session's
reasoning-effort level (Claude Code: read `$CLAUDE_EFFORT`) — don't omit the field if
the effort level is unclear; give your best description instead.

Claude Code's own automatic `Co-Authored-By` trailer is disabled repo-wide via
`.claude/settings.json`'s `attribution` key so it can't duplicate this line. Codex and
Qwen Code don't read that file, so if either tool's automatic attribution can't be
suppressed from here, drop it manually before finishing the commit message.

## Squashing

When combining several commits into one (interactive rebase or squash-merge), carry
forward every distinct `Agent: <model-id> <effort-level>` pair from the commits being
combined — one line per distinct pair, inherited into the result. Don't drop a
contributor's line just because its commit wasn't the last one before the squash;
don't repeat a pair that already appears.
