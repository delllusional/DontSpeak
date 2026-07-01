# Build & deploy — what a code change actually deploys to

The macOS app **hosts the engine in-process** and spawns a **warm child** for synthesis. So a
single repo has three runtime pieces that deploy by DIFFERENT routes — and using the wrong
route leaves the app running stale code that *looks* installed. This bit us hard (a TTS fix
that "didn't work" was simply never deployed to the running helper).

## The three pieces and how each reaches the running app

| Piece | What it is | Built+installed by | What the RUNNING APP actually executes |
|---|---|---|---|
| `dontspeak` | the CLI: MCP server **and** the Claude Code hook entries (`notify`/`provide`) | `install-daemon.sh` → `~/.local/bin/dontspeak` | **`~/.local/bin/dontspeak`** — the wired hook command points here, so this IS live after install-daemon |
| `ds-helper` | the warm TTS/STT synthesis child | `install-daemon.sh` → `~/.local/bin/ds-helper` **AND** `bundle.sh` → `DontSpeak.app/Contents/MacOS/ds-helper` | **the BUNDLED copy** — the app spawns `Contents/MacOS/ds-helper`. The `~/.local/bin` copy is NOT used by the app. |
| engine (`dontspeakd` logic) | the in-process engine (queue, IPC, playback) | `bundle.sh` → linked into the `DontSpeak.app` binary | **the app binary** — the engine is linked in and runs in-process; there is no standalone `dontspeakd` binary |

### The rule

- **Hook / MCP-surface change** (anything in the `dontspeak` binary — the `notify`/`provide`
  hook routing, `mcp`/`tools`, config parsing read by the hook): `install-daemon.sh` is
  enough. The hooks invoke
  `~/.local/bin/dontspeak` fresh each time, so it's live immediately (re-run `wire claude_code`
  only if the hook SET changed).
- **Engine or helper change** (anything in `dontspeakd`, `ds-tts` helper, `ds-stt`,
  the TTS queue/synth/chunking, IPC handlers): you **MUST** run the full **`./apps/macos/bundle.sh`**
  (then relaunch the app). `install-daemon.sh` does NOT update the bundled helper or the
  in-process engine, so the change will not take effect no matter how many times you rebuild.

A quick manual shortcut during iteration (avoids a full `bundle.sh`): build the one binary,
copy it into the bundle, **re-sign**, relaunch:

```sh
cargo build --release -p ds-tts --bin ds-helper --manifest-path rust/Cargo.toml
osascript -e 'quit app "DontSpeak"'; pkill -9 -f dontspeak
cp rust/target/release/ds-helper "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"
codesign --force --sign - "$HOME/Applications/DontSpeak.app/Contents/MacOS/ds-helper"  # REQUIRED — a copied binary is SIGKILLed until re-signed
open "$HOME/Applications/DontSpeak.app"
```

But prefer a real `bundle.sh` before you conclude a change works — the manual copy is easy to
forget and leaves a half-stale app.

### Symptom → diagnosis

A source fix that "has no effect" on synthesis/queue/IPC, while the binary and tests are
clearly updated → the **app is running its stale bundled helper / engine**. Confirm by
functional probe, not by `strings` (release binaries are stripped, so grepping for a symbol or
a test-only string gives false negatives). E.g. for the chunker: fire a long `speak` and check
`~/Library/Logs/ds-helper.log` for `phonemeSequenceTooLong` — present = stale helper.

## Debugging Claude Code hooks (ground-truth, don't guess)

When a hook "isn't firing" or a payload field seems missing, capture the RAW event instead of
guessing the schema. Temporarily append to a file at the top of the `notify` entry:

```rust
// in main.rs, the `notify` branch, before hook_core::notify(...)
let _ = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/ds_hooks.log")
    .map(|mut f| { use std::io::Write as _; writeln!(f, "[{}] {}", hook_core::event_name(&payload), payload.replace('\n'," ")); });
```

Then install + trigger a tool. Ground truths learned this way:

- **PostToolUse fires and the payload is rich**: `{ hook_event_name, session_id, tool_name,
  tool_input{...}, tool_response{...}, permission_mode, cwd, ... }`. For Bash, `tool_input`
  carries both `command` and the human `description` — the latter is the best spoken cue.
- The hook runs as a **fresh short-lived process** and loads `VoiceConfig` from
  `Paths::resolve().config_toml` = `~/Library/Application Support/DontSpeak/config.toml`. A
  stale `~/Library/Application Support/org.dontspeak.DontSpeak/config.toml` may also exist (old
  bundle id, an earlier config format) — ignore it; the live dir is `DontSpeak/`.

## Config-default asymmetry can MASK a deploy/read bug

`narrate` defaults to BOTH kinds ON (`["shorts", "digests"]`), but most other flags default
OFF (`greet_on_open`, `full_duplex`, the needs-input earcon, …). So a config read from the
wrong path, or not written where the reader looks, leaves default-on narration *working* while
a default-false opt-in is silently *off* — which reads as "the new feature is broken" when the
real fault is the config path or a stale deploy. When an opt-in is silent, FIRST confirm the
reader sees it set (log `cfg.<field>` + `paths.config_toml`) before touching the feature logic.

## TTS phoneme cap — both engines must chunk

The Core ML (FluidAudio) TTS chain has a fixed phoneme-input limit and **drops the whole
utterance** over it (`phonemeSequenceTooLong`); the ONNX path batches phonemes internally so it
never hit this. Any text bound for synthesis MUST go through the one shared text splitter
`ds_tts::batch::chunk_text` (bounds every chunk ≤ `TEXT_CHUNK_CHARS`, hard-splitting even
unpunctuated runs) before either engine — see `serve.rs`'s playback loop. Don't add a synthesis
path that bypasses it.
