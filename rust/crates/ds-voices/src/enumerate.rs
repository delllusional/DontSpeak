//! Voice list for selected engine + language (host picker + MCP). Kokoro ids from
//! the active ONNX/MLX model files (never downloads) or static fallback; system via `say -v ?`.
//! Pure filter/label except disk/`say`.

use ds_config::{TtsEngine, TtsModel};

use crate::{Gender, Quality, SpeakerVoice, say, voices};

/// One pickable voice: opaque engine `id` (handed back to `speak`/`settings.json`)
/// and a tidy human `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChoice {
    pub id: String,
    pub label: String,
}

/// Fallback Kokoro ids when `voices-v1.0.bin` is absent, so the list is never empty
/// and well-known English defaults always appear. No German (Kokoro ships none).
pub const KOKORO_FALLBACK_IDS: &[&str] = &[
    "af_sarah",
    "af_heart",
    "am_michael",
    "am_adam",
    "bf_emma",
    "bm_george",
];

/// Filename only — download pin stays `ds_model::KOKORO_VOICES_FILE` (issue #5: no ds-model dep).
/// Drift: `kokoro_voices_filename_matches_ds_model_registry`.
const KOKORO_VOICES_FILE: &str = "voices-v1.0.bin";
/// Directory name only; drift-guarded against ds-model without adding it as a runtime dependency.
const KOKORO_MLX_DIR_NAME: &str = "kokoro-82m";

// ── Kokoro id parsing (PURE) ─────────────────────────────────────────────────

/// Language a Kokoro id's leading family char actually speaks (`af_sarah` → "en"),
/// including the published-but-unrouted `j`/`z` families — the router locks those out
/// instead of mistaking them for language-agnostic voices. Whether this build ships a
/// frontend for that language is a separate question ([`is_routable_kokoro_voice`]).
/// Unknown shapes → "other"; Kokoro publishes no German family at all.
pub fn kokoro_language(id: &str) -> &'static str {
    match id.as_bytes().first() {
        // `a` American + `b` British both → "en".
        Some(b'a') | Some(b'b') => "en",
        Some(b'e') => "es",
        Some(b'f') => "fr",
        Some(b'h') => "hi",
        Some(b'i') => "it",
        Some(b'j') => "ja",
        Some(b'p') => "pt",
        Some(b'z') => "zh",
        _ => "other",
    }
}

/// Full BCP-47 tag. English families carry region (`a` → "en-US", `b` → "en-GB");
/// others fall back to the bare [`kokoro_language`] subtag.
pub fn kokoro_language_tag(id: &str) -> String {
    match id.as_bytes().first() {
        Some(b'a') => "en-US".to_string(),
        Some(b'b') => "en-GB".to_string(),
        _ => kokoro_language(id).to_string(),
    }
}

/// Gender from second char (`af_…` Female, `am_…` Male). Unknown → `None`.
pub fn kokoro_gender(id: &str) -> Option<Gender> {
    let bytes = id.as_bytes();
    if bytes.len() >= 3 && bytes[2] == b'_' {
        match bytes[1] {
            b'f' | b'F' => return Some(Gender::Female),
            b'm' | b'M' => return Some(Gender::Male),
            _ => {}
        }
    }
    None
}

/// Accent hint for English families only (`a` American, `b` British).
fn kokoro_accent(id: &str) -> Option<&'static str> {
    match id.as_bytes().first() {
        Some(b'a') => Some("American"),
        Some(b'b') => Some("British"),
        _ => None,
    }
}

/// Display name from a Kokoro id (`af_sarah` → "Sarah").
pub fn kokoro_display_name(id: &str) -> String {
    let raw = id.split_once('_').map(|(_, n)| n).unwrap_or(id);
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

/// Human label, e.g. "Sarah (American, Female)".
fn kokoro_label(id: &str) -> String {
    let name = kokoro_display_name(id);
    let mut parts: Vec<&str> = Vec::new();
    if let Some(a) = kokoro_accent(id) {
        parts.push(a);
    }
    match kokoro_gender(id) {
        Some(Gender::Female) => parts.push("Female"),
        Some(Gender::Male) => parts.push("Male"),
        None => {}
    }
    if parts.is_empty() {
        name
    } else {
        format!("{name} ({})", parts.join(", "))
    }
}

/// Serialized gender word (`"female"`/`"male"`), or `None`.
pub fn gender_str(g: Option<Gender>) -> Option<&'static str> {
    match g {
        Some(Gender::Female) => Some("female"),
        Some(Gender::Male) => Some("male"),
        None => None,
    }
}

// ── Engine voice id sources (disk / shell — the only impure bits) ────────────

fn mlx_voice_ids_from_dir(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|s| s.to_str()) == Some("safetensors"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Kokoro voice ids actually on disk — the active ONNX voices bin, else the MLX voices
/// dir. `None` when neither source yields names (fresh install: validation must fall back
/// to shape rules, not the static list). NEVER downloads. Probes disk only.
pub fn kokoro_disk_voice_ids() -> Option<Vec<String>> {
    if let Some(path) = ds_config::model_dir().map(|d| d.join(KOKORO_VOICES_FILE))
        && path.is_file()
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(names) = voices::voice_names(&bytes)
        && !names.is_empty()
    {
        return Some(names);
    }
    if let Some(dir) = ds_config::mlx_dir().map(|d| d.join(KOKORO_MLX_DIR_NAME).join("voices")) {
        let names = mlx_voice_ids_from_dir(&dir);
        if !names.is_empty() {
            return Some(names);
        }
    }
    None
}

/// [`kokoro_disk_voice_ids`] with the static fallback, so a display list is never empty.
pub fn kokoro_voice_ids() -> Vec<String> {
    kokoro_disk_voice_ids()
        .unwrap_or_else(|| KOKORO_FALLBACK_IDS.iter().map(|s| s.to_string()).collect())
}

/// System voices via `say -v ?` (macOS) — empty off-host. No network.
pub fn system_voices() -> Vec<SpeakerVoice> {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(o) = Command::new("say").arg("-v").arg("?").output()
            && o.status.success()
        {
            let text = String::from_utf8_lossy(&o.stdout);
            return say::parse_say_voices(&text);
        }
        Vec::new()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = say::parse_say_voices; // keep the import used off-host.
        Vec::new()
    }
}

// ── Language-filtered choice lists (PURE over the fetched ids/voices) ─────────

/// BCP-47 primary subtag (`en-US` → "en"); whole string if no `-`. Lower-cased.
pub fn primary_subtag(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

/// Kokoro voices for `language`, sorted by label.
pub fn kokoro_choices(language: &str) -> Vec<VoiceChoice> {
    kokoro_choices_from(&kokoro_voice_ids(), language)
}

/// PURE filter+label+sort of Kokoro `ids` (unit-tested without the disk read).
pub fn kokoro_choices_from(ids: &[String], language: &str) -> Vec<VoiceChoice> {
    let want = language.to_ascii_lowercase();
    let mut out: Vec<VoiceChoice> = ids
        .iter()
        .filter(|id| {
            is_routable_kokoro_voice(id)
                && if want.contains('-') {
                    kokoro_language_tag(id).eq_ignore_ascii_case(&want)
                } else {
                    kokoro_language(id) == want
                }
        })
        .map(|id| VoiceChoice {
            label: kokoro_label(id),
            id: id.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// PURE filter+label+sort of System `voices` (unit-tested without `say`).
pub fn system_choices_from(voices: &[SpeakerVoice], language: &str) -> Vec<VoiceChoice> {
    let want = primary_subtag(language);
    let mut out: Vec<VoiceChoice> = voices
        .iter()
        .filter(|v| primary_subtag(&v.language_tag) == want)
        .map(|v| VoiceChoice {
            id: v.id.clone(),
            label: system_label(v),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// System voice label ("Samantha", "Ava (Premium)").
fn system_label(v: &SpeakerVoice) -> String {
    match v.quality {
        Some(Quality::Enhanced) if !v.name.contains("Enhanced") => format!("{} (Enhanced)", v.name),
        Some(Quality::Premium) if !v.name.contains("Premium") => format!("{} (Premium)", v.name),
        _ => v.name.clone(),
    }
}

/// Built-in voices for a model and language.
pub fn built_in_choices(model: TtsModel, language: &str) -> Vec<VoiceChoice> {
    if model == TtsModel::Kokoro {
        return kokoro_choices(&primary_subtag(language));
    }
    model
        .descriptor()
        .voices
        .iter()
        .map(|voice| VoiceChoice {
            id: (*voice).to_string(),
            label: model_voice_label(model, voice),
        })
        .collect()
}

/// Which catalog a voice pool is drawn from. `System` carries its enumerated voices so the
/// language rules below stay pure — `say -v ?` is never run from here.
#[derive(Debug, Clone, Copy)]
pub enum VoiceCatalog<'a> {
    BuiltIn(TtsModel),
    System(&'a [SpeakerVoice]),
}

impl VoiceCatalog<'_> {
    /// Language `voice` can only speak, or `None` when it speaks whichever language synthesis
    /// is given. Kokoro encodes it in the id family char and System voices carry a locale tag;
    /// Chatterbox, Qwen, and OmniVoice condition on the language argument instead, so their
    /// voices are never locked. An id absent from the System catalog is treated as unlocked:
    /// those names are freeform, and a name we cannot resolve is no evidence of a mismatch.
    /// An unrouted Kokoro family reports its own language, so [`Self::pool_for_language`]
    /// drops it for every language this build actually speaks.
    pub fn voice_language(&self, voice: &str) -> Option<String> {
        match self {
            Self::BuiltIn(TtsModel::Kokoro) => match kokoro_language(voice) {
                "other" => None,
                language => Some(language.to_string()),
            },
            Self::BuiltIn(_) => None,
            Self::System(voices) => voices
                .iter()
                .find(|candidate| candidate.id == voice)
                .map(|candidate| primary_subtag(&candidate.language_tag)),
        }
    }

    /// Pool entries able to speak `language`. Unlocked voices always qualify, so a catalog of
    /// them returns `pool` unchanged and callers need no per-engine branch. Empty only when
    /// every entry is locked to some other language.
    pub fn pool_for_language(&self, pool: &[String], language: &str) -> Vec<String> {
        let want = primary_subtag(language);
        pool.iter()
            .filter(|voice| self.voice_language(voice).is_none_or(|owned| owned == want))
            .cloned()
            .collect()
    }

    /// Every shipped voice that can speak `language` — the stand-in pool for a language the
    /// user configured no voice for. Same listing the picker UI and the `voices` tool show, so
    /// a borrowed voice is always one the catalog actually offers. Empty when the catalog has
    /// none, including a fresh install whose Kokoro ids are still the static English fallback.
    pub fn voices_for_language(&self, language: &str) -> Vec<String> {
        match self {
            Self::BuiltIn(model) => built_in_choices(*model, language)
                .into_iter()
                .map(|choice| choice.id)
                .collect(),
            Self::System(voices) => system_choices_from(voices, &primary_subtag(language))
                .into_iter()
                .map(|choice| choice.id)
                .collect(),
        }
    }
}

/// Whether `voice` is a real id for `model` — the full model catalog, not the configured pool
/// (Kokoro ships many voices beyond a two-voice pool). Kokoro checks the ids actually on disk and
/// accepts anything before they exist (same rule as `set_config`: never validate a pack voice
/// against the static fallback); every other model checks its registry `voices`. Used to reject a
/// stale per-utterance voice that outlived a model switch (e.g. Chatterbox's `"default"` reaching
/// Kokoro) before it is handed to the synth backend.
pub fn is_model_voice(model: TtsModel, voice: &str) -> bool {
    if model == TtsModel::Kokoro {
        return kokoro_disk_voice_ids().is_none_or(|ids| ids.iter().any(|id| id == voice));
    }
    model.descriptor().voices.contains(&voice)
}

/// The pool the router picks from for `model`, and the configured entries it locks out.
/// Entries locked to a language this build ships no frontend for are dropped; when that
/// empties a non-empty pool the model defaults substitute, matching `VoiceConfig::clamp`'s
/// empty-pool rule. An unknown shape (a nonstandard MLX pack id) stays — existing config is
/// not discarded on that weaker evidence; new input is judged strictly instead
/// ([`is_routable_kokoro_voice`]). Pure: no disk, no `say`. Single source for the router,
/// `status`, and `voices`, so those three cannot name different pools.
pub fn effective_builtin_pool(model: TtsModel, configured: &[String]) -> EffectivePool {
    let catalog = VoiceCatalog::BuiltIn(model);
    let descriptor = model.descriptor();
    let (voices, ignored): (Vec<String>, Vec<String>) =
        configured.iter().cloned().partition(|voice| {
            catalog
                .voice_language(voice)
                .is_none_or(|language| descriptor.supports_language(&language))
        });
    if voices.is_empty() && !configured.is_empty() {
        return EffectivePool {
            voices: descriptor
                .default_voices
                .iter()
                .map(|voice| (*voice).to_string())
                .collect(),
            ignored,
        };
    }
    EffectivePool { voices, ignored }
}

/// Result of [`effective_builtin_pool`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePool {
    /// What the router picks from.
    pub voices: Vec<String>,
    /// Configured entries this build cannot route; empty when every entry is usable.
    pub ignored: Vec<String>,
}

/// The published language `id`'s Kokoro family speaks but this build ships no frontend for
/// (`ja`, `zh`), or `None` when the build can route `id`. An unknown family char names no
/// language, so — like an unlocked voice — it conditions on the detected language and is
/// routable. That match with [`effective_builtin_pool`], which keeps unknown shapes, is
/// load-bearing: split the two and a voice the router speaks with gets refused elsewhere,
/// naming a reason that is false for it. Allocation-free: [`kokoro_choices_from`] runs this
/// over the whole catalog per listing.
fn kokoro_unrouted_language(id: &str) -> Option<&'static str> {
    match kokoro_language(id) {
        "other" => None,
        language if !TtsModel::Kokoro.descriptor().supports_language(language) => Some(language),
        _ => None,
    }
}

/// Whether this build ships a frontend for `id`. Kokoro publishes Japanese and Mandarin
/// voices whose pipelines were dropped; they are real model ids, so only the language rules
/// them out — in a configured pool (`set_config`) and per utterance (`speak`) alike.
pub fn is_routable_kokoro_voice(id: &str) -> bool {
    kokoro_unrouted_language(id).is_none()
}

/// Why this build refuses `id` as NEW Kokoro input — `set_config` pool edits and `speak`
/// overrides share the sentence so neither states the wrong reason. `None` exactly when
/// [`is_routable_kokoro_voice`] is true.
pub fn kokoro_route_refusal(id: &str) -> Option<String> {
    kokoro_unrouted_language(id).map(|language| {
        format!("`{id}` speaks {language}, a language this build cannot route; see voices")
    })
}

/// Reject a per-utterance Kokoro voice this build cannot route. Other targets need no check
/// here: System names are freeform, and every other model's per-utterance id is already
/// gated against its registry at admit (`gate_item` -> `is_model_voice`) and clamped again
/// in the helper (`supported_voice`).
pub fn validate_speak_voices(args: &ds_config::TtsArgPools) -> Result<(), String> {
    match args
        .for_target(TtsEngine::BuiltIn, TtsModel::Kokoro)
        .and_then(ds_config::TtsTargetArgs::voice)
    {
        Some(voice) => kokoro_route_refusal(voice).map_or(Ok(()), |refusal| {
            Err(format!("tts_args.kokoro.voice {refusal}"))
        }),
        None => Ok(()),
    }
}

/// Non-refusing clamp of a per-utterance voice to `model` — the voice-axis twin of
/// `ds_tts::supported_language`. The engine gates a voice against the model active when the
/// utterance was queued, but the model can change before the warm helper synthesizes it. A voice
/// that no longer belongs to `model` is replaced with the model's first default voice rather than
/// dropped — Chatterbox/Qwen/OmniVoice have no per-voice fallback and would drop the utterance. A
/// voice already valid for `model` (including any voice on a fresh install, before the Kokoro
/// catalog is on disk) is returned unchanged.
pub fn supported_voice(model: TtsModel, voice: &str) -> String {
    if is_model_voice(model, voice) {
        return voice.to_string();
    }
    model
        .descriptor()
        .default_voices
        .first()
        .map(|v| v.to_string())
        .unwrap_or_else(|| voice.to_string())
}

fn model_voice_label(model: TtsModel, voice: &str) -> String {
    if voice == "default" {
        return "Default".to_string();
    }
    if model == TtsModel::Kokoro {
        return kokoro_display_name(voice);
    }
    voice
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Current voice NAME for the active engine (single cross-platform resolver) ─

/// Tidy a raw System-TTS name for greeting/UI: drop `"Microsoft "` prefix,
/// legacy `" Desktop"` suffix, and trailing ` (Quality)` — so
/// `"Microsoft Hazel Desktop"` → `"Hazel"`, `"Ava (Premium)"` → `"Ava"`.
pub fn friendly_system_name(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_prefix("Microsoft ").unwrap_or(s);
    let s = s.strip_suffix(" Desktop").unwrap_or(s);
    let s = s.split(" (").next().unwrap_or(s);
    s.trim().to_string()
}

/// DISPLAY name of a resolved `(engine, model, voice)` — the ONE place that turns "what
/// is speaking" into a short speakable name (greeting + UI):
/// * Kokoro → friendly name (`af_sarah` → "Sarah").
/// * Other built-in models → registry voice label.
/// * System → configured voice tidied, or — when empty — OS DEFAULT voice tidied.
///   `None` if the default can't be read.
pub fn voice_display_name(engine: TtsEngine, model: TtsModel, voice: &str) -> Option<String> {
    match engine {
        TtsEngine::BuiltIn => model
            .descriptor()
            .voices
            .contains(&voice)
            .then(|| model_voice_label(model, voice))
            .or_else(|| (model == TtsModel::Kokoro).then(|| kokoro_display_name(voice))),
        TtsEngine::System => {
            let raw = if voice.trim().is_empty() {
                crate::system::default_voice_name()?
            } else {
                voice.to_string()
            };
            let name = friendly_system_name(&raw);
            (!name.is_empty()).then_some(name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_voices_filename_matches_ds_model_registry() {
        // Drift guard (see KOKORO_VOICES_FILE): keep the local duplicate in sync
        // without making ds-model a real dependency.
        assert_eq!(KOKORO_VOICES_FILE, ds_model::KOKORO_VOICES_FILE);
        assert_eq!(KOKORO_MLX_DIR_NAME, ds_model::mlx_repo::KOKORO_MLX_DIR_NAME);
    }

    #[test]
    fn is_model_voice_rejects_ids_from_a_different_model() {
        // The stale-voice leak this guards: a `"default"` (Chatterbox/OmniVoice pool) or a Kokoro
        // id must not read as valid for a model that does not own it. Non-Kokoro membership is a
        // pure registry check (no disk), so this stays hermetic.
        assert!(is_model_voice(TtsModel::Chatterbox, "default"));
        assert!(!is_model_voice(TtsModel::Chatterbox, "af_sarah"));
        assert!(is_model_voice(TtsModel::Qwen, "sohee"));
        assert!(!is_model_voice(TtsModel::Qwen, "default"));
        assert!(is_model_voice(TtsModel::OmniVoice, "young_woman"));
        assert!(is_model_voice(TtsModel::OmniVoice, "default"));
        assert!(!is_model_voice(TtsModel::OmniVoice, "sohee"));
    }

    #[test]
    fn supported_voice_clamps_a_foreign_voice_to_the_model_default() {
        // A voice the model owns survives; one it does not is clamped to the model's first
        // default voice (never dropped, never passed through). Non-Kokoro membership is a pure
        // registry check, so this stays hermetic.
        assert_eq!(supported_voice(TtsModel::Qwen, "ryan"), "ryan");
        assert_eq!(
            supported_voice(TtsModel::Qwen, "default"),
            TtsModel::Qwen.descriptor().default_voices[0]
        );
        assert_eq!(
            supported_voice(TtsModel::Chatterbox, "sohee"),
            TtsModel::Chatterbox.descriptor().default_voices[0]
        );
    }

    #[test]
    fn mlx_voice_ids_are_read_from_safetensors_names() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bf_emma.safetensors"), b"").unwrap();
        std::fs::write(tmp.path().join("af_heart.safetensors"), b"").unwrap();
        std::fs::write(tmp.path().join("ignored.pt"), b"").unwrap();
        assert_eq!(
            mlx_voice_ids_from_dir(tmp.path()),
            vec!["af_heart".to_string(), "bf_emma".to_string()]
        );
    }

    #[test]
    fn kokoro_language_from_family_char() {
        assert_eq!(kokoro_language("af_sarah"), "en");
        assert_eq!(kokoro_language("bm_george"), "en");
        assert_eq!(kokoro_language("ef_dora"), "es");
        // Published but unrouted: they report their real language so the router can lock
        // them out rather than treat them as language-agnostic.
        assert_eq!(kokoro_language("jf_alpha"), "ja");
        assert_eq!(kokoro_language("zf_xiaobei"), "zh");
        // Unknown shapes never panic — Kokoro publishes no `d` family at all.
        assert_eq!(kokoro_language("df_anna"), "other");
        assert_eq!(kokoro_language("dm_klaus"), "other");
        assert_eq!(kokoro_language("weird"), "other");
        assert_eq!(kokoro_language(""), "other");
    }

    #[test]
    fn kokoro_language_tag_carries_english_region() {
        assert_eq!(kokoro_language_tag("af_sarah"), "en-US");
        assert_eq!(kokoro_language_tag("am_adam"), "en-US");
        assert_eq!(kokoro_language_tag("bm_george"), "en-GB");
        assert_eq!(kokoro_language_tag("ef_dora"), "es");
        assert_eq!(kokoro_language_tag("weird"), "other");
    }

    #[test]
    fn kokoro_gender_from_second_char() {
        assert_eq!(kokoro_gender("af_sarah"), Some(Gender::Female));
        assert_eq!(kokoro_gender("am_michael"), Some(Gender::Male));
        assert_eq!(kokoro_gender("xx_y"), None);
        assert_eq!(kokoro_gender(""), None);
    }

    #[test]
    fn kokoro_label_reads_naturally() {
        assert_eq!(kokoro_label("af_sarah"), "Sarah (American, Female)");
        assert_eq!(kokoro_label("bm_george"), "George (British, Male)");
        // Non-English family: gender only (no accent hint).
        assert_eq!(kokoro_label("ef_dora"), "Dora (Female)");
    }

    #[test]
    fn kokoro_choices_filter_by_language_and_sort() {
        // Fixed id set so the assertion is independent of which voices bin is installed.
        let ids: Vec<String> = ["am_michael", "af_sarah", "df_anna", "bm_george"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let en = kokoro_choices_from(&ids, "en");
        assert!(en.iter().all(|c| kokoro_language(&c.id) == "en"));
        assert!(en.iter().any(|c| c.id == "af_sarah"));
        assert!(en.iter().all(|c| c.id != "df_anna")); // `d` family excluded.
        let labels: Vec<&str> = en.iter().map(|c| c.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);

        // The `d` family is an unknown shape (Kokoro publishes none), so no language ever
        // selects it.
        assert!(kokoro_choices_from(&ids, "de").is_empty());
    }

    #[test]
    fn unrouted_kokoro_families_never_join_another_languages_pool() {
        let kokoro = VoiceCatalog::BuiltIn(TtsModel::Kokoro);
        assert_eq!(kokoro.voice_language("jf_alpha"), Some("ja".to_string()));
        assert_eq!(
            kokoro.pool_for_language(&["af_sarah".into(), "jf_alpha".into()], "en"),
            ["af_sarah"]
        );
        // #222's headline repro: a pool of only unrouted voices owns nothing.
        assert!(
            kokoro
                .pool_for_language(&["jf_alpha".into()], "en")
                .is_empty()
        );
        // The pickable list never starts advertising what the router cannot use.
        assert!(
            kokoro_choices_from(
                &["af_sarah".into(), "jf_alpha".into(), "zf_xiaobei".into()],
                "ja"
            )
            .is_empty()
        );

        assert!(is_routable_kokoro_voice("af_sarah"));
        assert!(is_routable_kokoro_voice("if_sara"));
        assert!(!is_routable_kokoro_voice("jf_alpha"));
        assert!(!is_routable_kokoro_voice("zf_xiaobei"));
        // Unknown shapes stay routable: no identifiable language means no lock, so `speak`
        // and the pool agree instead of one refusing what the other speaks with.
        assert!(is_routable_kokoro_voice("df_anna"));
        assert!(is_routable_kokoro_voice("xq_bogus"));
    }

    #[test]
    fn effective_builtin_pool_drops_locked_out_voices_and_substitutes_defaults() {
        let mixed = effective_builtin_pool(
            TtsModel::Kokoro,
            &["af_sarah".into(), "jf_alpha".into(), "zf_xiaobei".into()],
        );
        assert_eq!(mixed.voices, ["af_sarah"]);
        assert_eq!(mixed.ignored, ["jf_alpha", "zf_xiaobei"]);

        // Nothing routable left: silence is worse than the model's own defaults, and
        // `VoiceConfig::clamp` makes the same substitution for the models it can check.
        let locked = effective_builtin_pool(TtsModel::Kokoro, &["jf_alpha".into()]);
        assert_eq!(locked.voices, TtsModel::Kokoro.descriptor().default_voices);
        assert_eq!(locked.ignored, ["jf_alpha"]);

        // Existing config is not discarded on the weaker evidence of an unknown shape —
        // that is the nonstandard MLX pack id #222 warns against regressing.
        let unknown_shape = effective_builtin_pool(TtsModel::Kokoro, &["custom_v1".into()]);
        assert_eq!(unknown_shape.voices, ["custom_v1"]);
        assert!(unknown_shape.ignored.is_empty());

        let unlocked = effective_builtin_pool(TtsModel::Chatterbox, &["default".into()]);
        assert_eq!(unlocked.voices, ["default"]);
        assert!(unlocked.ignored.is_empty());

        // An empty pool is the user's own "speak with nothing", not an emptied one.
        let empty = effective_builtin_pool(TtsModel::Kokoro, &[]);
        assert!(empty.voices.is_empty());
        assert!(empty.ignored.is_empty());
    }

    #[test]
    fn kokoro_route_refusal_names_the_real_reason() {
        let ja = kokoro_route_refusal("jf_alpha").expect("an unrouted family is refused");
        assert!(ja.contains("speaks ja"), "got: {ja}");
        assert!(ja.contains("cannot route"), "got: {ja}");
        assert!(
            kokoro_route_refusal("zf_xiaobei")
                .expect("Mandarin is unrouted too")
                .contains("speaks zh")
        );
        // An unknown family char names no language, so it is not refused — the pool keeps it
        // and the router speaks with it (#222); refusing it here would state a false reason.
        for id in ["df_anna", "custom_v1", "xq_bogus"] {
            assert_eq!(kokoro_route_refusal(id), None, "{id}");
        }
        assert_eq!(kokoro_route_refusal("af_sarah"), None);
        assert_eq!(kokoro_route_refusal("if_sara"), None);
    }

    /// The allocation-free predicate and the sentence builder are separate entry points over
    /// one reason; this is what keeps them from drifting apart again.
    #[test]
    fn refusal_and_predicate_agree() {
        for id in [
            "af_sarah",
            "if_sara",
            "jf_alpha",
            "zf_xiaobei",
            "df_anna",
            "xq_bogus",
            "custom_v1",
        ] {
            assert_eq!(
                is_routable_kokoro_voice(id),
                kokoro_route_refusal(id).is_none(),
                "{id}"
            );
        }
    }

    #[test]
    fn validate_speak_voices_refuses_an_unroutable_kokoro_voice() {
        use ds_config::TtsArgPools;

        let unroutable =
            TtsArgPools::with_voice(TtsEngine::BuiltIn, TtsModel::Kokoro, "jf_alpha".into());
        assert!(
            validate_speak_voices(&unroutable)
                .unwrap_err()
                .contains("cannot route")
        );
        // An unknown family char is accepted, matching the pool that keeps it (#222); the
        // engine already clamps a per-utterance voice that is not on disk at synth time.
        let unknown_family =
            TtsArgPools::with_voice(TtsEngine::BuiltIn, TtsModel::Kokoro, "custom_v1".into());
        assert!(validate_speak_voices(&unknown_family).is_ok());
        assert!(
            validate_speak_voices(&TtsArgPools::with_voice(
                TtsEngine::BuiltIn,
                TtsModel::Kokoro,
                "if_sara".into()
            ))
            .is_ok()
        );
        // Other models keep their own registry gate; System names are freeform.
        assert!(
            validate_speak_voices(&TtsArgPools::with_voice(
                TtsEngine::BuiltIn,
                TtsModel::Chatterbox,
                "default".into()
            ))
            .is_ok()
        );
        assert!(
            validate_speak_voices(&TtsArgPools::with_voice(
                TtsEngine::System,
                TtsModel::Kokoro,
                "Anna".into()
            ))
            .is_ok()
        );
        assert!(validate_speak_voices(&TtsArgPools::default()).is_ok());
    }

    #[test]
    fn gender_str_maps_words() {
        assert_eq!(gender_str(Some(Gender::Female)), Some("female"));
        assert_eq!(gender_str(Some(Gender::Male)), Some("male"));
        assert_eq!(gender_str(None), None);
    }

    #[test]
    fn system_choices_filter_by_primary_subtag() {
        let mk = |id: &str, tag: &str| SpeakerVoice {
            id: id.into(),
            name: id.into(),
            language_tag: tag.into(),
            downloadable: false,
            gender: None,
            quality: None,
        };
        let voices = vec![
            mk("Samantha", "en-US"),
            mk("Daniel", "en-GB"),
            mk("Anna", "de-DE"),
        ];
        let en = system_choices_from(&voices, "en");
        assert_eq!(en.len(), 2); // en-US and en-GB both match "en".
        assert!(en.iter().all(|c| c.id != "Anna"));
        let de = system_choices_from(&voices, "de");
        assert_eq!(de.len(), 1);
        assert_eq!(de[0].id, "Anna");
    }

    #[test]
    fn primary_subtag_extracts_language() {
        assert_eq!(primary_subtag("en-US"), "en");
        assert_eq!(primary_subtag("de_DE"), "de");
        assert_eq!(primary_subtag("fr"), "fr");
        assert_eq!(primary_subtag("EN-gb"), "en");
    }

    fn system_fixture() -> Vec<SpeakerVoice> {
        let mk = |id: &str, tag: &str| SpeakerVoice {
            id: id.into(),
            name: id.into(),
            language_tag: tag.into(),
            downloadable: false,
            gender: None,
            quality: None,
        };
        vec![
            mk("Samantha", "en-US"),
            mk("Alice", "it-IT"),
            mk("Anna", "de-DE"),
        ]
    }

    #[test]
    fn locked_catalogs_report_a_voices_own_language() {
        let kokoro = VoiceCatalog::BuiltIn(TtsModel::Kokoro);
        assert_eq!(kokoro.voice_language("af_sarah").as_deref(), Some("en"));
        assert_eq!(kokoro.voice_language("if_sara").as_deref(), Some("it"));
        // An unparseable id must not claim a language it cannot back up.
        assert_eq!(kokoro.voice_language("weird"), None);

        let system = system_fixture();
        let system = VoiceCatalog::System(&system);
        assert_eq!(system.voice_language("Alice").as_deref(), Some("it"));
        // Freeform names absent from the catalog stay eligible everywhere.
        assert_eq!(system.voice_language("Unknown Voice"), None);
    }

    #[test]
    fn unlocked_catalogs_never_narrow_the_pool() {
        // Chatterbox/Qwen/OmniVoice condition on the language argument, so every voice stays
        // eligible and the shared filter is a no-op for them — no per-engine branch needed.
        let pool = vec!["sohee".to_string(), "ryan".to_string()];
        for model in [TtsModel::Chatterbox, TtsModel::Qwen, TtsModel::OmniVoice] {
            let catalog = VoiceCatalog::BuiltIn(model);
            assert_eq!(catalog.voice_language("sohee"), None);
            assert_eq!(catalog.pool_for_language(&pool, "it"), pool);
            // Their whole catalog speaks every language they support, so a stand-in pool is
            // the full voice list rather than a language-filtered slice.
            assert_eq!(catalog.voices_for_language("it"), model.descriptor().voices);
        }
    }

    #[test]
    fn pool_narrows_to_the_requested_language() {
        let catalog = VoiceCatalog::BuiltIn(TtsModel::Kokoro);
        let pool = vec![
            "af_sarah".to_string(),
            "bf_emma".to_string(),
            "if_sara".to_string(),
        ];
        assert_eq!(catalog.pool_for_language(&pool, "it"), vec!["if_sara"]);
        // Regional tags reduce to the primary subtag, and en-US/en-GB are one language.
        assert_eq!(
            catalog.pool_for_language(&pool, "en-GB"),
            vec!["af_sarah", "bf_emma"]
        );
        // No entry owns Spanish: empty, so the caller falls back rather than guessing.
        assert!(catalog.pool_for_language(&pool, "es").is_empty());
    }

    #[test]
    fn system_pool_and_default_follow_the_locale_tag() {
        let voices = system_fixture();
        let catalog = VoiceCatalog::System(&voices);
        let pool = vec!["Samantha".to_string(), "Alice".to_string()];
        assert_eq!(catalog.pool_for_language(&pool, "it"), vec!["Alice"]);
        assert_eq!(catalog.voices_for_language("de"), vec!["Anna"]);
        assert!(catalog.voices_for_language("fr").is_empty());
    }

    #[test]
    fn system_label_adds_quality_hint() {
        let mk = |name: &str, q: Option<Quality>| SpeakerVoice {
            id: name.into(),
            name: name.into(),
            language_tag: "en-US".into(),
            downloadable: false,
            gender: None,
            quality: q,
        };
        assert_eq!(
            system_label(&mk("Ava", Some(Quality::Premium))),
            "Ava (Premium)"
        );
        assert_eq!(
            system_label(&mk("Allison", Some(Quality::Enhanced))),
            "Allison (Enhanced)"
        );
        assert_eq!(
            system_label(&mk("Samantha", Some(Quality::Default))),
            "Samantha"
        );
        // Already-decorated names are not doubled.
        assert_eq!(
            system_label(&mk("Ava (Premium)", Some(Quality::Premium))),
            "Ava (Premium)"
        );
    }

    #[test]
    fn friendly_system_name_strips_vendor_and_suffix() {
        assert_eq!(friendly_system_name("Microsoft Hazel Desktop"), "Hazel");
        assert_eq!(friendly_system_name("Microsoft David"), "David");
        assert_eq!(friendly_system_name("Ava (Premium)"), "Ava");
        assert_eq!(friendly_system_name("Samantha"), "Samantha");
        assert_eq!(friendly_system_name("  Microsoft Zira Desktop  "), "Zira");
    }

    #[test]
    fn built_in_choices_follow_the_model_registry() {
        let choices = built_in_choices(TtsModel::Qwen, "ja");
        assert_eq!(choices.len(), TtsModel::Qwen.descriptor().voices.len());
        assert_eq!(choices[5].label, "Ono Anna");
    }

    #[test]
    fn voice_display_name_per_engine() {
        assert_eq!(
            voice_display_name(TtsEngine::BuiltIn, TtsModel::Kokoro, "af_sarah").as_deref(),
            Some("Sarah")
        );
        assert_eq!(
            voice_display_name(TtsEngine::BuiltIn, TtsModel::Chatterbox, "default").as_deref(),
            Some("Default")
        );
        assert_eq!(
            voice_display_name(TtsEngine::BuiltIn, TtsModel::Chatterbox, "bogus"),
            None
        );
        // Explicit System voice tidies without the OS-default query path.
        assert_eq!(
            voice_display_name(
                TtsEngine::System,
                TtsModel::Kokoro,
                "Microsoft Zira Desktop"
            )
            .as_deref(),
            Some("Zira")
        );
    }
}
