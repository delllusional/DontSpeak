# Herdr voice-session integration MVP

## Goal

Give every Don’t Speak client running in a Herdr pane a stable, distinct voice
identity and expose enough live state for a Herdr-native indicator or a playful
farm view. Clients outside Herdr keep the current session and voice behavior.

This task implements the Don’t Speak half. The Herdr half should be a separate
plugin repository; no Herdr core change is required for the first useful UI.

## Don’t Speak contract

Herdr already injects `HERDR_PANE_ID` into pane processes. When it is a non-empty
value, Don’t Speak:

1. uses `dontspeak:herdr:HERDR_PANE_ID:<pane_id>` as the terminal queue session;
2. gives `(client, pane_id, language)` its own sticky voice assignment (distinct
   while the configured pool has a spare voice, then least-loaded reuse);
3. joins a client logical session to that pane-scoped queue session; and
4. publishes one `voice_sessions` row keyed by the public `pane_id`.

With no `HERDR_PANE_ID`, queue scoping and voice assignment remain unchanged.

The raw status is available over the existing `ModelStatus` IPC response and as:

```text
dontspeak status --json
dontspeak status --json --since <seq> --timeout-ms 30000
```

The second form long-polls the existing model-status sequence. The MCP `status`
result also includes `state.voice_sessions`.

Relevant fields:

```json
{
  "seq": 42,
  "activity": { "muted": false },
  "voice_sessions": [
    {
      "pane_id": "workspace:pane",
      "source": "codex",
      "active": true,
      "speaking": true,
      "queued": 0,
      "blocked": false,
      "voice": "af_sarah",
      "language": "en"
    }
  ]
}
```

- `speaking` identifies the pane holding the globally serialized TTS player.
- `queued` counts pending speech for that pane and excludes speech already playing.
- `active` reflects Don’t Speak’s active-terminal routing.
- `blocked` means the pane has speech queued behind a different active terminal.
- `activity.muted` is global mute state.
- `voice` is the last voice resolved for the pane; it can be `null` before first
  greeting or utterance.

Agent lifecycle state remains Herdr-owned. The consumer should join
`voice_sessions[].pane_id` to `agent.list[].pane_id`, using Herdr for agent
`blocked`/`done` and Don’t Speak for voice-queue `blocked`.

## Phase 1: native Herdr sidebar indicator

Build a small external Herdr plugin that:

1. starts alongside Herdr and long-polls `dontspeak status --json`;
2. joins status rows to Herdr `agent.list` rows by `pane_id`;
3. calls `herdr pane report-metadata <pane_id> --source
   plugin:dontspeak-voice --token dontspeak_voice=<text> --ttl-ms <value>`;
4. clears the token when a pane disappears or Don’t Speak no longer reports it.

Users add `$dontspeak_voice` to `[ui.sidebar.agents].rows`. Suggested compact,
priority-ordered values:

| State | Token |
| --- | --- |
| speaking | `🔊 Sarah` |
| queued and active | `♪ 2 · Sarah` |
| voice-queue blocked | `⏳ 2 · Sarah` |
| globally muted | `🔇 Sarah` |
| idle, voice known | `· Sarah` |

If Unicode width proves inconsistent, the plugin should offer ASCII equivalents
(`SPEAK`, `Q2`, `WAIT2`, `MUTE`). Herdr’s existing semantic state icon remains
visible, so the voice indicator does not replace operational state.

This needs no fork: `pane.report_metadata` supports arbitrary token patches and
the agents sidebar supports `$name` metadata tokens. A plugin cannot currently
apply a live accent to the pane border. If direct border styling is still wanted,
the smallest upstream surface is one display-only pane metadata field (for
example `accent`) plus border/sidebar rendering of that field. It must not mutate
agent lifecycle state. Changing `display_agent`, titles, or lifecycle labels to
simulate speech is rejected because it overwrites unrelated semantics.

## Phase 2: delightful farm companion

The public `ragamo/herdr-flock` repository is a strong interaction reference: it
polls `agent.list`, keys sheep by `pane_id`, and already maps Herdr lifecycle
states to sheep behavior. Its public repository contains no `LICENSE`,
`COPYING`, or `NOTICE` file as of the reviewed revision
`ae24844b3c8b1cf7cf3dfc3d6e6bc701b6e048a3`. Public visibility alone does not
grant reuse rights, so do not copy, fork, redistribute, or derive from its code
or assets without written permission from the owner.

Recommended path:

- ask the owner to add an explicit license and accept a small optional Don’t Speak
  status adapter; or
- build an independently designed companion plugin using only Herdr’s documented
  plugin/socket contracts and the Don’t Speak contract above.

Keep Herdr lifecycle animation as the base behavior, then overlay voice state:

- **Speaking:** a restrained two-frame mouth movement and one small sound-wave
  ripple; show the voice name on focus/hover.
- **Queued:** one floating musical note, with a numeric badge when depth is above
  one. The note bobs slowly rather than competing with the working animation.
- **Muted:** dim the voice overlay and show a fixed mute badge; do not dim the
  sheep itself, which would hide Herdr’s lifecycle state.
- **Voice-queue blocked:** a small hourglass or gate beside the sheep. Keep this
  visually distinct from Herdr agent-blocked behavior.
- **Done:** retain the farm’s sleeping/done animation; no sound wave even if stale
  telemetry arrives.

Useful interactions that stay operational:

1. selecting a speaking sheep focuses its Herdr pane;
2. pressing a voice-details action toggles a compact label with voice, language,
   pending count, and mute state;
3. a brief “baton pass” ripple moves from the previous speaker to the next when
   the globally serialized queue changes panes.

The native token plugin should ship first: it is small, testable, accessible in
the normal sidebar, and establishes the status adapter the farm can later reuse.

## Deliverables and boundaries

Don’t Speak deliverable:

- Herdr pane-aware queue scope and voice ownership;
- `voice_sessions` model status plus native host mirrors;
- raw snapshot and long-poll CLI;
- MCP status projection and schema;
- unit and contract tests.

Separate Herdr plugin deliverable:

- status adapter and `agent.list` join;
- metadata-token reporter with token clearing/TTL;
- sample sidebar configuration;
- no bundled herdr-flock code or assets unless its owner supplies a compatible
  license.

Herdr core and the original Herdr checkout are outside this implementation task.
