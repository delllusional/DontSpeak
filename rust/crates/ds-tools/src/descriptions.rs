//! Tool + parameter description strings — the human/LLM-facing text of the catalog, kept in
//! ONE place, separate from the tool STRUCTURE in `lib.rs` (types, enums, required, order).
//! These are the canonical MCP descriptions Claude reads, so they stay clean, concise, and
//! English; edit them here without touching the catalog wiring. Referenced by name from
//! `lib.rs`'s `TOOLS`.
//!
//! Describe WHAT each tool/setting does, not HOW. No model/runtime/framework names: the
//! engine behind a setting is per-platform and can change, so the text stays about behavior.

// ── speak ────────────────────────────────────────────────────────────────────────────
pub const SPEAK: &str = "Queue text for spoken playback.";
pub const SPEAK_TEXT: &str = "Text to speak.";
pub const SPEAK_VOICE: &str = "Voice ID. Defaults to the configured voice.";
pub const SPEAK_RATE: &str = "Playback speed. Defaults to the configured rate.";

// ── stop_speech ───────────────────────────────────────────────────────────────────────
pub const STOP_SPEECH: &str = "Stop queued and active speech for this terminal session. If no \
    session identity is available, stop all speech. Active audio fades out.";

// ── mute ─────────────────────────────────────────────────────────────────────────────
pub const MUTE: &str = "Set global audio mute. While muted, speech drains silently and earcons \
    are suppressed. The setting lasts until changed or the DontSpeak engine restarts.";
pub const MUTE_ON: &str = "Set true to mute or false to unmute.";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List available English voices by engine and language.";
pub const LIST_VOICES_ENGINE: &str = "Engine to inspect. Defaults to the configured speech engine, \
    or the built-in engine when speech is off.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Record the microphone and return a transcript. Stops after speech ends \
    or the time limit is reached.";
pub const LISTEN_SECONDS: &str = "Maximum recording time in seconds. Default 30.";

// ── get_status ───────────────────────────────────────────────────────────────────────────
pub const GET_STATUS: &str = "Get speech configuration and runtime state.";
pub const STATUS_DETAIL: &str = "Include model, dictation, and runtime statistics. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record the microphone and identify who spoke when. Requires enabled \
    diarization and is available only on macOS.";
pub const DIARIZE_SECONDS: &str = "Recording time in seconds. Default 10.";

// ── manage_speakers ─────────────────────────────────────────────────────────────────────────
pub const MANAGE_SPEAKERS: &str = "List, enroll, or remove speaker voiceprints used by diarize. \
    Re-enrolling a name replaces it. Available only on macOS.";
pub const SPEAKERS_ACTION: &str = "Operation to perform.";
pub const SPEAKERS_NAME: &str = "Speaker name. Required for enroll and forget.";
pub const SPEAKERS_SECONDS: &str = "Enrollment recording time in seconds. Default 15.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Update one or more persistent settings atomically and reload them.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Speech engine: \"built_in\", \"system\", or \"off\". \
    Omit to keep the automatic preference. Unsupported engines are rejected.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered built-in voice IDs. The first is the default; \
    remaining voices form the per-terminal pool.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str =
    "Voice name for the system engine; empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate. 1.0 = normal.";
pub const SET_CONFIG_NARRATE: &str = "Reply types to narrate. Default both: \"digests\" speaks \
    long-reply summaries; \"shorts\" speaks short replies in full. [] disables narration.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal aloud in its pool voice. Default on.";
pub const SET_CONFIG_INPUT_CLEARS: &str = "Speech queues cleared when input is submitted: \
    \"current\" for this terminal and \"other\" for all others, including global audio. Default \
    [\"current\"]; [] clears none.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech while no terminal is frontmost; resume on focus. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str =
    "Reply-complete sound name or path within an OS sound folder. Default: OS chime; empty = off.";
pub const SET_CONFIG_EARCON_INPUT: &str =
    "Needs-input cue: system-sound name or path within an OS sound folder. Default off.";
pub const SET_CONFIG_CAPS: &str = "Enable Caps Lock tap-to-talk and speech cancellation. Default \
    on. Caps still silences speech when dictation is off.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine: \"built_in\", \"system\", \
    \"claude_code\", or \"off\". Omit to keep the automatic preference. Unsupported or \
    unauthorized engines are rejected.";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain before recognition: \"auto\" (default) or a fixed 0.5–20.0 multiplier.";
pub const SET_CONFIG_DOUBLE_TAP_SUBMITS: &str = "Whether a double tap submits and a single tap \
    only inserts. Default false, which swaps those actions.";
pub const SET_CONFIG_PASTE_SUBMIT_DELAY_MS: &str = "Delay between paste and submit, in \
    milliseconds. Default 100; 0 submits immediately.";
pub const SET_CONFIG_PROVIDER: &str = "Compute providers in preference order; the first usable \
    provider wins. Default [\"ane\",\"cuda\",\"cpu\"].";
pub const SET_CONFIG_DIARIZER: &str = "Diarization runtime + on/off switch: [\"apple_native\"] = \
    on, [] = off (default). macOS-only.";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity; lower values split more speakers. Default 0.7.";
pub const SET_CONFIG_SPEAKER_THRESH: &str = "Minimum voiceprint match score; higher values are \
    stricter. Default 0.65.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe only enrolled speaker(s), dropping others \
    — needs diarization on and ≥1 enrolled voice. Built-in dictation only. Default off.";
pub const SET_CONFIG_FULL_DUPLEX: &str = "Keep the mic open while replies play, using platform \
    echo cancellation, instead of closing it during speech. Default false; only takes effect \
    with built-in dictation and built-in speech output.";
pub const SET_CONFIG_TRAY: &str = "Speech states that color or animate the tray icon. Default \
    [\"stt\",\"tts_animated\"]; [] disables the indicator.";
