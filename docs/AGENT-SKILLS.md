# Agent and skill portability

How DontSpeak shares instructions and Agent Skills across CLIs.

## Repository instructions

`AGENTS.md` is canonical.

| CLI | How it loads |
| --- | --- |
| Codex, Qwen Code, Grok Build | Read `AGENTS.md` directly |
| Claude Code | `@AGENTS.md` in `CLAUDE.md` (+ Claude-only extras there) |
| Gemini CLI | `@AGENTS.md` in `GEMINI.md` |
| Older Qwen | `@AGENTS.md` in `QWEN.md` (current Qwen also reads `AGENTS.md`) |

Edit shared rules in `AGENTS.md` only. Wrappers hold vendor-specific content.

## Agent Skills

Canonical tree: `.agents/skills/` (open Agent Skills format).

| CLI | Discovery path |
| --- | --- |
| Codex | `.agents/skills/` |
| Gemini CLI | `.agents/skills/` (compat alias) |
| Claude Code | `.claude/skills/` |
| Grok Build | `.agents/skills/` (also accepts `.claude/skills/`) |
| Qwen Code | `.qwen/skills/` |

`.claude/skills/` and `.qwen/skills/` are **generated mirrors** (no git symlinks —
Windows checkouts break them without Developer Mode + `core.symlinks`).

After editing `.agents/skills/`:

```bash
node scripts/agents/sync-agent-skills.mjs
node scripts/agents/sync-agent-skills.mjs --check   # before push
```

Claude subagents/workflows stay under `.claude/agents/` and `.claude/workflows/`
(no portable discovery).

Skills that plan/check/review/build/test/implement/audit/release/land must apply
[TASK-BASELINE.md](TASK-BASELINE.md) and [TASK-EFFORT.md](TASK-EFFORT.md) — link them,
don't copy.

## Compatibility references

- [Codex skills](https://learn.chatgpt.com/docs/build-skills) ·
  [effort config](https://developers.openai.com/codex/config-reference)
- [Claude skills](https://code.claude.com/docs/en/skills) ·
  [imports](https://code.claude.com/docs/en/memory#import-additional-files) ·
  [effort](https://code.claude.com/docs/en/model-config#adjust-effort-level)
- [Gemini skills](https://geminicli.com/docs/cli/using-agent-skills/) ·
  [imports](https://geminicli.com/docs/cli/gemini-md/#modularize-context-with-imports) ·
  [thinking](https://geminicli.com/docs/get-started/configuration-v1/)
- [Qwen skills](https://qwenlm.github.io/qwen-code-docs/en/users/features/skills/) ·
  [memory](https://qwenlm.github.io/qwen-code-docs/en/users/features/memory/) ·
  [effort](https://qwenlm.github.io/qwen-code-docs/en/design/2026-06-30-unified-reasoning-effort-cli/)
- [Grok skills](https://docs.x.ai/build/features/skills-plugins-marketplaces) ·
  [effort](https://docs.x.ai/build/modes-and-commands)
