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

/// Language subtag from a Kokoro id's leading family char (`af_sarah` → "en").
/// Unknown shapes → "other". German (`d`) has no frontend; Japanese (`j`) and
/// Mandarin (`z`) lost theirs, so their voices stay unreachable.
pub fn kokoro_language(id: &str) -> &'static str {
    match id.as_bytes().first() {
        // `a` American + `b` British both → "en".
        Some(b'a') | Some(b'b') => "en",
        Some(b'e') => "es",
        Some(b'f') => "fr",
        Some(b'h') => "hi",
        Some(b'i') => "it",
        Some(b'p') => "pt",
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
            if want.contains('-') {
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
        assert!(is_model_voice(
            TtsModel::OmniVoice,
            "warm, clear female voice"
        ));
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
        // No frontend → "other": German was never mapped, Japanese and Mandarin were dropped.
        assert_eq!(kokoro_language("df_anna"), "other");
        assert_eq!(kokoro_language("dm_klaus"), "other");
        assert_eq!(kokoro_language("jf_alpha"), "other");
        assert_eq!(kokoro_language("zf_xiaobei"), "other");
        // Unknown shapes never panic.
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

        // German removed for now: no language selects the `d` family.
        assert!(kokoro_choices_from(&ids, "de").is_empty());
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
