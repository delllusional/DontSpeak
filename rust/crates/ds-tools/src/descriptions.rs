//! Tool + parameter description strings — the human/LLM-facing text of the catalog, kept in
//! ONE place, separate from the tool STRUCTURE in `lib.rs` (types, enums, required, order).
//! These are the canonical MCP descriptions Claude reads, so they stay clean, concise, and
//! English; edit them here without touching the catalog wiring. Referenced by name from
//! `lib.rs`'s `TOOLS`.

// ── speak ────────────────────────────────────────────────────────────────────────────
pub const SPEAK: &str = "Speak text aloud via local text-to-speech (Kokoro or the system voice).";
pub const SPEAK_TEXT: &str = "The text to speak.";
pub const SPEAK_VOICE: &str = "Voice id (default: the configured voice).";
pub const SPEAK_RATE: &str = "Speed multiplier 0.5–2.0 (default: from config).";

// ── stop_speak ───────────────────────────────────────────────────────────────────────
pub const STOP_SPEAK: &str = "Stop any in-progress speech immediately (barge-in).";

// ── list_voices ──────────────────────────────────────────────────────────────────────
pub const LIST_VOICES: &str = "List TTS voices grouped by language. Returns \
    {engine, language, languages:[{language, voices:[{id,label,language_tag,gender,engine,active}]}]}, \
    where language_tag is the full BCP-47 tag (e.g. \"en-US\"). Optional tts_engine and language \
    filters; defaults to the configured engine and English.";
pub const LIST_VOICES_ENGINE: &str =
    "Engine whose voices to list: \"built_in\" (Kokoro) or \"system\" (OS). Default: the configured engine.";
pub const LIST_VOICES_LANGUAGE: &str =
    "BCP-47 primary subtag (e.g. \"en\", \"fr\", \"ja\") or \"all\". Default: \"en\".";

// ── set_voice ────────────────────────────────────────────────────────────────────────
pub const SET_VOICE: &str = "Set or clear the voice for THIS session (not saved; reverts on engine \
    restart). Pass voice (an id from list_voices) to set; omit it to clear and revert to the \
    configured voice. Engine is inferred from the id (\"af_sarah\" → Kokoro, \"Samantha\" → System) \
    or pass tts_engine. Scoped to this Claude session.";
pub const SET_VOICE_VOICE: &str =
    "A voice id from list_voices (e.g. \"af_sarah\", \"Samantha\"). Omit to clear the session override.";
pub const SET_VOICE_ENGINE: &str =
    "TTS engine for the voice: \"built_in\" or \"system\"; inferred from the id when omitted.";

// ── listen ───────────────────────────────────────────────────────────────────────────
pub const LISTEN: &str = "Record the mic and return the transcribed text (local Parakeet STT).";
pub const LISTEN_SECONDS: &str = "Seconds to record before transcribing (default 10).";

// ── status ───────────────────────────────────────────────────────────────────────────
pub const STATUS: &str = "Report current state: engine, active voice, default rate, whether speech \
    is playing (tts_active), queue length, paused, and any session_voice override. Pass detail:true \
    to also include per-engine model lifecycle, the running map, dictation state, and stats.";
pub const STATUS_DETAIL: &str =
    "Also include the full engine lifecycle, the running map, dictation state, and stats. Default false.";

// ── diarize ──────────────────────────────────────────────────────────────────────────
pub const DIARIZE: &str = "Record the mic and return speaker diarization (who spoke when): \
    [{speaker,start,end,name?}] with per-speaker spans in seconds; name is set when a cluster matches \
    an enrolled voiceprint (see enroll). Requires diarization on (set_config \
    diarizer_provider=[\"apple_native\"]). macOS-only.";
pub const DIARIZE_SECONDS: &str = "Seconds to record before diarizing (default 10).";

// ── enroll ───────────────────────────────────────────────────────────────────────────
pub const ENROLL: &str = "Record the mic and save a speaker voiceprint under name, so future diarize \
    calls label that person. Re-enrolling the same name replaces it. macOS-only.";
pub const ENROLL_NAME: &str = "The person's name/label for this voiceprint.";
pub const ENROLL_SECONDS: &str = "Seconds to record (default 15; longer/varied = a stronger voiceprint).";

// ── forget_speaker ───────────────────────────────────────────────────────────────────
pub const FORGET_SPEAKER: &str = "Remove an enrolled voiceprint by name (no-op if it isn't enrolled).";
pub const FORGET_SPEAKER_NAME: &str = "The enrolled name to remove.";

// ── list_speakers ────────────────────────────────────────────────────────────────────
pub const LIST_SPEAKERS: &str = "List the names of enrolled speaker voiceprints.";

// ── set_config ───────────────────────────────────────────────────────────────────────
pub const SET_CONFIG: &str = "Atomically update DontSpeak's persistent settings (config.toml). All \
    fields optional; provide at least one. Validated, applied together, then hot-reloaded. For a \
    one-off voice, use set_voice.";
pub const SET_CONFIG_TTS_ENGINE: &str = "Spoken-reply engine PREFERENCE LADDER — an array in \
    descending preference; the first rung usable on this machine wins. Rungs: \"built_in\" (Kokoro) \
    and \"system\" (macOS `say`). Default [\"built_in\", \"system\"] (Kokoro, falling back to the \
    system voice where Kokoro can't run, e.g. an Intel mac). [] (or \"off\") = no spoken replies.";
pub const SET_CONFIG_TTS_VOICES: &str = "Ordered Kokoro voice ids for the BUILT-IN engine (the first is \
    the default, the rest a per-terminal round-robin pool). Built-in only.";
pub const SET_CONFIG_TTS_SYSTEM_VOICE: &str = "Voice name for the SYSTEM engine (e.g. \"Samantha\"); \
    empty = OS default. System engine only.";
pub const SET_CONFIG_TTS_RATE: &str = "Speech rate 0.5–2.0 (1.0 = normal). Applies to both engines.";
pub const SET_CONFIG_NARRATE: &str = "What to narrate aloud — a set of [\"shorts\", \"digests\"] \
    (default both). \"digests\": speak the spoken blockquotes Claude writes and inject the narration \
    spec (gives long replies a spoken digest). \"shorts\": also speak a short, blockquote-free reply on \
    its own. [] = narrate nothing.";
pub const SET_CONFIG_GREET: &str = "Greet each new terminal aloud in its pool voice. Default on.";
pub const SET_CONFIG_DROP_SPEECH: &str = "Drop a window's pending speech on submit — a set of \
    \"voice\" (a dictation submit) and \"keyboard\" (you type and press Enter). Default \
    [\"voice\",\"keyboard\"] (drop on any submit); [] = never. Keyboard needs the UserPromptSubmit hook.";
pub const SET_CONFIG_PAUSE_BG: &str =
    "Pause speech while no terminal is frontmost; resume on focus. Default false.";
pub const SET_CONFIG_EARCON_REPLY: &str = "Reply-done chime (Stop hook): a system-sound name or an \
    absolute path (.aiff/.wav/.oga); empty = off. Defaults to the OS chime.";
pub const SET_CONFIG_EARCON_INPUT: &str = "Needs-input cue (Notification hook): a system-sound name or \
    an absolute path; empty = off (default).";
pub const SET_CONFIG_CAPS: &str = "Enable the Caps Lock handler — push-to-talk dictation plus \
    silence/cancel. Default on. With stt_engine=off, Caps still silences the voice.";
pub const SET_CONFIG_STT_ENGINE: &str = "Dictation engine PREFERENCE LADDER — an array in descending \
    preference; the first rung usable on this machine wins. Rungs: \"built_in\" (Parakeet), \"system\" \
    (macOS 26+ on-device SpeechAnalyzer, en-US), \"claude_code\" (delegate to Claude Code's voice \
    key). Default [\"built_in\", \"system\", \"claude_code\"] — claude_code is always usable and LAST, \
    so dictation still works where the on-device engines can't run (e.g. an Intel mac). [] (or \
    \"off\") = dictation off (Caps still silences the voice).";
pub const SET_CONFIG_CAPTURE_GAIN: &str =
    "Mic gain before STT: \"auto\" (default) or a fixed 0.5–20.0 multiplier.";
pub const SET_CONFIG_AUTO_SUBMIT: &str = "Press Return after pasting dictation into the focused app. \
    Default true; false = insert only, you press Return.";
pub const SET_CONFIG_PROVIDER: &str = "Ordered compute-backend ladder for Kokoro (TTS) and Parakeet \
    (STT) (default [\"ane\",\"ort_cuda\",\"ort_cpu\"]): \"ane\" (Apple Neural Engine), \"ort_cuda\" \
    (NVIDIA GPU, TTS only), \"ort_coreml\" (macOS, TTS only), \"ort_cpu\". First usable rung wins; \
    unusable rungs are skipped.";
pub const SET_CONFIG_DIARIZER: &str = "Diarization runtime — the single \"apple_native\" rung (macOS). \
    Doubles as the switch: [\"apple_native\"] = on, [] = off (default).";
pub const SET_CONFIG_CLUSTERING: &str =
    "Diarization sensitivity 0.5–0.9 (default 0.7); lower splits more speakers apart.";
pub const SET_CONFIG_SPEAKER_THRESH: &str = "Cosine cutoff 0.0–1.0 (default 0.65) for labelling a \
    cluster with an enrolled name; higher = stricter.";
pub const SET_CONFIG_SPEAKER_LOCK: &str = "Transcribe ONLY enrolled speaker(s) — needs diarization on \
    and ≥1 enrolled voice; other voices are dropped. Parakeet only. Default off.";
pub const SET_CONFIG_TRAY: &str = "When the menu-bar pill colors and whether it breathes — a set \
    (default [\"stt\",\"tts_animated\"]): \"stt\"/\"tts\" color it statically, \"stt_animated\"/\
    \"tts_animated\" also pulse. [] = never color.";

// ── wire ─────────────────────────────────────────────────────────────────────────────
pub const WIRE: &str = "Write a config to disk, or register/remove a client integration (the same \
    setup the installer does, anytime). Targets: \"narration_spec\" (write narration-spec.md), \
    \"claude_code\" (voice hooks in settings.json), \"claude_desktop\" (the MCP entry), \"codex\" \
    (narration hooks in config.toml). Additive and backed-up; enabled=false removes only our entry.";
pub const WIRE_TARGET: &str = "What to wire: the narration spec, or a client integration.";
pub const WIRE_ENABLED: &str = "true = register/wire, false = remove.";
