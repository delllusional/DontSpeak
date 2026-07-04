# Build & deploy — what a code change actually deploys to

The macOS app **hosts the engine in-process** and spawns a **warm child** for
synthesis, so one repo has three runtime pieces that deploy by three different
routes. Rebuilding the wrong one leaves the app running stale code that *looks*
installed — always check which route a change needs before concluding it works.

## The three pieces and how each reaches the running app

| Piece | What it is | Built+installed by | What the RUNNING APP actually executes |
|---|---|---|---|
| `dontspeak` | the CLI: MCP server **and** the Claude Code hook entries (`notify`/`provide`) | `install-daemon.sh` → `~/.local/bin/dontspeak` | **`~/.local/bin/dontspeak`** — the wired hook command points here, so this IS live after install-daemon |
| `ds-helper` | the warm TTS/STT synthesis child | `install-daemon.sh` → `~/.local/bin/ds-helper` **AND** `bundle.sh` → `DontSpeak.app/Contents/MacOS/ds-helper` | **the BUNDLED copy** — the app spawns `Contents/MacOS/ds-helper`; the `~/.local/bin` copy is not what the app uses |
| engine (`dontspeakd` logic) | the in-process engine (queue, IPC, playback) | `bundle.sh` → linked into the `DontSpeak.app` binary | **the app binary** — the engine is linked in and runs in-process |

## The rule

- **Hook / MCP-surface change** (the `notify`/`provide` hook routing, `mcp`/`tools`,
  config parsing read by the hook): `install-daemon.sh` is enough — hooks invoke
  `~/.local/bin/dontspeak` fresh each time, so it's live immediately (re-run
  `wire claude_code` only if the hook set changed).
- **Engine or helper change** (`dontspeakd`, `ds-tts`/`ds-stt`, the TTS
  queue/synth/chunking, IPC handlers): run the full `./apps/macos/bundle.sh`, then
  relaunch the app. `install-daemon.sh` alone does not update the bundled helper or
  the in-process engine.

For fast iteration, a manual copy-and-resign can stand in for a full `bundle.sh`:

```sh
cargo build --release -p ds-helper --manifest-path rust/Cargo.toml
osascript -e 'quit app "DontSpeak"'; pkill -9 -f dontspeak
cp rust/target/release/ds-helper "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"
codesign --force --sign - "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"  # required — a copied binary is SIGKILLed until re-signed
open "$HOME/Applications/DontSpeak.app"
```

### Symptom → diagnosis

A source fix that "has no effect" on synthesis/queue/IPC while the binary and tests
are clearly updated means the app is running its stale bundled helper or engine.
Confirm with a functional probe rather than `strings` (release binaries are stripped,
so grepping for a symbol gives false negatives) — e.g. fire a long `speak` and check
`~/Library/Logs/ds-helper.log` for `phonemeSequenceTooLong` to catch a stale helper.

## Debugging Claude Code hooks

When a hook payload seems to be missing a field, capture the raw event instead of
guessing the schema — temporarily log it at the top of the `notify` entry in
`main.rs`, before `hook_core::notify(...)` runs, then install and trigger a tool.

Ground truths from doing this: `PostToolUse` fires with a rich payload —
`{ hook_event_name, session_id, tool_name, tool_input{...}, tool_response{...},
permission_mode, cwd, ... }` (for Bash, `tool_input.description` is the best spoken
cue). The hook itself is a fresh short-lived process that loads `VoiceConfig` from
`Paths::resolve().config_toml`, i.e. `~/Library/Application Support/DontSpeak/config.toml`
— the live directory is `DontSpeak/`, not the older `org.dontspeak.DontSpeak/`.

## Config defaults can mask a deploy/read bug

`narrate` defaults to both kinds on (`["shorts", "digests"]`); most other flags
(`greet_on_open`, `full_duplex`, the needs-input earcon, …) default off. A config read
from the wrong path can leave default-on narration working while a default-off opt-in
stays silently off — reading as "the new feature is broken" when the real fault is
the config path or a stale deploy. When an opt-in stays silent, confirm the reader
sees it set (log `cfg.<field>` and `paths.config_toml`) before touching feature logic.

## TTS phoneme cap — both engines must chunk

The Core ML (FluidAudio) TTS chain has a fixed phoneme-input limit and drops the
whole utterance over it (`phonemeSequenceTooLong`); the ONNX path batches phonemes
internally so it never hits this. Route any text bound for synthesis through the one
shared splitter, `ds_tts::batch::chunk_text` (bounds every chunk to `TEXT_CHUNK_CHARS`,
hard-splitting even unpunctuated runs), before either engine — see `serve.rs`'s
playback loop.
