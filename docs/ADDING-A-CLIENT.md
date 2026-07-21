# Adding a client

Checklist for integrating the next client, in order. Worked example: Kimi Code
(`kimi_code`). Most steps are test-enforced — run the step-12 test set after each
one and let the failures walk you forward.

1. **Upstream research first — before any code.** The official hooks/MCP docs
   become the registry `DocRef`s (≥1 per mechanism; a registry test enforces it).
   Capture a live hook stdin payload and the MCP `initialize` `clientInfo.name`
   (drives `mcp_client_prefix`). Decide now:
   - *Hook mechanism*: reuse `ClaudeJsonHooks` / `ClaudeTomlHooks` / `JsonMcp` /
     `TomlMcp` when the shape truly matches. A new shaper is for a genuinely new
     shape only — Kimi's bar: flat `[[hooks]]` with a hard
     event/matcher/command/timeout-only schema and a 600s timeout cap.
   - *Streaming story*: `MessageDisplay` hook, engine tail (design work — see
     `dontspeakd::grok_stream`), or Stop-only.
   - *Stop text source*: `last_assistant_message` in the payload, or a transcript
     fallback (Kimi: session `wire.jsonl`).
2. **`rust/crates/ds-client/src/lib.rs`** — variant, `parse`, `as_str`,
   `CLIENTS`, and the enumerating tests.
3. **`rust/crates/ds-config/src/paths.rs`** — path fields, `resolve()` via
   `client_config_dir` + `<CLIENT>_HOME` env override, the `rooted_at()` mirror,
   and a layout test (`kimi_paths_follow_the_kimi_code_home_layout` is the
   template).
4. **`rust/crates/ds-config/src/wire/registry.rs`** — `CLIENT_REGISTRY` entry in
   `CLIENTS` order (order is test-pinned), `LaunchSpec` + aliases,
   `mcp_client_prefix` + a row in the known-mcp-names test,
   `verified_client_version` / `verified_on` (set the pin last, step 12).
5. **New shaper only if step 1 demands it** —
   `rust/crates/ds-config/src/wire/<client>_hooks.rs` modeled on
   `kimi_hooks.rs`, including heal/strip/substring-misidentification tests; a
   `WireMechanism` variant; a thin writer wrapper over `toml_hooks_body` in
   `rust/crates/ds-wire/src/hooks.rs`. The compiler then forces the three
   `WireMechanism` matches in `rust/crates/ds-wire/src/lib.rs`.
6. **Launcher** — dispatch is registry-driven; update `USAGE` and the
   unknown-subcommand hint (`EXPECTED_SUBCOMMANDS`) in
   `rust/crates/dontspeak/src/main.rs` (test-enforced).
7. **`rust/crates/dontspeak/src/hook_narrate.rs`** — add a client arm in
   `last_assistant_text` only if a Stop fallback is needed (Grok/Kimi arms are
   templates; reuse `jsonl_tail`). Streaming clients get `hook_streaming: true`
   + witness seeding instead.
8. **Usage stats** — `rust/crates/ds-agent-usage/src/providers/<client>.rs` plus
   arms in `fetch_rows` / `fetch_account`; provider-matrix row +
   `usage.provider.<token>` in [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md)
   (Usage statistics). Tests use httpmock/tempdir only — never live network or
   credentials (repo invariant).
9. **i18n** — `usage.provider.<token>` in
   `rust/crates/ds-i18n/locales/en.yml` (test-enforced); all new UI strings via
   the catalog.
10. **Docs** — [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md) (supported list,
    Launch block + aliases, capability matrix, wiring table, `*_HOME` list,
    verified-versions table), [HOOKS.md](HOOKS.md) token list,
    [ARCHITECTURE.md](../ARCHITECTURE.md) streaming paragraph, AGENTS.md intro
    client list (content update — allowed).
11. **Skill mirrors** — verify-wiring `SKILL.md` in ALL THREE of
    `.claude/skills/`, `.agents/skills/`, `.qwen/skills/` (client list, count,
    shaper list) per [AGENT-SKILLS.md](AGENT-SKILLS.md).
12. **Verify** — `dontspeak wire <client> --print-only`; then
    `cargo test -p ds-client -p ds-config -p ds-wire -p dontspeak -p ds-agent-usage -p ds-i18n --locked`;
    then a live wire + real session (greet, narration, `clientInfo.name` in the
    activity log); only then set the version pin. Fresh-main worktree + prepush
    per [TASK-BASELINE.md](TASK-BASELINE.md).
