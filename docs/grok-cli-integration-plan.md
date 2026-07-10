# Plan: Add Grok CLI Support (similar to Claude Code)

**Branch**: grok-cli-support (new worktree off main)
**Date**: 2026-07-10
**Goal**: Enable DontSpeak integration for the xAI Grok CLI ("grok" command / Grok Build TUI) at parity with Claude Code where feasible: primarily MCP server registration so Grok sessions can call `speak`/`listen`/etc. tools. Narration (spoken replies) support requires additional research.

## Plan Review (adversarial check performed 2026-07-10)

**Reviewed against**:
- Actual source (registry, WireMechanism, paths, wire dispatch, io, codex toml patterns, ds-tools catalog + parity tests)
- Official docs (MCP TOML `[mcp_servers.DontSpeak]`, hooks in `~/.grok/hooks/*.json` + strong .claude compat)
- Local grok 0.2.93 install
- Existing shapers and backup/IO helpers

**Strengths**:
- Perfect fit for declarative registry + shared writers.
- MCP prioritized correctly (high value, uses existing `WriteBody::Str` + toml_edit).
- Realistic deferral of full native hooks (Grok compat layer + events make `wire claude_code` already useful for narration).
- Parity tests + `target_for` will make adding low-boilerplate.

**Required adjustments / gaps addressed in this review**:
- Add `WireMechanism::TomlMcp` (alongside JsonMcp / ClaudeTomlHooks).
- New pure shaper `ds-config/src/wire/toml_mcp.rs` returning rendered String (like codex hooks). Must handle table `[mcp_servers.DontSpeak]`, preserve siblings, error on bad shape without clobber.
- Extend `dontspeak/src/wire/mcp.rs` (or thin toml path) + dispatch in `wire.rs` to call TOML path using `backup_then_write(..., "toml", &WriteBody::Str(...))`.
- In registry ClientSpec for Grok: **MCP surface only** for v1 (hooks are separate *.json files; compat via .claude/settings.json is noted).
- Update dispatch match, print_registry "how", and special post-wire hint for Grok.
- ds-tools hardcoded enum + description string (parity test protects).
- paths.rs: fields in both `resolve()` and `rooted_at()`.
- WireTarget updates + CLIENTS.
- io.rs already supports Str — good.
- Docs: MCP-TOOLS.md, HOOKS-README (add Grok section), scripts comments if hardcoded.
- Manual verification on Windows + local grok: use `--print-only` then real wire after build.
- Risk: malformed TOML must leave file untouched (copy json_mcp behavior).
- No changes needed to core narration or STT.

**Implementation order recommended**:
1. Data model (paths, enums, WireMechanism, registry skeleton).
2. toml_mcp shaper + tests.
3. dontspeak wire dispatch + mcp apply.
4. ds-tools + static schemas.
5. Docs + any script notes.
6. Cargo test + manual wire test in this env.
7. Audit (diffs, tests, `grok inspect` after wiring).

Plan is approved for implementation with the above refinements incorporated below.

## Research Summary

### Current Client Wiring Architecture (declarative, no per-client code paths)
- Single source of truth: `rust/crates/ds-config/src/wire/registry.rs` — `CLIENT_REGISTRY: &[ClientSpec]`
- `WireTarget` enum + `parse`/`as_str`/`ALL`/`CLIENTS` in `ds-config/src/enums.rs`
- Surfaces per client:
  - `WireMechanism::ClaudeJsonHooks` → `~/.claude/settings.json` (and qwen) for voice hooks using Claude-contract events (`MessageDisplay`, `Stop`, `UserPromptSubmit`, `Session*`, `Notification`)
  - `WireMechanism::ClaudeTomlHooks` → `~/.codex/config.toml` (format-preserving via `toml_edit`)
  - `WireMechanism::JsonMcp` → `mcpServers.DontSpeak` stdio entry in JSON files (`~/.claude.json`, `~/.qwen/settings.json`)
- Shared pure shapers: `merge_*`/`strip_*` in `ds-config/src/wire/{hooks.rs,json_mcp.rs,codex.rs}`
- Orchestrator: `rust/crates/dontspeak/src/wire.rs` (and `wire/mcp.rs`)
- Installer + runtime: `dontspeak wire <target>` and MCP `setup_integration` tool call the same code.
- Presence gating: `gate_on_presence` + `present` fn; Claude Code is special (unconditional).
- MCP server name is always **DontSpeak** (capital S).
- Narration details live in `claude/hooks/HOOKS-README.md`, `docs/STREAMING-NARRATION.md`, `docs/MCP-TOOLS.md`.
- Parity tests + `verify-wiring` skill keep pins/docs current.

Current clients (from registry):
1. **Claude Code** (`claude_code`): hooks (streaming `MessageDisplay`) + MCP. `~/.claude/*`
2. **OpenAI Codex** (`codex`): TOML hooks only (no MCP surface). Uses special `codex_stream` supervisor for mid-turn narration via app-server. `~/.codex/config.toml`
3. **Qwen Code** (`qwen_code`): hooks (non-streaming, `Stop` carries full reply) + MCP (same file). Uses `InlineShell` command style.

STT special case only for `claude_code` (delegates via key simulation from `keybindings.json`).

### Grok CLI ("grok") Facts (from official docs.x.ai + local 0.2.93 install)
**Official sources** (searched 2026-07-10):
- Launch: https://x.ai/news/grok-build-cli
- MCP: https://docs.x.ai/build/features/mcp-servers
- Hooks + compat: https://docs.x.ai/build/features/hooks , https://docs.x.ai/build/features/skills-plugins-marketplaces
- Enterprise/config: https://docs.x.ai/build/enterprise

- Binary / version on this machine: `grok 0.2.93` (`~/.grok/bin/grok.exe`).
- Config: **TOML** `~/.grok/config.toml` (global) + project `.grok/config.toml` (walked from cwd to git root; project overrides global for same name).
- **MCP servers** (stdio or HTTP):
  ```toml
  [mcp_servers.DontSpeak]
  command = "/absolute/path/to/dontspeak"
  # args usually omitted for stdio mode
  env = { ... }                    # ${VAR} expansion supported
  startup_timeout_sec = 30
  tool_timeout_sec = 60
  # For HTTP: url = "..." + headers = {}
  ```
  Also managed via `grok mcp add`, `grok mcp list`, `grok mcp remove`, `grok mcp doctor`.
  Tools namespaced `<server>__<tool>`.
- **Hooks**: Defined in JSON files `~/.grok/hooks/*.json` (global) and `<project>/.grok/hooks/*.json`.
  Example structure:
  ```json
  {
    "hooks": {
      "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "dontspeak provide", "timeout": 5 }] }],
      "Stop": [{ "hooks": [{ "type": "command", "command": "dontspeak notify", "timeout": 1800 }] }],
      "SessionStart": [...],
      "Notification": [...]
    }
  }
  ```
  - `type`: "command" or "http".
  - `command` is a string (full command or path).
  - `matcher` for tool events (regex).
  - Project hooks require `/hooks-trust`.
  - Grok also **reads Claude Code's `.claude/settings.json`** (and Cursor) for hooks + MCPs for zero-config compatibility (camelCase supported).
  - Key events for narration: `SessionStart`/`SessionEnd`, `UserPromptSubmit`, `Stop`/`StopFailure`, `Notification`. (No `MessageDisplay` listed in current docs; end-of-turn narration via `Stop` is the Codex-like path.)
  - Payload: JSON on stdin with `hookEventName`, `sessionId`, etc. + env `GROK_HOOK_*`. Passive hooks ignore stdout.
- **Claude Code compatibility** (strong, default): Grok reads `~/.claude/*`, `.claude/*`, `CLAUDE.md`, `~/.claude.json` (MCP), `.claude/settings.json` (hooks), skills, plugins, agents, etc. alongside native `.grok/` paths. `grok inspect` shows sources.
- Other: ACP, headless (`-p`), subagents, leader socket, `grok inspect`, marketplace/plugins/skills.
- Presence: `~/.grok` directory (and/or `grok` in PATH).

Grep for "grok" in the DontSpeak source tree returned no hits prior to this work (new feature).

### Feasibility for "similar to Claude Code"
- **MCP / tools (speak, listen, get_status, setup_integration, ...)**: **High priority and straightforward**. Requires a TOML MCP writer for `[mcp_servers.DontSpeak]` (new `TomlMcp` mechanism + `merge`/`strip` using `toml_edit`, modeled exactly on existing JSON + Codex TOML code). Also supports `grok mcp add` flow indirectly via config edit.
- **Narration (spoken replies, greets, earcons, active-terminal routing)**: **Good partial support possible** (better than initial research suggested).
  - Core events exist: `SessionStart`/`End`, `UserPromptSubmit`, `Stop`/`StopFailure`, `Notification`.
  - Grok **reads `.claude/settings.json` hooks** for compatibility → existing `ClaudeJsonHooks` wiring (from `wire claude_code`) already activates narration hooks for Grok users.
  - Native Grok hooks use similar JSON payload + `command` string. The same `dontspeak notify` / `provide` binaries can be wired (payloads are Claude-compatible per community integrations; `hookEventName` + `additionalContext`-style injection appears supported on `UserPromptSubmit` and `Stop` for post-turn replies).
  - **No `MessageDisplay`** (streaming mid-turn) listed in current official hooks docs — narration will be primarily end-of-turn (like Codex) unless a streaming path (ACP / leader socket / other) is added later.
  - `UserPromptSubmit` can inject narration spec + mark active terminal; `Stop` can speak the reply + earcon.
  - Recommend **native Grok hooks surface** (write `~/.grok/hooks/dontspeak.json` or merge into hooks JSON) in addition to MCP.
- **STT / dictation delegation**: Not applicable (no push-to-talk key equivalent; use built-in/system).
- **Other surfaces**: `wire --all`, install scripts, `setup_integration` MCP tool (add `"grok"`), docs (point to https://docs.x.ai/...), UI strings, tests, `grok inspect` verification.
- **Bonus**: Because of deep Claude compat, `wire claude_code` gives partial Grok support today. Native target makes it first-class and pure-`~/.grok/`.

### Files Touched (high level)
- `ds-config`: paths.rs (add `grok_dir`, `grok_config`), enums.rs (WireTarget::Grok + ALL/CLIENTS/parse/as_str), wire/registry.rs (ClientSpec with docs URLs https://docs.x.ai/build/features/mcp-servers and /hooks), new `wire/toml_mcp.rs` (or generalized), possibly hooks JSON shaper.
- `dontspeak`: src/wire.rs (new arm for TomlMcp + optional Grok hooks JSON), src/wire/mcp.rs updates.
- `ds-tools`: descriptions.rs + lib.rs (add "grok" to setup_integration enum; parity test will enforce).
- Docs: update MCP-TOOLS.md, HOOKS-README.md, ARCHITECTURE.md, README.md; reference official x.ai docs.
- Scripts/installers: they use the registry — mostly automatic.
- Tests: wire/mcp/ registry parity + manual on 0.2.93.
- Static: also sync `mcps/DontSpeak/tools/setup_integration.json` (or let runtime schema dominate).
- Verified version: 0.2.93 (local) — bump via `verify-wiring` skill against current.

## Detailed Implementation Plan

### Phase 0: Prep (this branch)
- [x] `git worktree add -b grok-cli-support .claude/worktrees/grok-cli-support main`
- [ ] Run `cargo check` / targeted tests in the worktree to establish baseline.
- [ ] Read `docs/BUILD-DEPLOY.md`, `CONTRIBUTING.md`, `.claude/CLAUDE.md` (or root CLAUDE.md), AGENTS.md for local conventions.
- [ ] Use `verify-wiring` skill? (not yet, since no new verified client).

### Phase 1: Data Model & Paths (ds-config)
1. In `rust/crates/ds-config/src/paths.rs`:
   - Add `pub grok_dir: PathBuf;`
   - Add `pub grok_config: PathBuf;` (`.grok/config.toml`)
   - In `resolve()`: `let grok_dir = home.join(".grok"); grok_config: grok_dir.join("config.toml"), ...`
   - In `rooted_at()`: similar inert paths under the temp home.
2. In `rust/crates/ds-config/src/enums.rs`:
   - Add variant `Grok,` to `WireTarget` (after QwenCode).
   - Update `ALL`, `CLIENTS`.
   - Extend `parse` and `as_str` (token: `"grok"` for consistency with binary name; display "Grok" or "Grok CLI").
   - Update any lists/comments that enumerate clients.
3. In `rust/crates/ds-config/src/wire/registry.rs`:
   - Add `ClientSpec` entry for Grok (native MCP + hooks surface):
     ```rust
     ClientSpec {
         target: WireTarget::Grok,
         display_name: "Grok",
         kind: ClientKind::TerminalCli,
         present: |p| p.grok_dir.exists(),
         detect_dir: |p| &p.grok_dir,
         gate_on_presence: true,
         surfaces: &[
             Surface {
                 mechanism: WireMechanism::TomlMcp,  // new
                 config_file: |p| &p.grok_config,   // ~/.grok/config.toml
                 load_hint: Some("run `grok mcp list` or start a new session; `grok inspect` shows sources"),
                 hook_streaming: false,
                 hook_command_style: HookCommandStyle::ArgsArray, // or new style if needed
             },
             // Optional/future: native hooks JSON surface in ~/.grok/hooks/dontspeak.json
             // (or rely on .claude/settings.json compat for Claude-contract hooks)
         ],
         docs: &[
             DocRef { topic: "mcp", url: "https://docs.x.ai/build/features/mcp-servers" },
             DocRef { topic: "hooks", url: "https://docs.x.ai/build/features/hooks" },
         ],
         verified_client_version: "0.2.93",
         verified_on: "2026-07-10",
     },
     ```
   - Note: Because Grok reads `.claude/settings.json` hooks and `~/.claude.json` MCPs, `wire claude_code` already provides substantial functionality. Native target adds pure `~/.grok/` MCP + explicit hooks JSON wiring.
   - Update the registry match test implicitly via `WireTarget::CLIENTS`.
4. Add `TomlMcp` variant to `WireMechanism` (in registry.rs or enums if moved). Document it.

### Phase 2: TOML MCP Shaper
- New file `rust/crates/ds-config/src/wire/toml_mcp.rs` (or extend existing):
  - `merge_mcp_server_toml` / `strip_mcp_server_toml` using `toml_edit` (preserve formatting/comments like codex hooks).
  - Target table: `[mcp_servers.DontSpeak]` (note key casing; DontSpeak exactly).
  - Set `command`, `args` (usually empty for stdio), preserve other keys (enabled, env, timeouts, etc.).
  - Error types similar to `CodexMergeError`.
- Export from `wire.rs` mod.
- Pure functions + unit tests (mirror json_mcp.rs tests).

### Phase 3: Wire Orchestrator Updates
- `rust/crates/dontspeak/src/wire.rs`:
  - Handle `WireMechanism::TomlMcp` → new `mcp::apply_toml(...)` or unified `mcp` module.
- Extend or duplicate `src/wire/mcp.rs` → support TOML target (or make `Target` carry mechanism and dispatch).
- `target_for` may stay mostly the same (builds from spec).
- Add hint for Grok if needed (no app-server equivalent mentioned).
- Update help text / `--list` output automatically via registry.

### Phase 4: Tool / MCP Surface
- `rust/crates/ds-tools/src/lib.rs`:
  - Update the literal enum in `setup_integration` Tool: add `"grok"`.
- `rust/crates/ds-tools/src/descriptions.rs`:
  - Update `SETUP_INTEGRATION` const to list "grok".
- The parity test (`setup_integration_target_enum_matches_config_type`) will enforce consistency once WireTarget updated.
- `rust/crates/dontspeak/src/tools.rs`: `call_wire` already delegates to `WireTarget::parse` + registry — no change needed (it will just work).

### Phase 5: Narration / Hooks (Partial or Deferred)
- For now: **do not** add a hooks surface for Grok. Document in the ClientSpec comment and HOOKS-README.
- Research tasks (separate spike):
  - Does Grok emit/parse Claude-contract hook JSON on project hooks or global?
  - Can we drive narration via leader.sock / ACP events / process output?
  - Implement `grok_stream` supervisor analogous to codex if a streaming channel exists.
- If Grok later ships MessageDisplay etc. under Claude compat or native, flip `hook_streaming: true` + add surface + bump version pin (like Qwen future note).
- Update `claude/hooks/HOOKS-README.md` with a "Grok" section explaining current status.

### Phase 6: Documentation & User Experience
- `docs/MCP-TOOLS.md`: add `"grok"` to setup_integration targets table/desc.
- `ARCHITECTURE.md`, root `README.md`: mention Grok alongside others.
- `scripts/install.sh` and `web/install.sh`: they call `wire --all` or list clients; should pick up automatically, but add example / note.
- `claude/hooks/HOOKS-README.md`, `docs/STREAMING-NARRATION.md` (note differences).
- Add entry to `docs/` if a dedicated integration doc is warranted.
- Update any hardcoded client lists in shell scripts, PowerShell installers, etc.
- Desktop / shortcuts: optional later.

### Phase 7: Tests, Verification, Polish
- Existing tests (wire dispatch, mcp apply, registry parity, setup enum parity) should drive most coverage.
- Add specific tests for new toml_mcp shaper (shape preservation, sibling keys, remove, absent client skip).
- `cargo test -p ds-config -p dontspeak -p ds-tools`
- Run full prepush / CI gates locally (`prepush` skill?).
- Use / run `verify-wiring` skill after wiring a real `grok` install; update `verified_*` fields + date.
- Test end-to-end:
  - `dontspeak wire grok --print-only`
  - Actual wire (creates/updates `~/.grok/config.toml` safely).
  - `grok` then calls DontSpeak tools.
  - `setup_integration {target:"grok", enabled:true}` via MCP.
  - `--remove`, re-wire, backup files.
- Update snapshots or golden outputs if any.
- Windows-specific: ensure paths / toml_edit work (this env is Windows).

### Phase 8: Edge Cases & Future
- Grok project config: our wire targets the **user** `~/.grok/config.toml`. Project ones are for team sharing MCPs; user can copy or we could document.
- Multiple surfaces later (if hooks added).
- If Grok uses different server name casing or requires `args`, adjust (current stdio is no-args).
- Add Grok to status / "wired clients" UI if such a list exists.
- Consider STT delegation only if Grok exposes a dictation keybind.
- Once hooks supported, add a `ClaudeTomlHooks` or new mechanism + surface (Grok may prefer different command style).

## Risks & Mitigations
- **TOML shape drift**: Use `toml_edit` carefully; copy style from `codex.rs`. Add robust "unmergeable" errors instead of clobbering.
- **No narration yet**: Clearly communicate in docs, release notes, `wire` output. MCP alone is still valuable (tools inside Grok).
- **Config.toml already has other sections**: Our merge must only touch `[mcp_servers.DontSpeak]`.
- **Version pin**: Start with a recent observed version from `~/.grok/version.json`; re-verify with skill.
- **Test isolation**: Some wire tests have known leaks (see comments in mcp.rs); keep new code clean.
- **Back compat**: Adding target is additive; old binaries ignore unknown tokens.

## Order of Changes (suggested PR / commits)
1. Paths + enum + registry skeleton (compiles but no writer yet).
2. Implement toml_mcp shaper + tests.
3. Wire dispatch + mcp apply updates.
4. ds-tools schema + descriptions + update docs strings.
5. Docs, scripts, comments.
6. Tests + manual verification on this machine (has `grok`).
7. `verify-wiring` run + pin bump (follow-up commit).

## Commands to Validate in Worktree
```pwsh
cd .claude\worktrees\grok-cli-support
cargo check -p ds-config -p dontspeak -p ds-tools
cargo test -p ds-config --test '*'   # or specific
# After code:
dontspeak wire grok --print-only
dontspeak wire --list
# Then full wire test (backup will be made)
```

## Next Steps After Plan Approval
- Implement per phases.
- Possibly spawn sub-agents for isolated file edits (e.g. one for Rust changes, one for docs).
- Run verification before claiming done.

This brings Grok CLI into the "wired clients" family with minimal new surface area thanks to the declarative registry.
