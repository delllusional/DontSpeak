---
name: verify-wiring
description: Audit the client-wiring registry against CURRENT upstream client versions and official docs, then bump version pins (verified_client_version / verified_on in ds-config's wire/registry.rs). Not for "is my machine wired?" — boot reconcile already rewires on startup. Use when a client ships a new version, a user reports wiring breakage, a wiring/shaper is being changed, pins are old, or someone asks "are these contracts still current?".
---

# DontSpeak — verify client wiring

> Apply [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

Registry: `rust/crates/ds-config/src/wire/registry.rs` — per client: paths, mechanism,
`DocRef` URLs, pins (`verified_client_version` + `verified_on`). This skill is an
**upstream contract audit**, not a substitute for boot rewire (`ds_wire::reconcile` at
startup already converges local configs to *current code*). Print registry:
`dontspeak wire --list`. Pins also mirrored in `docs/CLIENT-INTEGRATIONS.md`.

## Per client (all four unless scoped)

Claude Code, OpenAI Codex, Qwen Code, **and Grok** — do not skip Grok.

1. **Current version**
   - Claude: `claude --version`
   - Codex: `codex --version` or `npm view @openai/codex version` (absent → docs-only pin)
   - Qwen: `qwen --version` or `npm view @qwen-code/qwen-code version`
   - Grok: `grok --version` if installed, else docs-only (registry /
     [CLI docs](https://docs.x.ai/build/cli/reference))

2. **Re-read that entry's `DocRef` URLs** (don't search elsewhere). Check contracts:
   - **Hooks:** Claude/Qwen JSON, Codex TOML, Grok `~/.grok/hooks/*.json` — stdin object
     routed by event; schemas + events in [HOOKS.md](../../../docs/HOOKS.md) and
     [CLIENT-INTEGRATIONS.md](../../../docs/CLIENT-INTEGRATIONS.md). Claude/Qwen: six events;
     Codex: three (no MessageDisplay; Stop voices reply; SessionStart greet-only); Grok:
     dedicated hooks file + AGENTS.md digests. Shapers: `wire/hooks.rs`, `codex.rs`, Grok.
   - **MCP:** `mcpServers` / `[mcp_servers.<name>]` shape + file path (Claude
     `~/.claude.json`, Qwen settings, Codex TOML, Grok `~/.grok/config.toml`).
   - **`mcp_client_prefix`:** `starts_with` on `clientInfo.name` from activity log
     `mcp initialize clientInfo.name=…` line.

3. **Merge shape** (no client needed):
   `./rust/target/debug/dontspeak wire <client> --print-only` matches schema.
   `cargo test -p ds-config -p dontspeakd -p dontspeak`.

4. **On-device** (optional): launch host so boot reconcile ran (or
   `dontspeak wire --reconcile`); spoken reply + dictation. If skipped, report
   "docs-verified" only.

5. **Bump pin** for clients actually re-checked. Contract change → fix shaper + tests
   first, then move pin.

6. **Commit** listing clients, versions, and whether the contract moved.

## Caveats

- Boot rewire ≠ this skill. Reconcile re-applies current code; wrong shaper → wrong
  wiring more reliably.
- Docs are unversioned pages — same URL can change; 404 → update `DocRef`, treat as
  contract change until proven.
- Codex/Grok often missing locally — print-only + tests still validate merge shape.
- Don't bump pins you didn't re-check.
