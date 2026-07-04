# Per-terminal narration: focus-gated TTS

DontSpeak's TTS queue is a single, session-tagged FIFO: the worker plays only the
active session's items and pauses everything when no terminal is frontmost. Tabbing
away from a terminal pauses its narration without dropping it; tabbing back resumes at
the latest item, whether you tabbed to a browser or to another terminal. This works the
same way on macOS, Windows, and Linux — it isn't a Terminal.app special case.

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
   the foreground session's voice.

Since there's no cheap, portable way to map "which window/tab has focus" to "which
Claude session," the active session tracks the terminal you last typed in rather than
the one you most recently brought to the front. Foregrounding a terminal picks which
session's narration you're listening *for*, but prompting it is what actually makes it
speak. The `pause_in_background` config setting (default false) only controls whether
playback pauses at all while no terminal is frontmost — it's independent of which
session is selected.

## One queue, no per-session state

Speech is a single FIFO with no reply/narration distinction and no cap: whatever the
`narrate` setting enqueues plays in order, with pause/resume, barge-in, and focus-hold
applying identically to every item. Each `Item` carries only `session: Option<String>`;
the worker holds any item whose session isn't active (or while no terminal is
frontmost) and plays it once the gate clears. Tagging and filtering one queue gets the
same per-terminal behavior as separate per-session queues would, without their
lifecycle/GC overhead. The active session falls back to silence when it goes quiet,
until the next `MarkActive` repoints it.
