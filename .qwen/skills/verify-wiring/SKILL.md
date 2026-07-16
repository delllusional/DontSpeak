---
name: verify-wiring
description: Audit the client-wiring registry against CURRENT upstream client versions and official docs, then bump version pins (verified_client_version / verified_on in ds-config's wire/registry.rs). Not for "is my machine wired?" — boot reconcile already rewires on startup. Use when a client ships a new version, a user reports wiring breakage, a wiring/shaper is being changed, pins are old, or someone asks "are these contracts still current?".
---

# DontSpeak — verify the client wiring against current versions

> **Task setup:** Before starting, read and apply
> [`docs/TASK-BASELINE.md`](../../../docs/TASK-BASELINE.md) and
> [`docs/TASK-EFFORT.md`](../../../docs/TASK-EFFORT.md).

> The registry (`rust/crates/ds-config/src/wire/registry.rs`) declares, per client, WHERE
> it's wired (paths), HOW (mechanism), the official docs the contract came from (`DocRef`
> URLs), and a VERSION PIN: `verified_client_version` + `verified_on` = "the merge shape was
> confirmed against these docs when this client version was current". This skill is the
> **upstream contract audit** that keeps those pins honest — not a substitute for boot
> rewire. The host engine already runs `ds_wire::reconcile` at startup (and on config
> change) so a machine's client configs converge to *whatever the code currently thinks is
> correct*. This skill checks whether that code still matches *upstream*. `dontspeak wire
> --list` prints the registry; `docs/CLIENT-INTEGRATIONS.md` mirrors the pins.

## Steps — per registry client (all four unless asked otherwise)

The registry is Claude Code, OpenAI Codex, Qwen Code, and Grok. Do not skip Grok.

1. **Current version.**
   - Claude Code: `claude --version`
   - Codex CLI: `codex --version` if installed, else `npm view @openai/codex version`
     (not installed ⇒ the pin means "docs read while X was current", note it as such)
   - Qwen Code: `qwen --version` if installed, else `npm view @qwen-code/qwen-code version` (or check package metadata)
   - Grok: `grok --version` if installed, else note the pin as docs-only for the last known
     release (see registry / [CLI docs](https://docs.x.ai/build/cli/reference))

2. **Re-read the entry's own `DocRef` URLs** (they are the source-of-truth list — don't
   search) and check the exact contract points the wiring depends on:
   - *Hook clients:* Claude Code / Qwen Code JSON, Codex TOML, and Grok's native
     `~/.grok/hooks/*.json` — one JSON (or TOML group) object on stdin routed by event
     name; config schema and registered events. Full table in `docs/HOOKS.md` and
     `docs/CLIENT-INTEGRATIONS.md`. Claude Code and Qwen Code share six events
     (`MessageDisplay`, `SessionStart`, `SessionEnd`, `UserPromptSubmit` ×2, `Stop`,
     `Notification`); Codex has three (`SessionStart`, `UserPromptSubmit`, `Stop` — no
     `MessageDisplay` stream, so `Stop` also voices the reply and `SessionStart` is
     greet-only); Grok owns a dedicated hooks file plus AGENTS.md digests (see registry
     comments). Shapers: `ds-config/src/wire/hooks.rs` (Claude/Qwen), `codex.rs`, Grok
     hooks mechanism.
   - *MCP clients:* the `mcpServers` / `[mcp_servers.<name>]` entry shape (stdio:
     `command`, optional `args`) and WHICH file (`~/.claude.json` user scope for Claude
     Code; `~/.qwen/settings.json` for Qwen Code; Codex TOML; `~/.grok/config.toml` for
     Grok).
   - *`mcp_client_prefix` (the entry's `clientInfo.name` prefix):* the name this client sends in
     its MCP `initialize` handshake, which is how a TOOL call is attributed to it (the hooks'
     half is the `--client <token>` verb) — matched by `starts_with`, not exact-equal, so one
     short token (`"qwen"`, `"grok"`, …) covers every observed variant. The authority is the
     field, not the docs: run the client against the MCP server once and read the activity log's
     `mcp initialize clientInfo.name=… client=…` line, and confirm it still starts with the
     registered prefix (or correct the prefix if the client renamed itself entirely).

3. **Verify the merge shape locally** (no client needed):
   `./rust/target/debug/dontspeak wire <client> --print-only` — the emitted document must
   match the doc's schema. Run `cargo test -p ds-config -p dontspeakd -p dontspeak` (shapers +
   installer-sync + registry tests).

4. **Behavioral check (on-device, needs the app + client installed):** ensure the host app
   has been launched so boot reconcile has run (or `dontspeak wire --reconcile`), then run
   one session — a spoken reply proves hooks; a dictation proves MCP. This step is the
   user's preference to run themselves; report the pin bump as "docs-verified" if it was
   skipped, and say so.

5. **Bump the pin** in `registry.rs`: set `verified_client_version` to step 1's version and
   `verified_on` to today, for the clients actually re-verified. If a contract point CHANGED,
   the pin bump is NOT enough — fix the shaper in `ds-config` (and its tests) first; the pin
   only moves once the wiring matches the doc again.

6. **Commit** with a message saying which clients were re-verified, against which versions,
   and whether anything in the contract moved.

## Caveats

- Boot rewire does **not** replace this skill. Reconcile only re-applies the code's current
  merge shape; if the shaper is wrong for a new upstream contract, every boot re-applies
  the wrong shape more reliably.
- The docs are unversioned web pages — a changed page under the same URL is exactly what this
  loop exists to catch. If a `DocRef` URL 404s, find the moved page, update the `DocRef`, and
  treat it as a contract-change until proven otherwise.
- Codex (and sometimes Grok) may be absent on a given dev machine — `--print-only` and the
  tests still fully validate the merge shape; only step 4 needs the real client.
- Pins are per-client: don't bump clients you didn't actually re-check.
