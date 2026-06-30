//! DontSpeak's own speech config (`VoiceConfig`) — the typed struct read from
//! `our config.toml`, its defaults, clamping, load, and the warm-subsystem delta.

use std::io;

use serde::{Deserialize, Serialize};

/// The product's out-of-the-box Kokoro voice — the ONE source of truth for "which
/// voice when none is otherwise chosen". Used for the default `tts_built_in_voices`,
/// the `current_voice()` empty-list fallback, AND the helper's empty-request fallback
/// (`ds_helper`), so a missing/blank voice string can NEVER resolve to a different
/// voice than the configured default. Do NOT re-hardcode this literal elsewhere.
pub const DEFAULT_KOKORO_VOICE: &str = "af_sarah";

use crate::enums::{
    de_diarizer_provider, de_drop_speech_on, de_listen_mode, de_narrate, de_provider,
    de_stt_engine, de_tray_indicator, de_tts_engine, default_diarizer_provider,
    default_drop_speech_on, default_narrate, default_provider, default_stt_engine,
    default_tray_indicator, default_tts_engine,
};
use crate::{
    DiarizerProvider, DropSpeechKind, ListenMode, LogLevel, NarrateKind, Paths, Provider,
    SttEngine, TrayKind, TtsEngine, log,
};

/// Spoken wake phrases for the hands-free (always-listening) mode: the word that opens
/// the dictation pill and starts capturing, the word that submits the captured text
/// (paste + Enter), and the word that discards it. Matched case-insensitively; the START
/// word is FUZZY (the STT mangles it, e.g. "computer" → "computor"/"computa") while submit
/// and cancel are EXACT, so a stray "submit" mid-thought never fires. Serialized as a
/// `[hands_free]` TOML table. This feature is shelved pending better on-device STT — the
/// plumbing works; the wake word is only as reliable as the transcription of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HandsFreePhrases {
    /// Opens the pill and begins capturing (default "computer").
    pub start: String,
    /// Submits the captured text — paste it + Enter (default "submit").
    pub submit: String,
    /// Discards the in-flight capture (default "cancel").
    pub cancel: String,
}

impl Default for HandsFreePhrases {
    fn default() -> Self {
        Self {
            start: "computer".to_string(),
            submit: "submit".to_string(),
            cancel: "cancel".to_string(),
        }
    }
}

/// DontSpeak's speech config, read from `our config.toml` (a neutral home, not tied to any
/// client). Claude Code's own `voice` block stays in its settings.json and is owned entirely
/// by the user — DontSpeak never writes it (the `claude_code` STT engine only READS it). Every
/// field is `#[serde(default)]` so an absent value reproduces today's behavior exactly.
///
/// `Serialize` writes the file directly as TOML (each enum via its `as_str()` token);
/// `Deserialize` reads it back — a typed round-trip, no JSON in between.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Ordered list of Kokoro voice ids (e.g. ["af_sarah", "am_adam"]). The FIRST entry
    /// is the default/current voice (`current_voice()`); the rest form a per-terminal pool:
    /// each distinct Claude session (terminal) claims the next untaken voice, so concurrent
    /// terminals speak with DIFFERENT voices, round-robining (reusing) once exhausted. This
    /// list is the single source of truth for the spoken voice — edited via `set_config`.
    #[serde(default = "default_voices")]
    pub tts_built_in_voices: Vec<String>,
    /// macOS `say` voice for the SYSTEM engine — the full display name incl. any
    /// quality suffix (e.g. "Ava (Premium)"). Empty = the OS default voice. Kept
    /// SEPARATE from `tts_built_in_voices` (Kokoro's) so switching engines restores each
    /// engine's own voice instead of handing one engine the other's incompatible name: the
    /// worker reads `tts_built_in_voices` for Kokoro and `tts_system_voice` for System.
    #[serde(default)]
    pub tts_system_voice: String,
    /// When true, a freshly-opened terminal (new Claude session) is greeted ALOUD — a short,
    /// varied one-liner in that terminal's assigned pool voice (e.g. "Sarah here — I'm with
    /// you today."). On by default. Drives the SessionStart hook → engine greet. (Voice only:
    /// CC 2.1+ drops a SessionStart hook's `systemMessage`, so the old printed banner is gone.)
    #[serde(default = "default_enabled")]
    pub greet_on_open: bool,

    // ── Phase-1.5: narration (§G.5) ─────────────────────────────────────────
    /// What gets narrated — a SET, `narrate = ["shorts", "digests"]` (default: both on).
    /// `digests` speaks each assistant message's top-level blockquotes as they STREAM (prose,
    /// the lines Claude leads each tool step with, and the final reply, all through the one
    /// `MessageDisplay` pipeline) AND injects the narration spec that asks Claude to write those
    /// blockquotes — so a long reply is heard as its spoken digest. `shorts` speaks a SHORT reply
    /// that carries NO blockquote, lightly cleaned and voiced whole, so brief one-liners are heard
    /// even when there's no digest. An EMPTY array narrates nothing (the old "off"). The other mute
    /// is `tts_engine = off`. See [`Self::narrates`].
    #[serde(default = "default_narrate", deserialize_with = "de_narrate")]
    pub narrate: Vec<NarrateKind>,

    // ── Phase-1.5: long-press reset (§F) ────────────────────────────────────
    /// Physical Caps hold ≥ this (ms) → force-reset to idle, LED off.
    #[serde(default = "default_long_press_ms")]
    pub long_press_ms: u64,

    // ── Phase-2: speech-to-text (§A.2 / Config Schema) ──────────────────────
    /// Dictation engine — an ORDERED PREFERENCE LADDER. Walk the list, use the first rung
    /// usable on this build/platform: `built_in` (Parakeet) → `system` (macOS SpeechAnalyzer)
    /// → `claude_code` (delegate to Claude Code), the default. EMPTY = dictation OFF (Caps
    /// still silences). ARRAYS ONLY — a scalar string (or wrong type) degrades to the default
    /// ladder. See [`Self::resolved_stt`].
    #[serde(default = "default_stt_engine", deserialize_with = "de_stt_engine")]
    pub stt_engine: Vec<SttEngine>,

    // ── Speaker diarization + voiceprint enrollment ("who spoke when", by name) ─
    /// Diarization runtime AND on/off in ONE field — an ORDERED PRIORITY ladder. EMPTY (the
    /// default) = diarization OFF (the `diarize`/`enroll` tools + speaker-lock are inert). A
    /// non-empty ladder ENABLES it and sets the runtime priority: walk the list, use the first
    /// rung usable on this platform (`apple_native` = macOS FluidAudio Core ML / ANE, macOS-only).
    /// e.g. `diarizer_provider = ["apple_native"]`. macOS / Core ML only for now. See
    /// [`Self::diarization_on`] / [`Self::resolved_diarizer`].
    #[serde(
        default = "default_diarizer_provider",
        deserialize_with = "de_diarizer_provider"
    )]
    pub diarizer_provider: Vec<DiarizerProvider>,
    /// Clustering threshold (0.5–0.9, lower = MORE speakers split apart). Default 0.7
    /// (FluidAudio's default). Tune down (~0.5) for close/similar voices.
    #[serde(default = "default_clustering_threshold")]
    pub clustering_threshold: f32,
    /// Cosine cutoff (0.0–1.0) for matching a diarized cluster to an ENROLLED voiceprint;
    /// at/above it the cluster is labelled with that person's name. Default 0.65.
    #[serde(default = "default_speaker_threshold")]
    pub speaker_threshold: f32,
    /// "Speaker lock" for dictation: when diarization is ON (`diarizer_provider` non-empty) AND at least one voice is
    /// enrolled, transcribe ONLY the enrolled speaker(s). Each utterance is diarized, the
    /// segments whose cluster matches an enrolled voiceprint are kept, and every other voice
    /// (other people, TV, background) is dropped before transcription. Default off. Applies to
    /// the built-in (Parakeet) STT path only; fails open (normal transcription) if diarization
    /// is unavailable.
    #[serde(default)]
    pub stt_speaker_lock: bool,

    // ── Phase-2: text-to-speech (§A.1 / Config Schema) ──────────────────────
    /// Spoken-reply engine — an ORDERED PREFERENCE LADDER. Walk the list, use the first rung
    /// usable on this build/platform: `built_in` (Kokoro) → `system` (macOS `say` / Windows
    /// SAPI), the default. EMPTY = spoken replies OFF. ARRAYS ONLY — a scalar string (or wrong
    /// type) degrades to the default ladder. See [`Self::resolved_tts`].
    #[serde(default = "default_tts_engine", deserialize_with = "de_tts_engine")]
    pub tts_engine: Vec<TtsEngine>,
    /// 0.5–2.0, step 0.25; 1.0 = normal. Passed to `Tts::speak(rate)`.
    #[serde(default = "default_rate")]
    pub tts_rate: f32,
    /// SHARED on-device compute backend for BOTH Kokoro TTS and Parakeet STT — an ORDERED
    /// PRIORITY ladder, `provider = ["ane", "ort_cuda", "ort_cpu"]` (the default). Each engine
    /// walks the list and picks the FIRST rung usable on this platform (rungs: `ane` = macOS
    /// FluidAudio native ANE, `ort_cuda` = NVIDIA GPU, `ort_coreml` = macOS ort CoreML EP
    /// (TTS-only, explicit), `ort_cpu`). Empty / all-unknown → the default ladder (there is
    /// always a backend). Resolves to a single token for the warm child via `DONTSPEAK_PROVIDER`
    /// (TTS, [`Self::tts_provider_token`]) and `DONTSPEAK_STT_PROVIDER` (STT,
    /// [`Self::resolved_stt_provider`]).
    #[serde(default = "default_provider", deserialize_with = "de_provider")]
    pub provider: Vec<Provider>,

    // ── Engine-owns-everything subsystem toggles (see docs/DAEMON-REFACTOR.md) ─
    // TTS and STT on/off are folded into their engine enums (`tts_engine`/`stt_engine` =
    // `off`) — there is no separate `tts_enabled`/`stt_enabled` flag; the engine choice IS
    // the selection. `caps_enabled` is its OWN axis: it gates the physical Caps key handler,
    // which does BOTH dictation (via `stt_engine`) AND silencing/cancelling the voice — so
    // it is independent of `stt_engine` (with `stt_engine=off`, Caps still silences speech).
    /// Caps-Lock key loop (dictation + voice silence/cancel). Default on.
    #[serde(default = "default_enabled")]
    pub caps_enabled: bool,
    /// Which live states the MENU-BAR icon colors itself for — a SET, `tray_indicator =
    /// ["stt", "tts"]` (default `["stt", "tts"]`). The app reads this; the engine just passes
    /// it through in model_status. `stt` colors the pill while the mic is live, `tts` while
    /// talking. An EMPTY array never colors the icon (the old "none"). Unknown tokens drop;
    /// any non-array value ⇒ the default set.
    #[serde(
        default = "default_tray_indicator",
        deserialize_with = "de_tray_indicator"
    )]
    pub tray_indicator: Vec<TrayKind>,

    // ── Always-listening (hands-free) mode (docs/ALWAYS-LISTENING.md) ─────────
    /// record_submit (default, today's Caps-Lock PTT) | always (hands-free
    /// continuous loop). Unknown ⇒ default. The two modes are exclusive.
    #[serde(default, deserialize_with = "de_listen_mode")]
    pub listen_mode: ListenMode,
    /// Hands-free (`always` mode) wake phrases — the start word that opens the pill,
    /// the submit word, and the cancel word. A stop word fires only as the FINAL token
    /// of an utterance + `submit_confirm_ms` of continued silence. See [`HandsFreePhrases`].
    #[serde(default)]
    pub hands_free: HandsFreePhrases,
    /// Continued silence (ms) required AFTER the stopword before it submits; if
    /// speech resumes inside this window the word was content, not a command.
    /// Default 1000.
    #[serde(default = "default_submit_confirm_ms")]
    pub submit_confirm_ms: u64,
    /// Trailing silence (ms) that closes an utterance in `always` mode. Default
    /// 700 (the 500–700 ms end-of-turn middle ground).
    #[serde(default = "default_endpoint_silence_ms")]
    pub endpoint_silence_ms: u64,

    // ── Full-duplex AEC (docs/AEC.md) ─────────────────────────────────────────────
    /// Keep the mic open WHILE TTS plays, with acoustic echo cancellation, instead
    /// of the half-duplex gate (mic closed during TTS). macOS-only (VoiceProcessing
    /// I/O) and scoped to the Parakeet STT path — the Ctrl+G path is unaffected.
    /// Default off; the engine only engages it when STT is Parakeet.
    #[serde(default)]
    pub full_duplex: bool,
    /// Make-up gain applied to captured mic audio before STT (1.0 = none). With
    /// full-duplex VPIO's AGC off, a quiet/distant mic may need a boost for
    /// Parakeet; also applies to the plain mic. Clamped to avoid clipping. Takes
    /// effect on the next dictation (no restart).
    #[serde(default = "default_capture_gain")]
    pub capture_gain: CaptureGain,

    /// Whether dictation presses Return to SUBMIT after pasting the transcript, in ANY
    /// focused app (terminal, chat box, search field, editor — the paste itself always
    /// lands in the focused field regardless). Default ON. Off ⇒ the text is just
    /// inserted and the user presses Return themselves.
    #[serde(default = "default_enabled")]
    pub auto_submit: bool,

    /// Which submit kinds drop that window's pending speech — a SET of `voice`/`text`
    /// (default `["text"]` = drop on a typed submit; `[]` = never). See [`DropSpeechKind`].
    #[serde(
        default = "default_drop_speech_on",
        deserialize_with = "de_drop_speech_on"
    )]
    pub drop_speech_on: Vec<DropSpeechKind>,

    /// Whether playback PAUSES while no terminal is frontmost — i.e. while DontSpeak's host
    /// terminal is in the BACKGROUND (you've tabbed to a browser/editor). When true, the
    /// worker HOLDS the queue (nothing dropped) until a terminal is focused again, then
    /// resumes. DEFAULT false = keep speaking regardless of focus (the hands-free intent:
    /// you tab away to read while still listening). NOTE: the gate is "is ANY terminal
    /// frontmost", not WHICH one — so which window's speech plays is the active-session
    /// selection, not the one you foreground (see docs/PER-TERMINAL-QUEUES.md).
    #[serde(default)]
    pub pause_in_background: bool,

    // ── Audible earcons: the reply "ding" + a needs-input cue (see `earcon` module) ──
    /// The sound for the reply-done "ding" — played when Claude finishes a reply (the Stop
    /// hook). The sound IS the on/off: a bundled system-sound NAME or an ABSOLUTE PATH
    /// (.aiff/.wav/.oga); EMPTY = off. Defaults to the OS's bundled chime — "ding" on Windows,
    /// "Tink" on macOS, "message" on Linux — so it rings out of the box. A bare name resolves
    /// against the OS's bundled sounds by introspection (the real file in the sounds folder, no
    /// hardcoded path); an unresolvable sound is effectively off. Honors global mute.
    #[serde(default = "default_earcon_reply")]
    pub earcon_reply_sound: String,
    /// The sound for the needs-input cue — played when Claude is waiting on you, a permission
    /// prompt or idle (the Notification hook). Same rule, but EMPTY by default (off, like the
    /// historically-unwired earcon): set a bundled name or an absolute path to enable it.
    #[serde(default)]
    pub earcon_needs_input_sound: String,
}

/// Which warm subsystems a `set_config` delta touches — computed by
/// [`VoiceConfig::changes_since`] so the engine applies ONLY what changed and
/// never does a full reload. Per-call params (voice/rate/region/flags) need no
/// flag here: they're read fresh on the next synth/transcribe call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConfigChange {
    /// `caps_enabled` flipped — start/stop the dictation behavior.
    pub caps_toggled: bool,
    /// `tts_engine` changed (incl. to/from `off`) — load (Kokoro on) or drop (off/System)
    /// the warm Kokoro synth.
    pub tts_toggled: bool,
    /// `stt_engine` changed — swap the STT path (claude_code ↔ local).
    pub stt_changed: bool,
    /// `listen_mode` flipped — start/stop the always-listening loop.
    pub listen_mode_changed: bool,
    /// `provider` (the shared compute ladder) changed — switch the warm child's runtime
    /// (and fetch the GPU runtime first when the resolved rung becomes "ort_cuda").
    pub provider_changed: bool,
}

impl ConfigChange {
    /// Nothing warm needs touching — only per-call params (voice/rate/flags)
    /// changed, so the next call simply reads the new value.
    pub fn is_noop(&self) -> bool {
        *self == ConfigChange::default()
    }
}

fn default_enabled() -> bool {
    true
}
/// The default reply-ding sound: the OS's bundled chime BY NAME (resolved to the real file in
/// the sounds folder by `earcon::resolve_cue`), so the ding rings out of the box per platform.
/// Windows "ding" (C:\Windows\Media\ding.wav), macOS "Tink" (the historical chime), Linux
/// "message" (freedesktop); empty on other platforms (off).
fn default_earcon_reply() -> String {
    if cfg!(target_os = "macos") {
        "Tink".to_string()
    } else if cfg!(target_os = "windows") {
        "ding".to_string()
    } else if cfg!(target_os = "linux") {
        "message".to_string()
    } else {
        String::new()
    }
}
fn default_voices() -> Vec<String> {
    // Default to a single voice ([`DEFAULT_KOKORO_VOICE`]) out of the box. Add more ids
    // via `set_config` to form a per-terminal pool so concurrent Claude sessions speak
    // with different voices; with one entry every terminal uses that voice.
    vec![DEFAULT_KOKORO_VOICE.to_string()]
}
fn default_long_press_ms() -> u64 {
    600
}
fn default_rate() -> f32 {
    1.0
}
fn default_clustering_threshold() -> f32 {
    0.7
}
fn default_speaker_threshold() -> f32 {
    0.65
}
fn default_submit_confirm_ms() -> u64 {
    1000
}
fn default_endpoint_silence_ms() -> u64 {
    700
}
fn default_capture_gain() -> CaptureGain {
    CaptureGain::Auto
}

/// Mic make-up gain before STT. `Auto` (default) normalizes each utterance to a target
/// level — machine- AND mode-independent, so dictation just works after install on any
/// mic without per-machine tuning (it gives the half-duplex path the level-consistency
/// VPIO's AGC provides in full-duplex). `Manual(g)` applies a fixed multiplier instead.
///
/// Serializes as the string `"auto"` or a JSON number, and deserializes from either, so
/// settings.json accepts `"capture_gain": "auto"` or `"capture_gain": 2.5`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum CaptureGain {
    #[default]
    Auto,
    Manual(f32),
}

impl CaptureGain {
    /// The fixed multiplier for `Manual`; `None` for `Auto` (the caller normalizes).
    pub fn manual(self) -> Option<f32> {
        match self {
            CaptureGain::Manual(g) => Some(g),
            CaptureGain::Auto => None,
        }
    }
}

impl serde::Serialize for CaptureGain {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            CaptureGain::Auto => s.serialize_str("auto"),
            CaptureGain::Manual(g) => s.serialize_f32(*g),
        }
    }
}

impl<'de> serde::Deserialize<'de> for CaptureGain {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match serde_json::Value::deserialize(d)? {
            serde_json::Value::String(s) if s.eq_ignore_ascii_case("auto") => Ok(CaptureGain::Auto),
            serde_json::Value::Number(n) => {
                let g = n
                    .as_f64()
                    .ok_or_else(|| Error::custom("capture_gain: invalid number"))?
                    as f32;
                Ok(CaptureGain::Manual(g.clamp(0.5, 20.0)))
            }
            _ => Err(Error::custom(
                r#"capture_gain must be "auto" or a number 0.5–20.0"#,
            )),
        }
    }
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            tts_built_in_voices: default_voices(),
            tts_system_voice: String::new(),
            greet_on_open: true,
            narrate: default_narrate(),
            long_press_ms: default_long_press_ms(),
            stt_engine: default_stt_engine(),
            diarizer_provider: default_diarizer_provider(),
            clustering_threshold: default_clustering_threshold(),
            speaker_threshold: default_speaker_threshold(),
            stt_speaker_lock: false,
            tts_engine: default_tts_engine(),
            tts_rate: default_rate(),
            provider: default_provider(),
            caps_enabled: default_enabled(),
            tray_indicator: default_tray_indicator(),
            listen_mode: ListenMode::default(),
            hands_free: HandsFreePhrases::default(),
            submit_confirm_ms: default_submit_confirm_ms(),
            endpoint_silence_ms: default_endpoint_silence_ms(),
            full_duplex: false,
            capture_gain: default_capture_gain(),
            auto_submit: default_enabled(),
            drop_speech_on: default_drop_speech_on(),
            pause_in_background: false,
            earcon_reply_sound: default_earcon_reply(),
            earcon_needs_input_sound: String::new(),
        }
    }
}

impl VoiceConfig {
    /// Whether spoken replies are on — the `tts_engine` ladder has a rung usable on this
    /// platform. Folds in the old `tts_enabled` flag (the engine choice IS the on/off): an
    /// empty ladder, or one whose only rungs can't run here, means no spoken replies.
    pub fn tts_on(&self) -> bool {
        self.resolved_tts().is_some()
    }

    /// The TTS engine that actually runs on THIS build/platform: walk the `tts_engine`
    /// preference ladder and take the first rung usable here (see [`TtsEngine::tts_usable`]).
    /// `None` = spoken replies off (empty ladder, or no usable rung — e.g. `["built_in"]` on
    /// an x86_64 mac). A STATIC preference: runtime model gating still applies downstream.
    pub fn resolved_tts(&self) -> Option<TtsEngine> {
        self.tts_engine.iter().copied().find(|e| e.tts_usable())
    }

    /// The STT engine that actually runs on THIS build/platform: the first usable rung of the
    /// `stt_engine` preference ladder (see [`SttEngine::stt_usable`]). `None` = dictation off.
    /// With the default ladder this resolves to `claude_code` wherever the on-device engines
    /// can't run (it's always usable and LAST), so dictation degrades rather than dying.
    pub fn resolved_stt(&self) -> Option<SttEngine> {
        self.stt_engine.iter().copied().find(|e| e.stt_usable())
    }

    /// Compute the warm-subsystem changes from `prev` to `self`, so the engine
    /// applies a `set_config` delta surgically (see [`ConfigChange`]). Per-call
    /// params (voice/rate/region/narrate/…) are intentionally NOT diffed here.
    pub fn changes_since(&self, prev: &VoiceConfig) -> ConfigChange {
        ConfigChange {
            caps_toggled: self.caps_enabled != prev.caps_enabled,
            // Re-evaluate the warm-TTS lifecycle when the RESOLVED TTS engine changes (off ⇄
            // Kokoro warms/frees a child; System uses `say` — no warm) — the engine choice now
            // carries the on/off, so an engine change covers the old tts_enabled toggle. Diff
            // the resolved engine (not the raw ladder) so a reorder that doesn't change what
            // runs is a no-op.
            tts_toggled: self.resolved_tts() != prev.resolved_tts(),
            // The shared `provider` drives the STT runtime too, so a provider change
            // rebuilds STT (as well as restarting the TTS child via `provider_changed`).
            stt_changed: self.resolved_stt() != prev.resolved_stt()
                || self.provider != prev.provider,
            listen_mode_changed: self.listen_mode != prev.listen_mode,
            provider_changed: self.provider != prev.provider,
        }
    }
}

/// Read `our config.toml` as a native TOML table. Fail-open: a missing file,
/// bad TOML, or a non-table top level yields an empty table so callers degrade to
/// their own defaults. The file is a flat table (our config has no nesting), so its
/// keys are exactly `VoiceConfig`'s fields plus the MCP-HTTP settings — read/merged
/// at the `toml` layer (no JSON in between).
pub(crate) fn read_config_table(paths: &Paths) -> toml::Table {
    std::fs::read_to_string(&paths.config_toml)
        .ok()
        .and_then(|s| toml::from_str::<toml::Table>(&s).ok())
        .unwrap_or_default()
}

/// Atomically write a TOML table to `our config.toml`.
pub(crate) fn write_config_table(paths: &Paths, table: &toml::Table) -> io::Result<()> {
    let text = toml::to_string_pretty(table).map_err(io::Error::other)?;
    crate::atomic_write_str(&paths.config_toml, &text)
}

impl VoiceConfig {
    /// Read `our config.toml` straight into the typed struct; fall back to
    /// defaults on any error (missing file, bad TOML). Unknown keys (typos, or keys
    /// owned by another reader like the MCP-HTTP settings) are tolerated, but a typo
    /// among OUR keys is logged so a silently-ignored hand-edit is discoverable.
    /// Out-of-range numbers are clamped, so a hand-edit can't feed the engine a bad value.
    pub fn load(paths: &Paths) -> Self {
        let Ok(text) = std::fs::read_to_string(&paths.config_toml) else {
            return Self::default();
        };
        let Ok(table) = toml::from_str::<toml::Table>(&text) else {
            log(
                paths,
                LogLevel::Warn,
                "config",
                "config.toml is not valid TOML; using defaults",
            );
            return Self::default();
        };
        // Warn (don't fail) on keys that are neither ours nor a known sibling reader's —
        // serde would otherwise drop them silently, hiding a typo like `narate`.
        let known = Self::known_keys();
        for k in table.keys() {
            if !known.contains(k.as_str()) {
                log(
                    paths,
                    LogLevel::Warn,
                    "config",
                    &format!("unknown key in config.toml: {k:?} (ignored)"),
                );
            }
        }
        let mut cfg: VoiceConfig = toml::Value::Table(table).try_into().unwrap_or_default();
        cfg.clamp();
        cfg
    }

    /// Every key OUR config legitimately contains: the serialized `VoiceConfig` fields.
    /// Used to flag typos on load.
    fn known_keys() -> std::collections::HashSet<String> {
        let mut keys = std::collections::HashSet::new();
        if let Ok(toml::Value::Table(t)) = toml::Value::try_from(VoiceConfig::default()) {
            keys.extend(t.keys().cloned());
        }
        keys
    }

    /// Clamp numeric fields to their valid ranges so a hand-edited config can't push an
    /// out-of-range value past the `set_config` API (which validates) into the engine.
    /// `capture_gain` self-clamps in its own deserializer.
    fn clamp(&mut self) {
        self.tts_rate = self.tts_rate.clamp(0.5, 2.0);
        self.clustering_threshold = self.clustering_threshold.clamp(0.5, 0.9);
        self.speaker_threshold = self.speaker_threshold.clamp(0.0, 1.0);
    }

    /// True when the active TTS model is the apple-native (FluidAudio Core ML / ANE) Kokoro
    /// — the Kokoro engine running on the apple-native provider. It runs on the Neural
    /// Engine and self-manages its model cache (materializing voices on demand from the
    /// shared `voices-v1.0.bin`), so the status code gates its Kokoro row on shim capability
    /// rather than the ONNX files. Voices are SHARED — there is no separate voice set.
    pub fn uses_apple_native_model(&self) -> bool {
        self.resolved_tts() == Some(TtsEngine::Kokoro)
            && cfg!(target_os = "macos")
            && self.resolved_tts_provider() == Provider::Ane
    }

    /// The concrete STT runtime the `provider` ladder resolves to on THIS platform: walk the
    /// priority list, take the first rung usable for STT, else CPU. Only ever returns
    /// `OrtCpu`, `OrtCuda`, or `Ane`. A static PREFERENCE — the loader still falls back to CPU
    /// at runtime if a GPU runtime/driver is absent, so a CUDA result never breaks dictation.
    pub fn resolved_stt_provider(&self) -> Provider {
        self.provider
            .iter()
            .copied()
            .find(|p| p.stt_usable())
            .unwrap_or(Provider::OrtCpu)
    }

    /// The concrete TTS (Kokoro) runtime the `provider` ladder resolves to on THIS platform:
    /// the first rung usable for TTS, else CPU. macOS may resolve to `Ane` (FluidAudio native)
    /// or `OrtCoreMl`; Windows to `OrtCuda`; CPU otherwise.
    pub fn resolved_tts_provider(&self) -> Provider {
        self.provider
            .iter()
            .copied()
            .find(|p| p.tts_usable())
            .unwrap_or(Provider::OrtCpu)
    }

    /// The single `DONTSPEAK_PROVIDER` token the warm child should run TTS on — the resolved
    /// TTS rung as its canonical string (e.g. "ane" / "ort_cuda" / "ort_cpu").
    pub fn tts_provider_token(&self) -> &'static str {
        self.resolved_tts_provider().as_str()
    }

    /// Whether diarization is ON — i.e. the `diarizer_provider` ladder is non-empty. The
    /// single gate for the `diarize`/`enroll` tools and speaker-lock (folds in the old
    /// `diarization_enabled` flag).
    pub fn diarization_on(&self) -> bool {
        !self.diarizer_provider.is_empty()
    }

    /// The concrete diarizer runtime the `diarizer_provider` ladder resolves to on THIS
    /// platform: the first rung usable here, else `apple_native` (the only rung). What the
    /// `diarize`/`enroll` tools actually load (only meaningful when [`Self::diarization_on`]).
    pub fn resolved_diarizer(&self) -> DiarizerProvider {
        self.diarizer_provider
            .iter()
            .copied()
            .find(|p| p.diar_usable())
            .unwrap_or(DiarizerProvider::AppleNative)
    }

    /// The Kokoro voice pool — `voices`, shared by every TTS provider. Both backends draw
    /// from one list: the apple-native FluidAudio (Core ML / ANE) backend materializes any
    /// requested voice on demand from the same `voices-v1.0.bin` the ONNX path uses (see
    /// `ds_tts::ane_voices`), so there is no separate apple-native voice set. First = default,
    /// rest = the per-terminal pool.
    pub fn active_voices(&self) -> &[String] {
        &self.tts_built_in_voices
    }

    /// The default/current voice — `active_voices()[0]`, falling back to
    /// [`DEFAULT_KOKORO_VOICE`] if the list is somehow empty. The per-session pool
    /// assignment (in the engine) hands later terminals the subsequent entries.
    pub fn current_voice(&self) -> String {
        self.active_voices()
            .first()
            .cloned()
            .unwrap_or_else(|| DEFAULT_KOKORO_VOICE.into())
    }

    /// Whether `kind` is in the narration set. `narrates(Digests)` gates both message-blockquote
    /// narration AND the injected narration spec; `narrates(Shorts)` gates voicing a short,
    /// blockquote-less reply whole.
    pub fn narrates(&self, kind: NarrateKind) -> bool {
        self.narrate.contains(&kind)
    }

    /// The narration set as a compact `[messages,short]`-style token list, for log lines.
    pub fn narrate_summary(&self) -> String {
        let toks: Vec<&str> = self.narrate.iter().map(|k| k.as_str()).collect();
        format!("[{}]", toks.join(","))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{voice_from_value, voice_to_value, write_settings};

    #[test]
    fn capture_gain_accepts_auto_or_number() {
        // "auto" (any case) → Auto; a number → Manual, clamped to 0.5–20.
        assert_eq!(
            serde_json::from_str::<CaptureGain>("\"auto\"").unwrap(),
            CaptureGain::Auto
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("\"AUTO\"").unwrap(),
            CaptureGain::Auto
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("2.5").unwrap(),
            CaptureGain::Manual(2.5)
        );
        assert_eq!(
            serde_json::from_str::<CaptureGain>("99").unwrap(),
            CaptureGain::Manual(20.0) // clamped
        );
        assert!(serde_json::from_str::<CaptureGain>("\"loud\"").is_err());
        // Round-trips: Auto → "auto", Manual → number.
        assert_eq!(
            serde_json::to_string(&CaptureGain::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&CaptureGain::Manual(3.0)).unwrap(),
            "3.0"
        );
    }

    #[test]
    fn voice_defaults_when_absent() {
        let v: VoiceConfig = serde_json::from_str("{}").unwrap();
        // Both the default pool and the empty-list fallback resolve to the ONE shared
        // default constant — no independently-hardcoded literal can drift from it.
        assert_eq!(v.tts_built_in_voices, vec![DEFAULT_KOKORO_VOICE]);
        assert_eq!(v.current_voice(), DEFAULT_KOKORO_VOICE);
        let empty = VoiceConfig {
            tts_built_in_voices: vec![],
            ..v.clone()
        };
        assert_eq!(empty.current_voice(), DEFAULT_KOKORO_VOICE);
        // Default narration: shorts first, then digests — both on out of the box.
        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
        assert!(v.narrates(NarrateKind::Digests) && v.narrates(NarrateKind::Shorts));
        assert_eq!(v.long_press_ms, 600);
        // Default ladders: TTS prefers Kokoro then the system synth; STT prefers Parakeet,
        // then SpeechAnalyzer, then claude_code (always-usable, LAST).
        assert_eq!(v.tts_engine, vec![TtsEngine::Kokoro, TtsEngine::System]);
        assert_eq!(
            v.stt_engine,
            vec![SttEngine::BuiltIn, SttEngine::System, SttEngine::ClaudeCode]
        );
        assert_eq!(v.tts_rate, 1.0);
        // Always-listening defaults: unset == today (record-and-submit PTT).
        assert_eq!(v.listen_mode, ListenMode::RecordSubmit);
        assert_eq!(v.hands_free.submit, "submit");
        assert_eq!(v.submit_confirm_ms, 1000);
        assert_eq!(v.endpoint_silence_ms, 700);
        assert_eq!(
            v.provider,
            vec![Provider::Ane, Provider::OrtCuda, Provider::OrtCpu]
        );
        assert_eq!(v.tray_indicator, vec![TrayKind::Stt, TrayKind::TtsAnimated]);
    }

    #[test]
    fn provider_is_an_ordered_ladder_failing_open_to_default() {
        let prov = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().provider;
        let default = vec![Provider::Ane, Provider::OrtCuda, Provider::OrtCpu];
        // An explicit ordered ladder keeps its order (deduped); known tokens only.
        assert_eq!(
            prov(r#"{"provider":["ort_cuda","ane","ort_cpu"]}"#),
            vec![Provider::OrtCuda, Provider::Ane, Provider::OrtCpu]
        );
        assert_eq!(prov(r#"{"provider":["ort_cpu"]}"#), vec![Provider::OrtCpu]);
        // Unknown tokens dropped; the old bare `coreml`/`cuda`/`auto` tokens are now unknown
        // (renamed, no back-compat) — left with nothing → falls back to the default ladder.
        assert_eq!(prov(r#"{"provider":["coreml","auto"]}"#), default);
        // Empty array, all-unknown, and any non-array all fall open to the default ladder
        // (compute is never "off").
        assert_eq!(prov(r#"{"provider":[]}"#), default);
        assert_eq!(prov(r#"{"provider":"ane"}"#), default);
        assert_eq!(prov(r#"{"provider":42}"#), default);

        // Canonical tokens round-trip through as_str().
        for p in [
            Provider::OrtCpu,
            Provider::OrtCuda,
            Provider::OrtCoreMl,
            Provider::Ane,
        ] {
            assert_eq!(Provider::parse(p.as_str()), Some(p));
        }
        for t in [TrayKind::Stt, TrayKind::Tts] {
            assert_eq!(TrayKind::parse(t.as_str()), Some(t));
        }
    }

    #[test]
    fn provider_resolution_walks_the_ladder_per_platform() {
        // Only the macOS arm below exercises the resolver (Core ML / ANE rungs); other
        // platforms have nothing platform-specific to assert here, so the helper is gated
        // with them to avoid an unused-closure warning off macOS.
        #[cfg(target_os = "macos")]
        let cfg = |rungs: Vec<Provider>| VoiceConfig {
            provider: rungs,
            ..VoiceConfig::default()
        };
        // STT resolution: first STT-usable rung, else CPU. CoreML is never STT-usable.
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                cfg(vec![Provider::Ane, Provider::OrtCpu]).resolved_stt_provider(),
                Provider::Ane
            );
            assert_eq!(
                cfg(vec![Provider::OrtCoreMl, Provider::OrtCpu]).resolved_stt_provider(),
                Provider::OrtCpu
            );
            // TTS may resolve to the CoreML EP when it's the first TTS-usable rung.
            assert_eq!(
                cfg(vec![Provider::OrtCoreMl, Provider::OrtCpu]).resolved_tts_provider(),
                Provider::OrtCoreMl
            );
            // `uses_apple_native_model` needs the Kokoro engine to actually RESOLVE, which only
            // happens where the built-in stack is usable (arm64 macOS) — on x86_64 macOS the
            // default TTS ladder falls through to `system`, so Kokoro never runs.
            #[cfg(target_arch = "aarch64")]
            {
                assert!(cfg(vec![Provider::Ane]).uses_apple_native_model());
                assert!(!cfg(vec![Provider::OrtCpu]).uses_apple_native_model());
            }
        }
        // Default ladder always resolves to a concrete usable rung (never panics).
        let _ = VoiceConfig::default().resolved_stt_provider();
        let _ = VoiceConfig::default().tts_provider_token();
    }

    #[test]
    fn diarizer_provider_is_the_on_off_ladder() {
        let diar = |j: &str| {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .diarizer_provider
        };
        // Default is EMPTY = diarization OFF (opt-in); the on/off flag is folded in.
        assert!(VoiceConfig::default().diarizer_provider.is_empty());
        assert!(!VoiceConfig::default().diarization_on());
        // A non-empty ladder keeps its order (deduped) and turns diarization ON.
        let on = diar(r#"{"diarizer_provider":["apple_native"]}"#);
        assert_eq!(on, vec![DiarizerProvider::AppleNative]);
        // Empty, all-unknown (old `auto`/`onnx`), and non-array all read as OFF (empty).
        assert!(diar(r#"{"diarizer_provider":["auto"]}"#).is_empty());
        assert!(diar(r#"{"diarizer_provider":["onnx"]}"#).is_empty());
        assert!(diar(r#"{"diarizer_provider":[]}"#).is_empty());
        assert!(diar(r#"{"diarizer_provider":"apple_native"}"#).is_empty());

        // diarization_on() = non-empty; resolution walks to the first platform-usable rung.
        let cfg = |r: Vec<DiarizerProvider>| VoiceConfig {
            diarizer_provider: r,
            ..VoiceConfig::default()
        };
        assert!(cfg(vec![DiarizerProvider::AppleNative]).diarization_on());
        let ladder = vec![DiarizerProvider::AppleNative];
        assert_eq!(
            cfg(ladder).resolved_diarizer(),
            DiarizerProvider::AppleNative
        );
    }

    #[test]
    fn tray_indicator_is_a_set_of_tokens() {
        let tray = |j: &str| {
            serde_json::from_str::<VoiceConfig>(j)
                .unwrap()
                .tray_indicator
        };
        // The array form normalizes to one token per state, canonical order (stt, then tts);
        // an empty array = never color.
        assert_eq!(
            tray(r#"{"tray_indicator":["stt","tts"]}"#),
            vec![TrayKind::Stt, TrayKind::Tts]
        );
        assert_eq!(tray(r#"{"tray_indicator":["tts"]}"#), vec![TrayKind::Tts]);
        assert!(
            tray(r#"{"tray_indicator":[]}"#).is_empty(),
            "empty array = none"
        );
        // The `_animated` form colors AND breathes, and wins if both forms of a state appear.
        assert_eq!(
            tray(r#"{"tray_indicator":["stt_animated","tts"]}"#),
            vec![TrayKind::SttAnimated, TrayKind::Tts]
        );
        assert_eq!(
            tray(r#"{"tray_indicator":["tts","tts_animated"]}"#),
            vec![TrayKind::TtsAnimated]
        );
        // Unknown tokens drop, duplicates collapse, order canonicalizes.
        assert_eq!(
            tray(r#"{"tray_indicator":["tts","both","tts","stt"]}"#),
            vec![TrayKind::Stt, TrayKind::Tts]
        );
        // A legacy string / wrong-typed value degrades to the default set (NO migration of the
        // old none/both tokens — clean rename, no compat shim).
        for raw in [
            r#"{"tray_indicator":"both"}"#,
            r#"{"tray_indicator":"none"}"#,
            r#"{"tray_indicator":3}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<VoiceConfig>(raw)
                    .unwrap()
                    .tray_indicator,
                vec![TrayKind::Stt, TrayKind::TtsAnimated],
                "{raw} → default set"
            );
        }
    }

    #[test]
    fn listen_mode_parses_and_falls_back() {
        let p = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().listen_mode;
        assert_eq!(p(r#"{"listen_mode":"always"}"#), ListenMode::Always);
        assert_eq!(
            p(r#"{"listen_mode":"record_submit"}"#),
            ListenMode::RecordSubmit
        );
        // Each mode has ONE canonical token (no aliases): the old `always-listening` spelling
        // is now an unknown token and degrades to the default.
        assert_eq!(
            p(r#"{"listen_mode":"always-listening"}"#),
            ListenMode::RecordSubmit
        );
        // Unknown / wrong-typed degrade to the default, never error the block.
        assert_eq!(
            p(r#"{"listen_mode":"telepathy"}"#),
            ListenMode::RecordSubmit
        );
        assert_eq!(p(r#"{"listen_mode":9}"#), ListenMode::RecordSubmit);
    }

    #[test]
    fn always_listening_fields_parse() {
        let v: VoiceConfig = serde_json::from_str(
            r#"{"listen_mode":"always","hands_free":{"start":"hey","submit":"send it","cancel":"scrap"},"submit_confirm_ms":800,"endpoint_silence_ms":600}"#,
        )
        .unwrap();
        assert_eq!(v.listen_mode, ListenMode::Always);
        assert_eq!(v.hands_free.start, "hey");
        assert_eq!(v.hands_free.submit, "send it");
        assert_eq!(v.hands_free.cancel, "scrap");
        assert_eq!(v.submit_confirm_ms, 800);
        assert_eq!(v.endpoint_silence_ms, 600);
    }

    #[test]
    fn listen_mode_change_flagged() {
        let base = VoiceConfig::default();
        let m = VoiceConfig {
            listen_mode: ListenMode::Always,
            ..base.clone()
        };
        assert!(m.changes_since(&base).listen_mode_changed);
        // A wake-phrase change alone touches no warm subsystem (read fresh per turn).
        let w = VoiceConfig {
            hands_free: HandsFreePhrases {
                submit: "okay".into(),
                ..Default::default()
            },
            ..base.clone()
        };
        assert!(w.changes_since(&base).is_noop());
    }

    #[test]
    fn always_listening_value_roundtrips() {
        let v = sample_voice();
        let back = voice_from_value(voice_to_value(&v));
        assert_eq!(back.listen_mode, v.listen_mode);
        assert_eq!(back.hands_free, v.hands_free);
        assert_eq!(back.submit_confirm_ms, v.submit_confirm_ms);
        assert_eq!(back.endpoint_silence_ms, v.endpoint_silence_ms);
    }

    // ── Phase-2 engine enum parsing ─────────────────────────────────────────

    #[test]
    fn stt_engine_is_an_ordered_ladder() {
        let p = |j: &str| -> Vec<SttEngine> {
            serde_json::from_str::<VoiceConfig>(j).unwrap().stt_engine
        };
        let default = vec![SttEngine::BuiltIn, SttEngine::System, SttEngine::ClaudeCode];
        // An explicit array keeps its order (deduped), known non-`off` tokens only.
        assert_eq!(
            p(r#"{"stt_engine":["claude_code","built_in"]}"#),
            vec![SttEngine::ClaudeCode, SttEngine::BuiltIn]
        );
        // ARRAYS ONLY: a bare scalar string is NO LONGER a one-rung shorthand — it (known token
        // or not) degrades to the default ladder. `["off"]`/`[]` are the only way to disable.
        assert_eq!(p(r#"{"stt_engine":"system"}"#), default);
        // `["off"]` / empty array ⇒ EMPTY ladder = dictation off.
        assert!(p(r#"{"stt_engine":["off"]}"#).is_empty());
        assert!(p(r#"{"stt_engine":[]}"#).is_empty(), "empty array = off");
        // Unknown tokens drop from an array; an all-unknown / wrong-typed value (incl. a bare
        // scalar) falls open to the default ladder (never errors the block).
        assert_eq!(
            p(r#"{"stt_engine":["deepgram","built_in"]}"#),
            vec![SttEngine::BuiltIn]
        );
        assert_eq!(p(r#"{"stt_engine":"deepgram"}"#), default);
        assert_eq!(p(r#"{"stt_engine":"off"}"#), default);
        assert_eq!(p(r#"{"stt_engine":3}"#), default);
    }

    #[test]
    fn tts_engine_is_an_ordered_ladder() {
        let p = |j: &str| -> Vec<TtsEngine> {
            serde_json::from_str::<VoiceConfig>(j).unwrap().tts_engine
        };
        let default = vec![TtsEngine::Kokoro, TtsEngine::System];
        assert_eq!(
            p(r#"{"tts_engine":["system","built_in"]}"#),
            vec![TtsEngine::System, TtsEngine::Kokoro]
        );
        // ARRAYS ONLY: a bare scalar string degrades to the default ladder (no one-rung
        // shorthand); `["off"]`/`[]` are the only disable.
        assert_eq!(p(r#"{"tts_engine":"system"}"#), default);
        assert!(p(r#"{"tts_engine":["off"]}"#).is_empty());
        assert!(p(r#"{"tts_engine":[]}"#).is_empty(), "empty array = off");
        assert_eq!(p(r#"{"tts_engine":"off"}"#), default);
        assert_eq!(p(r#"{"tts_engine":"festival"}"#), default);
        assert_eq!(p(r#"{"tts_engine":9}"#), default);
    }

    #[test]
    fn resolved_engines_walk_the_ladder_first_usable() {
        // claude_code is always usable, so a default STT ladder always resolves to SOMETHING.
        assert!(VoiceConfig::default().resolved_stt().is_some());
        // An empty ladder = off (resolves to None) for both roles.
        let off = VoiceConfig {
            tts_engine: Vec::new(),
            stt_engine: Vec::new(),
            ..VoiceConfig::default()
        };
        assert!(off.resolved_tts().is_none() && !off.tts_on());
        assert!(off.resolved_stt().is_none());
        // On the x86_64-macOS build the on-device rungs are unusable, so the default ladders
        // fall through: TTS → system (`say`), STT → claude_code (LAST, always usable).
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            assert_eq!(
                VoiceConfig::default().resolved_tts(),
                Some(TtsEngine::System)
            );
            assert_eq!(
                VoiceConfig::default().resolved_stt(),
                Some(SttEngine::ClaudeCode)
            );
            // A ladder with no usable rung resolves to None (= off), not a forced fallback.
            let only_builtin = VoiceConfig {
                tts_engine: vec![TtsEngine::Kokoro],
                ..VoiceConfig::default()
            };
            assert!(only_builtin.resolved_tts().is_none());
        }
    }

    #[test]
    fn resolved_tts_honors_ladder_order_when_multiple_usable() {
        // On a build where BOTH built_in (Kokoro) and system (`say`) can run, the FIRST listed
        // rung wins — proving resolution is preference-ORDERED, not a fixed priority.
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            target_os = "windows"
        ))]
        {
            let c = |rungs: Vec<TtsEngine>| VoiceConfig {
                tts_engine: rungs,
                ..VoiceConfig::default()
            };
            assert_eq!(
                c(vec![TtsEngine::System, TtsEngine::Kokoro]).resolved_tts(),
                Some(TtsEngine::System)
            );
            assert_eq!(
                c(vec![TtsEngine::Kokoro, TtsEngine::System]).resolved_tts(),
                Some(TtsEngine::Kokoro)
            );
        }
    }

    // ── Phase-1.5 config parsing ────────────────────────────────────────────

    #[test]
    fn auto_submit_is_a_plain_bool() {
        let sub = |j: &str| serde_json::from_str::<VoiceConfig>(j).unwrap().auto_submit;
        // Absent ⇒ default ON; explicit booleans pass through.
        assert!(sub("{}"));
        assert!(sub(r#"{"auto_submit":true}"#));
        assert!(!sub(r#"{"auto_submit":false}"#));
    }

    #[test]
    fn narrate_is_a_set_of_tokens() {
        // The array form: known tokens kept in order, an empty array = narrate nothing.
        let both: VoiceConfig =
            serde_json::from_str(r#"{"narrate":["digests","shorts"]}"#).unwrap();
        assert_eq!(
            both.narrate,
            vec![NarrateKind::Digests, NarrateKind::Shorts]
        );
        assert!(both.narrates(NarrateKind::Digests) && both.narrates(NarrateKind::Shorts));

        let msgs: VoiceConfig = serde_json::from_str(r#"{"narrate":["digests"]}"#).unwrap();
        assert_eq!(msgs.narrate, vec![NarrateKind::Digests]);
        assert!(!msgs.narrates(NarrateKind::Shorts));

        let none: VoiceConfig = serde_json::from_str(r#"{"narrate":[]}"#).unwrap();
        assert!(none.narrate.is_empty(), "empty array narrates nothing");
    }

    #[test]
    fn narrate_drops_unknown_tokens_and_dedups() {
        // Unknown tokens in the array are dropped (fail-open), duplicates collapsed. The
        // pre-rename `short`/`messages` aliases are now unknown (no compat shim).
        let v: VoiceConfig =
            serde_json::from_str(r#"{"narrate":["shorts","loud","shorts","digests"]}"#).unwrap();
        assert_eq!(v.narrate, vec![NarrateKind::Shorts, NarrateKind::Digests]);
    }

    #[test]
    fn narrate_non_array_falls_back_to_default() {
        // A legacy bool/string or wrong-typed value degrades to the default set (NO migration
        // of the old off/final/all tokens — clean rename, no compat shim).
        for raw in [
            r#"{"narrate":true}"#,
            r#"{"narrate":"all"}"#,
            r#"{"narrate":7}"#,
        ] {
            let v: VoiceConfig = serde_json::from_str(raw).unwrap();
            assert_eq!(
                v.narrate,
                vec![NarrateKind::Shorts, NarrateKind::Digests],
                "{raw} → default set"
            );
        }
    }

    #[test]
    fn narrate_extra_fields_parse() {
        let v: VoiceConfig = serde_json::from_str(
            r#"{"narrate":["digests"],"skip_ahead_secs":8,"long_press_ms":750}"#,
        )
        .unwrap();
        assert_eq!(v.narrate, vec![NarrateKind::Digests]);
        assert_eq!(v.long_press_ms, 750);
    }

    /// A non-default config so the merge is observably distinct from defaults.
    pub(crate) fn sample_voice() -> VoiceConfig {
        VoiceConfig {
            tts_built_in_voices: vec!["am_michael".into(), "am_adam".into()],
            tts_system_voice: "Samantha (Enhanced)".into(),
            greet_on_open: true,
            stt_engine: vec![SttEngine::BuiltIn],
            diarizer_provider: vec![DiarizerProvider::AppleNative],
            clustering_threshold: 0.55,
            speaker_threshold: 0.7,
            stt_speaker_lock: false,
            tts_engine: vec![TtsEngine::System],
            provider: vec![Provider::OrtCoreMl],
            tts_rate: 1.25,
            narrate: vec![NarrateKind::Digests],
            long_press_ms: 750,
            caps_enabled: false,
            tray_indicator: vec![TrayKind::Stt],
            listen_mode: ListenMode::Always,
            hands_free: HandsFreePhrases {
                submit: "go ahead".into(),
                ..Default::default()
            },
            submit_confirm_ms: 1200,
            endpoint_silence_ms: 650,
            full_duplex: true,
            capture_gain: CaptureGain::Manual(2.5),
            auto_submit: false, // non-default (default is on)
            drop_speech_on: vec![DropSpeechKind::Voice], // non-default (default is [text])
            pause_in_background: true, // non-default (default is false)
            earcon_reply_sound: "Glass".into(), // non-default (default is empty/off)
            earcon_needs_input_sound: "Funk".into(),
        }
    }

    #[test]
    fn changes_since_flags_only_what_changed() {
        let base = VoiceConfig::default();

        // A per-call-only change (voice/rate) flags nothing warm.
        let only_voice = VoiceConfig {
            tts_built_in_voices: vec!["am_michael".into()],
            tts_rate: 1.5,
            ..base.clone()
        };
        assert!(only_voice.changes_since(&base).is_noop());

        // Each toggle/engine field flags exactly its subsystem.
        let caps = VoiceConfig {
            caps_enabled: !base.caps_enabled,
            ..base.clone()
        };
        assert!(caps.changes_since(&base).caps_toggled);

        // Disabling TTS (empty ladder) changes the resolved engine on every platform.
        let tts = VoiceConfig {
            tts_engine: Vec::new(), // off; base default has a usable rung
            ..base.clone()
        };
        assert!(tts.changes_since(&base).tts_toggled);

        // changes_since diffs the RESOLVED engine: disabling dictation (empty ladder) flips
        // stt_changed regardless of which on-device rungs are usable on this build.
        let eng = VoiceConfig {
            stt_engine: Vec::new(), // off; base default resolves to a usable engine
            ..base.clone()
        };
        assert!(eng.changes_since(&base).stt_changed);
    }

    #[test]
    fn write_settings_atomic_roundtrip_on_disk() {
        // The disk wrapper: write our config into a temp our config.toml, then
        // load() it back. Uses a tempdir so it never touches the live config.
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        // Seed an existing config with a hand-added key to prove a voice write preserves
        // the file's other keys.
        std::fs::write(&cfg, "custom_key = \"keep\"\n").unwrap();

        let mut paths = Paths::resolve().expect("resolve");
        paths.config_toml = cfg.clone();

        let v = sample_voice();
        write_settings(&paths, &v).unwrap();

        // Re-read raw TOML to confirm the unrelated key survived the voice write.
        let raw: serde_json::Value =
            toml::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert_eq!(
            raw["custom_key"],
            serde_json::json!("keep"),
            "hand-added key preserved"
        );

        // And load() reconstructs the written config.
        let lv = VoiceConfig::load(&paths);
        assert_eq!(lv.current_voice(), "am_michael");
    }

    #[test]
    fn write_settings_tolerates_missing_file() {
        // No existing config.toml at all → write creates it, load reads it back.
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::resolve().expect("resolve");
        paths.config_toml = dir.path().join("nested").join("config.toml");

        write_settings(&paths, &VoiceConfig::default()).unwrap();
        assert!(paths.config_toml.is_file());
        let lv = VoiceConfig::load(&paths);
        assert_eq!(
            lv.stt_engine,
            vec![SttEngine::BuiltIn, SttEngine::System, SttEngine::ClaudeCode]
        );
    }

    #[test]
    fn config_toml_is_native_typed_round_trip() {
        // Write a non-default config, then re-LOAD it from the TOML file: every enum
        // token + the numeric capture_gain must survive a typed TOML round-trip (no
        // JSON in between). Also assert the on-disk text is native TOML, not JSON.
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::resolve().expect("resolve");
        paths.config_toml = dir.path().join("config.toml");

        let v = VoiceConfig {
            stt_engine: vec![SttEngine::BuiltIn],
            tts_engine: vec![TtsEngine::System],
            narrate: Vec::new(),
            full_duplex: true,
            tts_rate: 1.25,
            capture_gain: CaptureGain::Manual(3.5),
            tts_built_in_voices: vec!["am_adam".into(), "af_bella".into()],
            ..VoiceConfig::default()
        };
        write_settings(&paths, &v).unwrap();

        let text = std::fs::read_to_string(&paths.config_toml).unwrap();
        assert!(
            text.contains("stt_engine = [\"built_in\"]"),
            "native TOML array of tokens, got:\n{text}"
        );
        assert!(
            !text.trim_start().starts_with('{'),
            "must be TOML, not JSON"
        );
        assert!(
            text.contains("capture_gain = 3.5"),
            "manual gain as a TOML number"
        );

        let r = VoiceConfig::load(&paths);
        assert_eq!(r.stt_engine, vec![SttEngine::BuiltIn]);
        assert_eq!(r.tts_engine, vec![TtsEngine::System]);
        assert!(
            r.narrate.is_empty(),
            "empty narrate set round-trips through TOML"
        );
        assert!(r.full_duplex);
        assert_eq!(r.tts_rate, 1.25);
        assert_eq!(r.capture_gain.manual(), Some(3.5));
        assert_eq!(r.current_voice(), "am_adam");
    }

    #[test]
    fn load_clamps_out_of_range_rate() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::resolve().expect("resolve");
        paths.config_toml = dir.path().join("config.toml");
        // A hand-edited rate well past the 0.5–2.0 range is clamped on load.
        std::fs::write(&paths.config_toml, "tts_rate = 5.0\n").unwrap();
        assert_eq!(VoiceConfig::load(&paths).tts_rate, 2.0);
        std::fs::write(&paths.config_toml, "tts_rate = 0.1\n").unwrap();
        assert_eq!(VoiceConfig::load(&paths).tts_rate, 0.5);
    }

    #[test]
    fn load_tolerates_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Paths::resolve().expect("resolve");
        paths.config_toml = dir.path().join("config.toml");
        // A typo'd key is ignored by the typed parse; the known key still loads.
        std::fs::write(
            &paths.config_toml,
            "narate = \"off\"\nstt_engine = [\"built_in\"]\n",
        )
        .unwrap();
        let cfg = VoiceConfig::load(&paths);
        assert_eq!(
            cfg.stt_engine,
            vec![SttEngine::BuiltIn],
            "array ladder ['built_in'] loads as a one-rung ladder"
        );
        assert_eq!(
            cfg.narrate,
            vec![NarrateKind::Shorts, NarrateKind::Digests],
            "typo'd 'narate' ignored → default narration set"
        );
        assert!(VoiceConfig::known_keys().contains("stt_engine"));
        assert!(!VoiceConfig::known_keys().contains("narate"));
    }
}
