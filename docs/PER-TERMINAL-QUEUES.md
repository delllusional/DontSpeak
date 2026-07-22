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

## Custom terminal and editor identifiers

Two direct `config.toml` settings extend focus detection and hot-reload with the rest of
the engine configuration:

```toml
extra_terminals = ["myterm.exe"]
extra_editors = ["myeditor.exe"]
```

| OS | `extra_terminals` values | `extra_editors` values |
|---|---|---|
| Windows | Executable basename, such as `myterm.exe` | Executable basename |
| macOS | Bundle identifier, such as `com.example.MyTerm` | Bundle identifier |
| Linux | X11 `WM_CLASS` | Ignored |

Matching is case-insensitive. Linux cannot query the active window portably on Wayland,
so `extra_terminals` applies only to X11 there. These keys are configuration-file escape
hatches and are not parameters of the `set_config` MCP tool.

`extra_terminals` widens the terminal focus and transcript-injection gate; add only apps
that should be treated as terminals. `extra_editors` does not mark an app as a terminal.
On Windows and macOS it only suppresses a false no-paste-target warning for editors whose
custom-drawn text surface is invisible to the accessibility probe.

## Queue shape

Single FIFO (no reply vs narration split). Limits: 10 KiB/item; 128 items / 1 MiB global;
32 items / 256 KiB per session; overflow rejects. Pause, barge-in, focus-hold apply to
all items. Each `Item` has `session: Option<String>`; worker holds non-active or
no-terminal-frontmost items until the gate clears. Active session falls silent when quiet
until next `MarkActive`.
