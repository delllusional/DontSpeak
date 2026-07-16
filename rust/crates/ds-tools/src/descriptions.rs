//! Tool + parameter description strings — the human/LLM-facing text of the catalog, kept in
//! one place, separate from the tool structure in `lib.rs` (types, enums, required, order).
//! Canonical MCP descriptions; edit here without touching catalog wiring. Referenced from
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
pub const STOP_SPEECH: &str = "Stop this session's speech (or all if no session). Active \
    audio fades out.";

// ── mute ─────────────────────────────────────────────────────────────────────────────
pub const MUTE: &str = "Set global mute until changed or the engine restarts. While muted, \
    speech drains silently and earcons are suppressed.";
pub const MUTE_ON: &str = "True to mute, false to unmute.";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List available English voices by engine and language.";
pub const LIST_VOICES_ENGINE: &str = "Engine to inspect. Defaults to the configured speech \
    engine, or the built-in engine when speech is off.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Record the mic and return a transcript. Stops on end-of-speech or \
    the time limit.";
pub const LISTEN_SECONDS: &str = "Max recording time in seconds. Default 30.";

// ── get_status ───────────────────────────────────────────────────────────────────────────
pub const GET_STATUS: &str = "Get speech configuration and runtime state.";
pub const STATUS_DETAIL: &str = "Include model, dictation, and runtime stats. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record the mic and identify who spoke when. Diarization on; \
    macOS only.";
pub const DIARIZE_SECONDS: &str = "Recording time in seconds. Default 10.";

// ── manage_speakers ─────────────────────────────────────────────────────────────────────────
pub const MANAGE_SPEAKERS: &str = "List, enroll, or remove speaker voiceprints for diarize. \
    Re-enroll replaces. macOS only.";
pub const SPEAKERS_ACTION: &str = "Operation to perform.";
pub const SPEAKERS_NAME: &str = "Speaker name. Required for enroll and forget.";
pub const SPEAKERS_SECONDS: &str = "Enrollment recording time in seconds. Default 15.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Update one or more persistent settings atomically and reload them.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Speech engine: \"built_in\", \"system\", or \"off\". \
    Omit to keep the automatic preference. Unsupported engines are rejected.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered built-in voice IDs. First is default; rest are \
    the per-terminal pool.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str =
    "System-engine voice name; empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate. 1.0 = normal.";
pub const SET_CONFIG_NARRATE: &str = "Reply types to narrate. Default both: \"digests\" = \
    long-reply summaries; \"shorts\" = short replies in full. [] disables.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal in its pool voice. Default on.";
pub const SET_CONFIG_INPUT_CLEARS: &str = "Queues cleared on submit: \"current\" this terminal, \
    \"other\" all others (incl. global). Default [\"current\"]; [] clears none.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech while no terminal is frontmost; resume on focus. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str =
    "Reply-done sound name or path in an OS sound folder. Default: OS chime; empty = off.";
pub const SET_CONFIG_EARCON_INPUT: &str =
    "Needs-input cue: system-sound name or path. Default off.";
pub const SET_CONFIG_CAPS: &str = "Caps Lock tap-to-talk and speech cancel. Default on. Caps \
    still silences speech when dictation is off.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine: \"built_in\", \"system\", \
    \"claude_code\", or \"off\". Omit to keep the automatic preference. Unsupported or \
    unauthorized engines are rejected.";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain before recognition: \"auto\" (default) or a fixed 0.5–20.0 multiplier.";
pub const SET_CONFIG_DOUBLE_TAP_SUBMITS: &str = "Double tap submits and single tap inserts only. \
    Default false, which swaps those actions.";
pub const SET_CONFIG_PASTE_SUBMIT_DELAY_MS: &str = "Delay between paste and submit (ms). \
    Default 100; 0 submits immediately.";
pub const SET_CONFIG_PROVIDER: &str = "Compute providers in preference order; first usable wins. \
    Default [\"ane\",\"cuda\",\"cpu\"].";
pub const SET_CONFIG_DIARIZER: &str = "Diarization on/off: [\"apple_native\"] = on, \
    [] = off (default). macOS only.";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity; lower splits more speakers. Default 0.7.";
pub const SET_CONFIG_SPEAKER_THRESH: &str =
    "Min voiceprint match score; higher is stricter. Default 0.65.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe only enrolled speakers. Needs diarization \
    on and ≥1 enrolled voice. Built-in dictation only. Default off.";
pub const SET_CONFIG_FULL_DUPLEX: &str = "Keep mic open during replies with platform echo \
    cancellation. Default false; built-in dictation and speech only.";
pub const SET_CONFIG_TRAY: &str = "Speech states that color or animate the tray icon. Default \
    [\"stt\",\"tts_animated\"]; [] disables the indicator.";
