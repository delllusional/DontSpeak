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

// ── stop_speak ───────────────────────────────────────────────────────────────────────
pub const STOP_SPEAK: &str = "Stop any in-progress speech immediately.";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List available voices, grouped by language (English only in this \
    build). Optional engine filter; defaults to the configured engine.";
pub const LIST_VOICES_ENGINE: &str =
    "Which engine's voices to list: \"built_in\" or \"system\". Default: the configured engine.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Open the mic and return the transcribed text. Auto-stops when the \
    speaker stops talking, so you can ask a question mid-turn and get the spoken answer back \
    without anyone pressing a key.";
pub const LISTEN_SECONDS: &str =
    "Hard upper bound in seconds (default 30); the mic normally stops on end-of-speech first.";

// ── status ───────────────────────────────────────────────────────────────────────────
pub const STATUS: &str = "Report current state: engine, active voice, default rate, whether \
    speech is playing, queue length, and paused. Pass detail:true to also include per-engine \
    model status, dictation state, and stats.";
pub const STATUS_DETAIL: &str =
    "Also include per-engine model status, dictation state, and stats. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record the mic and return who spoke when: per-speaker time spans in \
    seconds, each labelled with an enrolled name when it matches one (see enroll). Needs \
    diarization on (set_config diarizer_provider). macOS-only.";
pub const DIARIZE_SECONDS: &str = "Seconds to record (default 10).";

// ── enroll ───────────────────────────────────────────────────────────────────────────
pub const ENROLL: &str = "Record the mic and save a speaker's voiceprint under name, so future \
    diarize calls label that person. Re-enrolling the same name replaces it. macOS-only.";
pub const ENROLL_NAME: &str = "Name/label for this voiceprint.";
pub const ENROLL_SECONDS: &str = "Seconds to record (default 15; longer/varied = stronger).";

// ── forget_speaker ───────────────────────────────────────────────────────────────────
pub const FORGET_SPEAKER: &str = "Remove an enrolled voiceprint by name (no-op if absent).";
pub const FORGET_SPEAKER_NAME: &str = "The enrolled name to remove.";

// ── list_speakers ────────────────────────────────────────────────────────────────────
pub const LIST_SPEAKERS: &str = "List enrolled speaker names.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Update persistent settings. All fields optional; provide at least \
    one. Validated, applied together, then hot-reloaded. To change the voice, set \
    tts_built_in_voices or tts_system_voice.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Spoken-reply engine as a preference ladder (first usable \
    wins): \"built_in\" (on-device) and \"system\" (OS voice). Default [\"built_in\",\"system\"]. \
    [] or [\"off\"] = no spoken replies.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered voice ids for the built-in engine — first is the \
    default, the rest a per-terminal pool. English ids only in this build. Built-in only.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str =
    "Voice name for the system engine (e.g. \"Samantha\"); empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate 0.5–2.0 (1.0 = normal). Both engines.";
pub const SET_CONFIG_NARRATE: &str = "What to narrate aloud — any of [\"shorts\",\"digests\"] \
    (default both). \"digests\": speak the spoken digest of long replies. \"shorts\": also speak \
    short replies in full. [] = nothing.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal aloud in its pool voice. Default on.";
pub const SET_CONFIG_DROP_SPEECH: &str = "Drop a window's pending speech on submit — any of \
    \"voice\" (dictation submit) and \"text\" (typed + Enter). Default [\"text\"]; [] = never. \
    Text needs the UserPromptSubmit hook.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech while no terminal is frontmost; resume on focus. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str =
    "Reply-done chime: a system-sound name or absolute path; empty = off. Defaults to the OS chime.";
pub const SET_CONFIG_EARCON_INPUT: &str =
    "Needs-input cue: a system-sound name or absolute path; empty = off (default).";
pub const SET_CONFIG_CAPS: &str = "Enable the Caps Lock handler — push-to-talk dictation plus \
    silence/cancel. Default on. With stt_engine=off, Caps still silences the voice.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine as a preference ladder (first usable \
    wins): \"built_in\" (on-device), \"system\" (OS recognizer), \"claude_code\" (Claude Code's \
    voice key). Default [\"built_in\",\"system\",\"claude_code\"]. [] or [\"off\"] = dictation off.";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain before recognition: \"auto\" (default) or a fixed 0.5–20.0 multiplier.";
pub const SET_CONFIG_AUTO_SUBMIT: &str =
    "Press Return after pasting dictation. Default true; false = insert only.";
pub const SET_CONFIG_PROVIDER: &str = "Compute-backend ladder for speech output and recognition \
    (first usable wins): \"ane\" (on-device accelerator), \"ort_cuda\" (GPU), \"ort_coreml\" \
    (platform accelerator), \"ort_cpu\" (CPU). Default [\"ane\",\"ort_cuda\",\"ort_cpu\"].";
pub const SET_CONFIG_DIARIZER: &str =
    "Diarization runtime + on/off switch: [\"apple_native\"] = on, [] = off (default). macOS.";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity 0.5–0.9 (default 0.7); lower splits more speakers apart.";
pub const SET_CONFIG_SPEAKER_THRESH: &str = "Match cutoff 0.0–1.0 (default 0.65) for labelling a \
    span with an enrolled name; higher = stricter.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe only enrolled speaker(s) — needs diarization \
    on and ≥1 enrolled voice; other voices are dropped. Built-in dictation only. Default off.";
pub const SET_CONFIG_TRAY: &str = "Menu-bar pill: which states color it and whether it pulses — \
    any of [\"stt\",\"tts\",\"stt_animated\",\"tts_animated\"] (default [\"stt\",\"tts_animated\"]). \
    [] = never color.";

// ── wire ─────────────────────────────────────────────────────────────────────────────
pub const WIRE: &str = "Write a config file, or register/remove a client integration (the same \
    setup the installer does, anytime). Targets: \"narration_spec\", \"claude_code\", \
    \"claude_desktop\", \"codex\". Additive and backed up; enabled=false removes only our entry.";
pub const WIRE_TARGET: &str = "What to wire: the narration spec, or a client integration.";
pub const WIRE_ENABLED: &str = "true = register, false = remove.";
