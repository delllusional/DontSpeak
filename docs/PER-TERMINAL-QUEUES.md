# Per-terminal narration: focus-gated TTS

DontSpeak uses one session-tagged TTS FIFO. The worker plays only the active session.
With `pause_in_background` enabled, it pauses when no terminal is frontmost and resumes
when one returns on macOS, Windows, and Linux.

## Focus signal

Two portable layers feed the worker thread's pause/resume decision, both published by
the engine's poll thread into atomics (the worker itself can't call platform focus
APIs directly):

1. **Is a terminal frontmost** — `Platform::is_terminal_frontmost` (macOS
   `NSWorkspace`, Windows `GetForegroundWindow` + exe name, Linux X11/Wayland). Coarse
   but portable: pauses all narration whenever you're in a browser or other non-terminal
   app.
2. **Which session is active** — the session you last submitted a prompt to, set by a
   `MarkActive` RPC carried on the `UserPromptSubmit` hook. Tracking last-*prompted*
   rather than last-to-*produce-audio* means a background agent's output can't steal
   the foreground session's voice. A `MarkActive` ping classified `synthetic` — a
   harness-injected continuation (e.g. Claude Code auto-re-invoking the agent with a
   `<task-notification>` block after a background task finishes), not something a
   human typed — does NOT reassign active-terminal status; only a genuine submit does
   (issue #11).

There is no portable mapping from a focused window or tab to a Claude session, so the
active session is the terminal most recently prompted. `pause_in_background` (default
`false`) only controls pausing while no terminal is frontmost.

## One bounded queue, no per-session queue state

Speech is a single FIFO with no reply/narration distinction. New work is accepted up to
10 KiB per item, 128 queued items and 1 MiB globally, and 32 queued items and 256 KiB for
one session; overflow is rejected. Pause/resume, barge-in, and focus-hold apply identically
to every accepted item. Each `Item` carries only `session: Option<String>`; the worker holds
any item whose session isn't active (or while no terminal is frontmost) and plays it once
the gate clears. Tagging and filtering one queue gets the same per-terminal behavior as
separate per-session queues would, without their lifecycle/GC overhead. The active session
falls back to silence when it goes quiet, until the next `MarkActive` repoints it.
