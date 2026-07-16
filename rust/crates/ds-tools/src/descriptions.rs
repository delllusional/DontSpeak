//! Tool + parameter description strings — human/LLM-facing catalog text, separate from
//! structure in `lib.rs`. Canonical MCP descriptions; edit here. Referenced from `TOOLS`.
//!
//! WHAT not HOW. No model/runtime names (platform-specific engines can change).

// ── speak ────────────────────────────────────────────────────────────────────────────
pub const SPEAK: &str = "Queue text for spoken playback.";
pub const SPEAK_TEXT: &str = "Text to speak.";
pub const SPEAK_VOICE: &str = "Voice ID. Defaults to the configured voice.";
pub const SPEAK_RATE: &str = "Playback speed. Defaults to the configured rate.";

// ── stop_speech ───────────────────────────────────────────────────────────────────────
pub const STOP_SPEECH: &str = "Stop this session's speech, or all speech if no session. Fades out.";

// ── mute ─────────────────────────────────────────────────────────────────────────────
pub const MUTE: &str = "Global mute until changed or engine restart. Muted speech drains \
    silently; earcons off.";
pub const MUTE_ON: &str = "True to mute, false to unmute.";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List English voices by engine and language.";
pub const LIST_VOICES_ENGINE: &str = "Engine to inspect. Defaults to configured speech engine, \
    or built-in when speech is off.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Record the mic → transcript. Stops on end-of-speech or time limit.";
pub const LISTEN_SECONDS: &str = "Max recording seconds. Default 30.";

// ── get_status ───────────────────────────────────────────────────────────────────────────
pub const GET_STATUS: &str = "Speech config and runtime state.";
pub const STATUS_DETAIL: &str = "Include model, dictation, and runtime stats. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record mic and label who spoke when. Needs diarization; macOS only.";
pub const DIARIZE_SECONDS: &str = "Recording seconds. Default 10.";

// ── manage_speakers ─────────────────────────────────────────────────────────────────────────
pub const MANAGE_SPEAKERS: &str = "List, enroll, or remove diarize voiceprints. Re-enroll \
    replaces. macOS only.";
pub const SPEAKERS_ACTION: &str = "list | enroll | forget.";
pub const SPEAKERS_NAME: &str = "Speaker name (required for enroll/forget).";
pub const SPEAKERS_SECONDS: &str = "Enrollment seconds. Default 15.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Atomically update and reload persistent settings.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Speech: \"built_in\", \"system\", or \"off\". Omit to \
    keep the automatic preference. Unsupported engines are rejected.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered built-in voice IDs. First = default; rest = \
    per-terminal pool.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str =
    "System voice name; empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate. 1.0 = normal.";
pub const SET_CONFIG_NARRATE: &str = "What to narrate. Default both: \"digests\" = long-reply \
    summaries; \"shorts\" = short replies whole. [] off.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal in its pool voice. Default on.";
pub const SET_CONFIG_INPUT_CLEARS: &str = "Queues to clear on submit: \"current\" this terminal, \
    \"other\" the rest (incl. global). Default [\"current\"]; [] none.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech when no terminal is frontmost. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str =
    "Reply-done sound name/path. Default: OS chime; empty = off.";
pub const SET_CONFIG_EARCON_INPUT: &str =
    "Needs-input sound name/path. Default off.";
pub const SET_CONFIG_CAPS: &str = "Caps Lock PTT and speech cancel. Default on. Still silences \
    speech when dictation is off.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation: \"built_in\", \"system\", \"claude_code\", \
    or \"off\". Omit to keep the automatic preference. Unsupported/unauthorized rejected.";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain: \"auto\" (default) or 0.5–20.0 fixed.";
pub const SET_CONFIG_DOUBLE_TAP_SUBMITS: &str = "Double-tap submits; single-tap inserts only. \
    Default false (swaps those).";
pub const SET_CONFIG_PASTE_SUBMIT_DELAY_MS: &str = "Paste→submit delay (ms). Default 100; 0 = \
    immediate.";
pub const SET_CONFIG_PROVIDER: &str = "Compute provider preference order. Default \
    [\"ane\",\"cuda\",\"cpu\"].";
pub const SET_CONFIG_DIARIZER: &str = "Diarization: [\"apple_native\"] on, [] = off (default). \
    macOS only.";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity; lower → more speakers. Default 0.7.";
pub const SET_CONFIG_SPEAKER_THRESH: &str =
    "Min voiceprint match; higher → stricter. Default 0.65.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe enrolled speakers only. Needs diarization \
    + ≥1 voice. Built-in STT only. Default off.";
pub const SET_CONFIG_FULL_DUPLEX: &str = "Mic open during replies (platform AEC). Default false; \
    built-in STT+TTS only.";
pub const SET_CONFIG_TRAY: &str = "Tray icon speech states. Default [\"stt\",\"tts_animated\"]; \
    [] off.";
