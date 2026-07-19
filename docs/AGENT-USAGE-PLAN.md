# Agent usage statistics

Spec for the desktop **Usage** tab. Matches the shipped implementation on
`feat/agent-usage-statistics`. CodexBar is a behavioral reference only.

## Goal

Show subscription quota for coding agents DontSpeak supports: Claude Code, Codex,
Qwen Code, Grok, and Kimi Code. Independent of speech-engine stats; works with the engine
stopped; does not extend `model_status`.

## User-visible behavior

### Navigation

**Usage** → Status → Tools → Log → Credits on macOS, Windows, and Linux.
Usage is the cold-start default on all three hosts. On macOS, opening the hidden
window from the tray preserves the current sidebar selection; it does not force
the Status screen.

### When the tab is selected

1. **Skeleton (no network):** `ds_agent_usage_skeleton_json()` returns every
   **installed** agent (`ClientSpec::present` / wire detect dirs) plus any
   **cached** rows.
2. **UI paints only cards that already have rows** (cache hits). Empty shells are
   not shown. If there is no cache yet, the list stays blank (not “loading…”).
3. **Per-agent force load:** for each installed agent, off the UI thread call
   `ds_agent_usage_card_json(agent, force=1)`. When a load returns rows, insert or
   update that card in its existing UI slot. Identical, failed, or empty probes do
   not remount a card and do not wipe a prior good value.
4. **Settled empty:** after all loads finish with no data, show
   `usage.unavailable`. While loads are still in flight, do not show that string.

There is **no** Refresh button and **no** loading spinner. Selecting the tab again
repeats the flow (cache first, then force network per agent).

### Card layout (identical on all hosts)

For each card with data:

1. **Title** — `usage.provider.<agent>` (`claude_code`, `codex`, `qwen_code`, `grok`, `kimi_code`)
   left; optional **account** (usually email) top-right in the same caption
   style as period remaining. Fully **transparent by default**; click/tap
   toggles full opacity for this UI session only (not persisted — reload hides
   again). Sources are local credentials only:
   - Claude: `~/.claude.json` → `oauthAccount.emailAddress`
   - Codex: `~/.codex/auth.json` JWT `id_token` → `email`
   - Grok: `~/.grok/auth.json` session entry → `email`
   - Qwen: none (API-key auth)
2. **Each row** (session → week → month when present):
   - period label: `usage.<period>` (`session` | `week` | `month`)
   - remaining time until `resets_at_unix` top-right via `ds_usage_resets_in`
     (minute granularity only — e.g. `2d 05h`, `5h 11m`, `12m`; no seconds, no
     “Resets in” prefix)
   - progress bar (0…100) — percent is shown only as the bar, not as text

Missing periods are omitted, never shown as `0%`. Never show plan, cost,
balance, charts, or raw provider errors.

### Speaking highlight

While TTS is playing an utterance from a wireable client, `model_status.activity`
includes `speaker` (`claude_code` / `codex` / `qwen_code` / `grok` / `kimi_code`; `null` when
idle or non-client). Hosts wash that agent’s Usage card with a **random pastel tint**
from `ds_random_pastel_wash_json` (single recipe in `ds-core`: HSV random H, S=0.42,
V=0.92, α=0.30 → `{"r","g","b","a"}`). A new pastel is chosen when `speaker`
becomes non-null or changes agent; the color is **frozen** for the continuous
highlight on that agent. Clear on idle (`speaker` null).

The **top-bar speaking stripe** remains brand purple. Usage progress bars remain
brand purple. Pastel wash is Usage-card-only; hosts only paint and freeze (no local HSV).

**Pipe (one producer field → one status field → host match):**

1. Enqueue keeps `source: ClientSource` on the TTS item (`Speak` / `SpeakNarration` /
   `Earcon` / `GreetSession` / Codex·Grok stream adapters). Arg order: source before
   session.
2. Worker claim publishes `PlayingClaim { source, session }`.
3. `tts_running()` → `(tts_active, Option<source>)` filtering `ClientSource::is_client()`.
4. Hosts: if `speaker == card.agent`, apply speaking pastel wash.

Queued earcons that set `tts_active` use the same claim. Out-of-band needs-input
cues under focus hold do **not** claim playback (pre-existing half-duplex rule) and
therefore leave `speaker` null.

## Scope boundaries

Out of scope:

- model-specific weeklies (e.g. Claude `seven_day_opus`), daily-only counters;
- token/request counts, costs, balances, credits, billing history;
- plan names (account email is in-scope when local credentials expose it);
- local transcript/`/stats` estimates;
- charts, history, notifications, menu-bar widgets;
- account switching, credential edit, browser-cookie import;
- background polling while the tab is closed;
- Refresh button / global loading UI.

## Provider matrix (implemented)

| Agent | Source | Session | Week | Month |
| --- | --- | --- | --- | --- |
| Claude Code | OAuth `GET …/api/oauth/usage` + macOS Keychain or `~/.claude/.credentials.json` | API `five_hour` → `session` | API `seven_day` → `week` | — |
| Codex | short-lived `codex app-server` → `account/rateLimits/read` | 300 min or session label | 10080 min or weekly label | explicit monthly label only |
| Qwen Code | Alibaba Coding Plan HTTP + env/settings API keys | `per5Hour*` | `perWeek*` | `perBillMonth*` / `perMonth*` |
| Grok | try `x.ai/billing` via `grok agent stdio`; else gRPC-web `GetGrokCreditsConfig` + Bearer from `~/.grok/auth.json` | — | web: full cycle length ~4–12 days (start→reset); not remaining distance | CLI monthly-named; web else / no cycle start → month (stable) |
| Kimi Code | `GET https://api.kimi.com/coding/v1/usages` + Bearer from `~/.kimi-code/credentials/kimi-code.json` | `limits[]` 5h window (300 min / 5 h or `5h` label) | top-level `usage` weekly | — |

Windows resolves CLI binaries via `.exe`/`.cmd` (never extensionless npm shebangs).

Claude notes: accept fractional `resets_at`; `utilization` is percent 0…100
(same scale as `limits[].percent` — do not treat `1.0` as a full fraction);
on HTTP failure/empty keep last good card (do not cache empty). Period wire tokens
and labels: `session` → Session, `week` → Week, `month` → Month.

## Architecture

### Crates

| Crate | Role |
| --- | --- |
| `ds-agent-usage` | Domain: deck/card/row, install gate, per-agent cache, providers |
| `ds-http` | Blocking HTTP leaf (trust roots, timeouts, bounded body); usage providers enforce HTTPS |
| `ds-core` FFI | JSON C ABI + `ds_usage_resets_in` |
| `ds-i18n` | All Usage UI strings |

No dependency on `dontspeakd`, speech engine, or model download semantics.

### Domain model

```text
UsageDeck
  cards[]: UsageCard
    agent            // ClientSource wire token
    account?         // optional email / login from local credentials
    rows[]: UsageRow
      period         // session | week | month
      used_percent   // finite 0…100
      resets_at_unix // UTC epoch seconds
```

Rules:

- Probe only agents with `ClientSpec::present`.
- Card order is `ClientSource::CLIENTS`; hosts preserve the skeleton deck order
  when asynchronous card loads complete. Hosts do not maintain agent-token maps.
- At most one row per period per card; order session → week → month.
- Require percent + reset timestamp to emit a row; clamp percent to 0…100.
- Never serialize credentials, response bodies, or provider error strings.
- Classify windows by semantic labels or exact known durations (not primary/secondary slots).

### Cache

- One typed cache keyed by `ClientSource` is shared by every agent provider and
  every host through `ds-core`.
- Last-good cards are held in memory and atomically mirrored to
  `agent-usage-cache.json` under the OS cache directory. The file contains normalized
  quota rows, optional display-only account labels, and fetch timestamps; never
  authentication secrets or provider bodies.
- Skeleton lazily restores that file, so the first tab visit after restart can
  paint the prior value before any network request completes.
- Soft TTL **60s** for non-force `refresh_card` (tooling / optional soft path).
- Tab UI always force-loads after skeleton so re-select refreshes.
- Store only cards with rows; empty probe never overwrites a good cache entry.
- Concurrent refreshes for the same agent serialize through one slot and reuse the
  probe that completed while later callers waited.

### FFI (handle-free, never panics)

| Symbol | Behavior |
| --- | --- |
| `ds_agent_usage_skeleton_json()` | Installed agents + cache; **no network** |
| `ds_agent_usage_card_json(agent, refresh)` | Blocking single-card load |
| `ds_agent_usage_json(refresh)` | Aggregate all cards (tests/tooling) |
| `ds_usage_resets_in(resets_at_unix)` | Remaining duration string (no seconds) |

Mirrors: `ds-core/src/ffi.rs`, `dontspeak.h`, `Native.cs`, `apps/linux/gtk/src/ffi.rs`.

Hosts call blocking entry points **off the UI thread**. ABI JSON is decoded in each
host's lowest-level data-source adapter and only typed decks/cards reach view code.
Use a generation counter so stale completions after leave-tab are ignored.

### Native UI

| Host | Navigation | Bind |
| --- | --- | --- |
| macOS | `NavigationSplitView`, first/default; tray reopen preserves selection | typed `UsageDeck` / `UsageCard` / `UsageRow` |
| Windows | `NavigationView`, first/default | typed DTOs from `AgentUsageDataSource` |
| Linux | `AdwViewStack`, first/default | typed structs from the FFI adapter |

Hosts retain stable card shells and transition only changed typed values. Agent
identity/order, fetch/cache, install gating, and period selection live in Rust.

### Localization

`rust/crates/ds-i18n/locales/en.yml` under `usage.*` and `common.nav_agents`.
Period keys match wire tokens. Unused keys `usage.loading` / `usage.refresh` /
`usage.retry` may remain for future use but are not required by the current UI.

## Security and privacy

- Credential reads are read-only, size-bounded, documented paths only.
- No login, token refresh, browser launch, interactive Keychain prompt, or provider-file writes
  during Usage refresh.
- Production HTTPS only for live probes; tests use fixtures / loopback only.
- Secrets and raw provider payloads never appear in FFI JSON or logs.
- Provider children are bounded and reaped.

## Acceptance criteria

- [x] Usage, Status, Tools, Log, Credits in that order on all three hosts; Usage is the cold-start default.
- [x] macOS tray Settings reopens the window without discarding the current sidebar selection.
- [x] Card-centric schema (`agent` + `rows`; periods `session` | `week` | `month`); skeleton + per-card ABI.
- [x] First open with no cache: no empty shells; cards appear only when loaded.
- [x] Re-select: cached cards first, then force per-agent refresh; no Refresh button.
- [x] Restart: restore last-good cards from the OS cache, then refresh in background.
- [x] ABI JSON decoded below view code; unchanged refresh results do not remount cards.
- [x] Rows: period, percent, progress, remaining duration (no “Resets in” prefix).
- [x] Wire install gate; missing periods omitted; failures isolated / sanitized.
- [x] Engine stopped still works; no UI-thread blocking for network.
- [x] Automated tests: fixtures / temp homes / loopback only.

## Related docs

- [CLIENT-INTEGRATIONS.md](CLIENT-INTEGRATIONS.md) — short Usage summary for integrators
- GitHub issue #102
- PR #105
