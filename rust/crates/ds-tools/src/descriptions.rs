//! Tool + parameter description strings — the human/LLM-facing text of the catalog, kept in
//! ONE place, separate from the tool STRUCTURE in `lib.rs` (types, enums, required, order).
//! These are the canonical MCP descriptions Claude reads, so they stay clean, concise, and
//! English; edit them here without touching the catalog wiring. Referenced by name from
//! `lib.rs`'s `TOOLS`.
//!
//! Describe WHAT each tool/setting does, not HOW. No model/runtime/framework names: the
//! engine behind a setting is per-platform and can change, so the text stays about behavior.

// ── speak ────────────────────────────────────────────────────────────────────────────
pub const SPEAK: &str = "Speak text aloud.";
pub const SPEAK_TEXT: &str = "The text to speak.";
pub const SPEAK_VOICE: &str = "Voice id (default: the configured voice).";
pub const SPEAK_RATE: &str = "Speed multiplier 0.5–2.0 (default: from config).";

// ── stop_speech ───────────────────────────────────────────────────────────────────────
pub const STOP_SPEECH: &str = "Stop any in-progress speech immediately.";

// ── mute ─────────────────────────────────────────────────────────────────────────────
pub const MUTE: &str = "Silence or restore ALL audio output — the app's global mute. Muted \
    replies and narration drain silently; queued earcons are suppressed and an active cue \
    stops without replaying on unmute. Persists, unlike stop_speech; get_status shows mute.";
pub const MUTE_ON: &str = "true = mute; false = unmute.";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List available voices, grouped by language (English only in this \
    build). Optional engine filter; default: the configured engine.";
pub const LIST_VOICES_ENGINE: &str =
    "Which engine's voices to list: \"built_in\" or \"system\". Default: the configured engine.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Open the mic and return the transcribed text. Auto-stops when the \
    speaker stops talking — no key press needed.";
pub const LISTEN_SECONDS: &str =
    "Hard upper bound in seconds (default 30); the mic normally stops on end-of-speech first.";

// ── get_status ───────────────────────────────────────────────────────────────────────────
pub const GET_STATUS: &str = "Report current state: engine, active voice, default rate, whether \
    speech is playing, queue length, paused, muted. Pass detail=true for per-engine model \
    status, dictation state, and stats.";
pub const STATUS_DETAIL: &str =
    "Per-engine model status, dictation state, and stats. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record the mic and return who spoke when: per-speaker time spans \
    (seconds), labelled with an enrolled name when matched. Needs diarization on (set_config \
    diarizer_provider). macOS-only.";
pub const DIARIZE_SECONDS: &str = "Seconds to record (default 10).";

// ── manage_speakers ─────────────────────────────────────────────────────────────────────────
pub const MANAGE_SPEAKERS: &str = "Manage the enrolled voiceprints diarize uses to name speakers. \
    list: show enrolled names. enroll: record the mic and learn the name (re-enrolling replaces \
    it). forget: remove the name. macOS-only.";
pub const SPEAKERS_ACTION: &str = "What to do: \"list\", \"enroll\", or \"forget\".";
pub const SPEAKERS_NAME: &str = "Speaker name — required for enroll and forget.";
pub const SPEAKERS_SECONDS: &str =
    "Seconds to record for enroll (default 15; longer/varied = stronger). Ignored otherwise.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Update persistent settings. All fields optional; provide at least \
    one. Validated, applied together, then hot-reloaded. To change the voice, set \
    tts_built_in_voices or tts_system_voice.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Spoken-reply engine: \"built_in\" (on-device) or \
    \"system\" (OS voice) to force exactly that engine, or \"off\" to turn spoken replies off. \
    Omit to keep the automatic preference (config-file only). Rejected if the engine isn't \
    usable on this platform/build.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered voice ids for the built-in engine — first is the \
    default, the rest a per-terminal pool. English ids only in this build. Built-in only.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str =
    "Voice name for the system engine; empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate 0.5–2.0 (1.0 = normal). Both engines.";
pub const SET_CONFIG_NARRATE: &str = "What to narrate aloud — any of [\"shorts\",\"digests\"] \
    (default both). \"digests\": speak the spoken digest of long replies. \"shorts\": also speak \
    short replies in full. [] = nothing.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal aloud in its pool voice. Default on.";
pub const SET_CONFIG_INPUT_CLEARS: &str = "Which sessions a submit (typed + Enter, or a \
    voice/dictation submit — how you submitted doesn't matter) clears pending speech for: any \
    of \"current\" (the submitting window) and \"other\" (every other window, including \
    untagged/global audio). Default [\"current\"]; [] = never.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech while no terminal is frontmost; resume on focus. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str = "Reply-done chime: system-sound name or path within an OS sound folder. Default: OS chime; empty = off.";
pub const SET_CONFIG_EARCON_INPUT: &str =
    "Needs-input cue: system-sound name or path within an OS sound folder. Default off.";
pub const SET_CONFIG_CAPS: &str = "Enable the Caps Lock handler — tap-to-talk dictation plus \
    silence/cancel. Default on. With dictation off (stt_engine=\"off\"), Caps still silences the \
    voice.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine: \"built_in\" (on-device), \"system\" \
    (OS recognizer, macOS only), or \"claude_code\" (Claude Code's voice key) to force exactly \
    that engine, or \"off\" to turn dictation off. Omit to keep the automatic preference \
    (config-file only). Rejected if the engine isn't usable on this platform/build; \"system\" \
    is also checked for on-device availability/authorization when set.";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain before recognition: \"auto\" (default) or a fixed 0.5–20.0 multiplier.";
pub const SET_CONFIG_DOUBLE_TAP_SUBMITS: &str = "Default false: a single tap submits (paste + \
    Return), a fast double tap only inserts. true swaps them.";
pub const SET_CONFIG_PASTE_SUBMIT_DELAY_MS: &str = "Delay (ms) between the paste and the Enter \
    that submits — lets the async clipboard paste settle before Enter. Default 100; 0 = instant.";
pub const SET_CONFIG_PROVIDER: &str = "Compute-backend ladder for speech output and recognition \
    (first usable wins): \"ane\" (on-device accelerator), \"cuda\" (GPU), \"coreml\" \
    (platform accelerator, speech output only), \"cpu\" (CPU). Default [\"ane\",\"cuda\",\"cpu\"].";
pub const SET_CONFIG_DIARIZER: &str = "Diarization runtime + on/off switch: [\"apple_native\"] = \
    on, [] = off (default). macOS-only.";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity 0.5–0.9 (default 0.7); lower splits more speakers apart.";
pub const SET_CONFIG_SPEAKER_THRESH: &str = "Match cutoff 0.0–1.0 (default 0.65) for labelling a \
    span with an enrolled name; higher = stricter.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe only enrolled speaker(s), dropping others \
    — needs diarization on and ≥1 enrolled voice. Built-in dictation only. Default off.";
pub const SET_CONFIG_FULL_DUPLEX: &str = "Keep the mic open while replies play, using platform \
    echo cancellation, instead of closing it during speech. Default false; only takes effect \
    with built-in dictation and built-in speech output.";
pub const SET_CONFIG_TRAY: &str = "Tray icon: which states color it and whether it pulses — \
    any of [\"stt\",\"tts\",\"stt_animated\",\"tts_animated\"] (default [\"stt\",\"tts_animated\"]). \
    [] = never color.";
