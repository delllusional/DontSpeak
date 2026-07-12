# Agent and skill portability

This file is the single source of truth for how DontSpeak shares repository
instructions and Agent Skills across terminal coding agents.

## Repository instructions

`AGENTS.md` is the canonical repository instruction file.

- Codex, Qwen Code, and Grok Build read `AGENTS.md` directly.
- Claude Code loads it through `CLAUDE.md`'s `@AGENTS.md` import. `CLAUDE.md` may
  additionally contain Claude-only workflows that do not belong in the shared file.
- Gemini CLI loads it through `GEMINI.md`'s `@AGENTS.md` import.
- `QWEN.md` keeps the same import for older Qwen Code versions even though current
  Qwen Code also reads `AGENTS.md` directly.

Edit shared instructions in `AGENTS.md`, not in a vendor wrapper. Keep a wrapper's
additional content specific to that agent.

## Agent Skills

The skill contents follow the open Agent Skills format, but project discovery paths
differ by CLI. The canonical authoring tree is `.agents/skills/`:

| CLI | Discovered tree |
| --- | --- |
| Codex | `.agents/skills/` |
| Gemini CLI | `.agents/skills/` compatibility alias |
| Claude Code | `.claude/skills/` |
| Grok Build | `.agents/skills/` in current builds, with `.claude/skills/` as the documented Claude-compatibility path |
| Qwen Code | `.qwen/skills/` |

`.claude/skills/` and `.qwen/skills/` are generated mirrors. Git symlinks are not
used because Windows checkouts silently turn them into plain placeholder files unless
Developer Mode or administrator privileges and `core.symlinks` are all enabled.

After editing `.agents/skills/`, regenerate the mirrors:

```bash
node scripts/sync-agent-skills.mjs
```

Before every push, verify that no mirror drift remains:

```bash
node scripts/sync-agent-skills.mjs --check
```

Claude-specific subagents and workflows remain under `.claude/agents/` and
`.claude/workflows/`; there is no verified portable discovery path for those files.

## Compatibility references

- [Codex skills](https://learn.chatgpt.com/docs/build-skills)
- [Claude Code skills](https://code.claude.com/docs/en/skills) and
  [instruction imports](https://code.claude.com/docs/en/memory#import-additional-files)
- [Gemini CLI skill discovery](https://geminicli.com/docs/cli/using-agent-skills/) and
  [instruction imports](https://geminicli.com/docs/cli/gemini-md/#modularize-context-with-imports)
- [Qwen Code skills](https://qwenlm.github.io/qwen-code-docs/en/users/features/skills/) and
  [repository instructions](https://qwenlm.github.io/qwen-code-docs/en/users/features/memory/)
- [Grok Build skills and compatibility](https://docs.x.ai/build/features/skills-plugins-marketplaces)
