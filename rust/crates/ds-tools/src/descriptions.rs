//! MCP / Tools-tab wire names + bare description prose (token-cost minimal).

// ── tool wire names (Tools tab titles; MCP tool names) ──────────────────────

pub const SPEAK_NAME: &str = "speak";
pub const LISTEN_NAME: &str = "listen";
pub const STOP_NAME: &str = "stop";
pub const MUTE_NAME: &str = "mute";
pub const STATUS_NAME: &str = "status";
pub const USAGE_NAME: &str = "usage";
pub const VOICES_NAME: &str = "voices";
pub const MODELS_NAME: &str = "models";
pub const DIARIZE_NAME: &str = "diarize";
pub const MANAGE_SPEAKERS_NAME: &str = "manage_speakers";
pub const SET_CONFIG_NAME: &str = "set_config";

// ── param wire names ────────────────────────────────────────────────────────

pub const TEXT: &str = "text";
pub const TTS_ARGS: &str = "tts_args";
pub const SECONDS: &str = "seconds";
pub const ON: &str = "on";
pub const DETAIL: &str = "detail";
pub const SINCE: &str = "since";
pub const TIMEOUT_MS: &str = "timeout_ms";
pub const REFRESH: &str = "refresh";
pub const TTS_ENGINE: &str = "tts_engine";
pub const TTS_MODEL: &str = "tts_model";
pub const PREFERRED_LANGUAGES: &str = "preferred_languages";
pub const LANGUAGE: &str = "language";
pub const REMOVE: &str = "remove";
pub const ACTION: &str = "action";
pub const NAME: &str = "name";
pub const TTS_VOICES: &str = "tts_voices";
pub const TTS_PARAMS: &str = "tts_params";
pub const NARRATE: &str = "narrate";
pub const GREET: &str = "greet";
pub const CLEAR_ON_INPUT: &str = "clear_on_input";
pub const PAUSE_BG: &str = "pause_bg";
pub const EARCON_REPLY: &str = "earcon_reply";
pub const EARCON_INPUT: &str = "earcon_input";
pub const CAPS: &str = "caps";
pub const STT_ENGINE: &str = "stt_engine";
pub const CAPTURE_GAIN: &str = "capture_gain";
pub const DOUBLE_TAP_SUBMIT: &str = "double_tap_submit";
pub const PASTE_DELAY_MS: &str = "paste_delay_ms";
pub const FULL_DUPLEX: &str = "full_duplex";
pub const PROVIDER: &str = "provider";
pub const DIARIZER: &str = "diarizer";
pub const ACTIVITY_THRESHOLD: &str = "activity_threshold";
pub const MATCH_THRESHOLD: &str = "match_threshold";
pub const SPEAKER_LOCK: &str = "speaker_lock";
pub const TRAY: &str = "tray";
pub const AGENTS: &str = "agents";

// ── tool descriptions ───────────────────────────────────────────────────────

pub const SPEAK: &str = "Queue text for spoken playback.";
pub const LISTEN: &str = "Record mic to transcript.";
pub const STOP: &str = "Stop this MCP connection's speech.";
pub const MUTE: &str = "Global mute until changed or engine restart.";
pub const STATUS: &str = "Speech config and runtime state.";
pub const USAGE: &str = "Coding-agent subscription usage.";
pub const VOICES: &str = "List languages and voices.";
pub const MODELS: &str = "Built-in models: capabilities, disk usage, and removal.";
pub const DIARIZE: &str = "Record and label speakers. macOS only.";
pub const MANAGE_SPEAKERS: &str = "List, enroll, or forget diarize voiceprints. macOS only.";
pub const SET_CONFIG: &str = "Update and reload settings.";

// ── param descriptions ──────────────────────────────────────────────────────

pub const SPEAK_TEXT: &str = "Text to speak.";
pub const SPEAK_TTS_ARGS: &str =
    "Per-target voice/language/params for this utterance. See voices and models.";
pub const SPEAK_KOKORO_VOICE: &str =
    "Kokoro voice ID. Use the `voices` tool to list currently accepted values.";
pub const SPEAK_SYSTEM_VOICE: &str = "Installed OS voice name. The `voices` tool lists installed names on macOS; Windows accepts an installed SAPI voice name without tool enumeration.";

pub const MUTE_ON: &str = "True to mute, false to unmute.";

pub const VOICES_ENGINE: &str = "Engine to inspect.";
pub const VOICES_MODEL: &str = "Built-in model to inspect.";
pub const VOICES_LANGUAGE: &str = "Language to inspect.";

pub const MODELS_REMOVE: &str = "Model or shared asset to delete from the cache. The active model, and a shared asset something still needs, are refused.";

pub const LISTEN_SECONDS: &str = "Max recording seconds. Default 30.";

pub const STATUS_DETAIL: &str = "Include model, dictation, and runtime stats.";
pub const STATUS_SINCE: &str = "Long-poll until status sequence changes from this value.";
pub const STATUS_TIMEOUT_MS: &str = "Long-poll max wait ms when since is set. Default 30000.";

pub const USAGE_REFRESH: &str = "Bypass 60s cache. Default false.";

pub const DIARIZE_SECONDS: &str = "Recording seconds. Default 10.";

pub const SPEAKERS_ACTION: &str = "list | enroll | forget.";
pub const SPEAKERS_NAME: &str = "Speaker name for enroll/forget.";
pub const SPEAKERS_SECONDS: &str = "Enrollment seconds. Default 15.";

pub const SET_CONFIG_TTS_ENGINE: &str = "Speech engine. Omit to keep the automatic preference.";
pub const SET_CONFIG_TTS_MODEL: &str = "Built-in model.";
pub const SET_CONFIG_PREFERRED_LANGUAGES: &str =
    "Language detection scope (ISO 639-1). [] = auto-detect (default).";
pub const SET_CONFIG_TTS_VOICES: &str = "Voice pools by target. `system: []` uses the OS default.";
pub const SET_CONFIG_TTS_PARAMS: &str =
    "Param objects by target. rate default 1.0 (system/kokoro only). `{}` resets.";
pub const SET_CONFIG_NARRATE: &str = "Narration modes. Default both.";
pub const SET_CONFIG_GREET: &str = "Greet new terminals. Default on.";
pub const SET_CONFIG_INPUT_CLEARS: &str = "Queues to clear on submit. `current` = the submitting terminal, `other` = everything else (incl. untagged). Default [\"current\"].";
pub const SET_CONFIG_PAUSE_BG: &str = "Pause speech when no terminal is frontmost. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str = "Reply-done sound. Default: OS chime; empty = off.";
pub const SET_CONFIG_EARCON_INPUT: &str = "Needs-input sound. Default off.";
pub const SET_CONFIG_CAPS: &str = "Caps Lock PTT and speech cancel. Default on.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine. Omit to keep the automatic preference.";
pub const SET_CONFIG_CAPTURE_GAIN: &str = "Mic gain: \"auto\" (default) or 0.5–20.0.";
pub const SET_CONFIG_DOUBLE_TAP_SUBMITS: &str = "Double-tap submits. Default false.";
pub const SET_CONFIG_PASTE_SUBMIT_DELAY_MS: &str = "Paste→submit delay ms. Default 100.";
pub const SET_CONFIG_PROVIDER: &str = "Compute provider order. Default [\"mlx\",\"cuda\",\"cpu\"].";
pub const SET_CONFIG_DIARIZER: &str = "Diarization providers. [] = off (default).";
pub const SET_CONFIG_ACTIVITY_THRESHOLD: &str = "Speaker-activity cutoff. Default 0.5.";
pub const SET_CONFIG_SPEAKER_THRESH: &str = "Voiceprint match threshold. Default 0.65.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Only enrolled speakers. Default off.";
pub const SET_CONFIG_FULL_DUPLEX: &str = "Mic open during replies. Default false.";
pub const SET_CONFIG_TRAY: &str = "Tray speech states. Default [\"stt\",\"tts_animated\"].";
pub const SET_CONFIG_AGENTS: &str = "Agents tab and usage tool. Off by default.";
