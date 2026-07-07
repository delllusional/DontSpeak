---
name: verify-wiring
description: Re-verify the client-wiring registry against the CURRENT client versions and their official docs, then bump the version pins (verified_client_version / verified_on in ds-config's wire/registry.rs). Use when a client ships a new version, a user reports wiring breakage, a wiring is being changed anyway, or the pins are simply old and someone asks "are these still current?".
---

# DontSpeak — verify the client wiring against current versions

> The registry (`rust/crates/ds-config/src/wire/registry.rs`) declares, per client, WHERE
> it's wired (paths), HOW (mechanism), the official docs the contract came from (`DocRef`
> URLs), and a VERSION PIN: `verified_client_version` + `verified_on` = "the merge shape was
> confirmed against these docs when this client version was current". This skill is the
> re-verification loop that keeps those pins honest. `dontspeak wire --list` prints it all.

## Steps — per client (three unless asked otherwise)

1. **Current version.**
   - Claude Code: `claude --version`
   - Codex CLI: `codex --version` if installed, else `npm view @openai/codex version`
     (not installed ⇒ the pin means "docs read while X was current", note it as such)
   - Qwen Code: `qwen-code --version` if installed, else `npm view @qwenlm/qwen-code version` (or check package metadata)

2. **Re-read the entry's own `DocRef` URLs** (they are the source-of-truth list — don't
   search) and check the exact contract points the wiring depends on:
   - *Hook clients (Claude Code JSON, Codex TOML, Qwen Code JSON — same contract):* one JSON object on stdin
     routed by `hook_event_name`; `Stop` carrying `last_assistant_message`; `UserPromptSubmit`
     honouring `hookSpecificOutput.additionalContext`; the config schema (`hooks.<Event>`
     groups in `~/.claude/settings.json` / `~/.qwen/settings.json` or `[[hooks.<Event>]]` tables in Codex's
     `~/.codex/config.toml`). Registered events — full table in
     `claude/hooks/HOOKS-README.md` — are Claude Code's six (`MessageDisplay`, `SessionStart`,
     `SessionEnd`, `UserPromptSubmit` ×2, `Stop`, `Notification`) vs Qwen Code's five (`SessionStart`,
     `SessionEnd`, `UserPromptSubmit` ×2, `Stop`, `Notification`) vs Codex's two (`UserPromptSubmit`,
     `Stop` — no `MessageDisplay` stream, so `Stop` also voices the reply; shaped in
     `ds-config/src/wire/hooks.rs` for Claude/Qwen, or `ds-config/src/wire/codex.rs` for Codex).
   - *MCP clients:* the `mcpServers.<name>` entry shape (stdio: `command`, optional `args`)
     and WHICH file (`~/.claude.json` user scope for Claude Code; `~/.qwen/settings.json` for Qwen Code).

3. **Verify the merge shape locally** (no client needed):
   `./rust/target/debug/dontspeak wire <client> --print-only` — the emitted document must
   match the doc's schema. Run `cargo test -p ds-config -p dontspeakd -p dontspeak` (shapers +
   installer-sync + registry tests).

4. **Behavioral check (on-device, needs the app + client installed):** wire for real, run one
   session — a spoken reply proves hooks; a dictation proves MCP. This step is the user's
   preference to run themselves; report the pin bump as "docs-verified" if it was skipped, and
   say so.

5. **Bump the pin** in `registry.rs`: set `verified_client_version` to step 1's version and
   `verified_on` to today, for the clients actually re-verified. If a contract point CHANGED,
   the pin bump is NOT enough — fix the shaper in `ds-config` (and its tests) first; the pin
   only moves once the wiring matches the doc again.

6. **Commit** with a message saying which clients were re-verified, against which versions,
   and whether anything in the contract moved.

## Caveats

- The docs are unversioned web pages — a changed page under the same URL is exactly what this
  loop exists to catch. If a `DocRef` URL 404s, find the moved page, update the `DocRef`, and
  treat it as a contract-change until proven otherwise.
- Codex is often absent on dev machines (`~/.codex`) — `--print-only` and the tests still
  fully validate the merge shape; only step 4 needs the real client.
- Pins are per-client: don't bump clients you didn't actually re-check.
