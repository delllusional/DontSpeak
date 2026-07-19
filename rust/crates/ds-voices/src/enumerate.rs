//! Voice list for selected engine + language (host picker + MCP). Kokoro ids from
//! `voices-v1.0.bin` (never downloads) or static fallback; system via `say -v ?`.
//! Pure filter/label except disk/`say`.

use ds_config::TtsEngine;

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

/// Bare on-disk filename for the Kokoro voices asset — a DELIBERATE name-only
/// duplicate, NOT a new source of truth. `ds_model::KOKORO_VOICES_FILE` remains
/// the download pin (URL + SHA-256 + size); this crate must not depend on
/// `ds-model` as a real dep (issue #5). Dev-only test
/// `kokoro_voices_filename_matches_ds_model_registry` is the drift guard.
const KOKORO_VOICES_FILE: &str = "voices-v1.0.bin";

// ── Kokoro id parsing (PURE) ─────────────────────────────────────────────────

/// Language subtag from a Kokoro id's leading family char (`af_sarah` → "en").
/// Unknown shapes → "other". German (`d`) intentionally unmapped for now.
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

/// Kokoro voice ids from `voices-v1.0.bin` if present; else static fallback.
/// NEVER downloads. Probes disk only.
pub fn kokoro_voice_ids() -> Vec<String> {
    if let Some(path) = ds_config::model_dir().map(|d| d.join(KOKORO_VOICES_FILE))
        && path.is_file()
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(names) = voices::voice_names(&bytes)
        && !names.is_empty()
    {
        return names;
    }
    KOKORO_FALLBACK_IDS.iter().map(|s| s.to_string()).collect()
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
        .filter(|id| kokoro_language(id) == want)
        .map(|id| VoiceChoice {
            label: kokoro_label(id),
            id: id.clone(),
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

/// System voices for `language`, sorted by label. Label carries Enhanced/Premium
/// where the OS reports it.
pub fn system_choices(language: &str) -> Vec<VoiceChoice> {
    system_choices_from(&system_voices(), language)
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

/// DISPLAY name of a resolved `(engine, voice)` — the ONE place that turns "what
/// is speaking" into a short speakable name (greeting + UI):
/// * Kokoro → friendly name (`af_sarah` → "Sarah").
/// * System → configured voice tidied, or — when empty — OS DEFAULT voice tidied.
///   `None` if the default can't be read.
pub fn voice_display_name(engine: TtsEngine, voice: &str) -> Option<String> {
    match engine {
        TtsEngine::BuiltIn => Some(kokoro_display_name(voice)),
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
    }

    #[test]
    fn kokoro_language_from_family_char() {
        assert_eq!(kokoro_language("af_sarah"), "en");
        assert_eq!(kokoro_language("bm_george"), "en");
        assert_eq!(kokoro_language("ef_dora"), "es");
        // German removed for now: `d` family not mapped.
        assert_eq!(kokoro_language("df_anna"), "other");
        assert_eq!(kokoro_language("dm_klaus"), "other");
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
    fn voice_display_name_per_engine() {
        assert_eq!(
            voice_display_name(TtsEngine::BuiltIn, "af_sarah").as_deref(),
            Some("Sarah")
        );
        // Explicit System voice tidies without the OS-default query path.
        assert_eq!(
            voice_display_name(TtsEngine::System, "Microsoft Zira Desktop").as_deref(),
            Some("Zira")
        );
    }
}
