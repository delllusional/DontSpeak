# Per-terminal narration: focus-gated TTS

One session-tagged TTS FIFO. Worker plays only the active session. With
`pause_bg`, pauses when no terminal is frontmost (all three OSes).

## Focus signals

Engine poll thread publishes atomics (worker can't call platform focus APIs):

1. **Terminal frontmost** — `Platform::is_terminal_frontmost` (macOS `NSWorkspace`,
   Windows foreground exe, Linux X11/Wayland). Pauses narration in non-terminal apps.
2. **Active session** — last real `UserPromptSubmit` via `MarkActive` RPC. Last-*prompted*,
   not last-to-speak, so background agents don't steal the voice. `synthetic` MarkActive
   (harness auto-reinvoke) does **not** reassign (issue #11).

No portable window→session map; active = last prompted. `pause_bg` default
`false`.

## Queue shape

Single FIFO (no reply vs narration split). Limits: 10 KiB/item; 128 items / 1 MiB global;
32 items / 256 KiB per session; overflow rejects. Pause, barge-in, focus-hold apply to
all items. Each `Item` has `session: Option<String>`; worker holds non-active or
no-terminal-frontmost items until the gate clears. Active session falls silent when quiet
until next `MarkActive`.
