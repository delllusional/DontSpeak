//! Voice enumeration for the CURRENTLY-SELECTED engine, filtered to a language.
//!
//! A small, dependency-light home for "what voices can I pick right now?" so it
//! is shared by every consumer instead of re-derived:
//!   - the SwiftUI/Slint picker (`ds-core` delegates its `kokoro_ids` /
//!     `system_voices` here), and
//!   - the MCP server's `list_voices` tool (which has no access to the FFI
//!     `ds-core` crate).
//!
//! Two engines, two id conventions:
//!   - Kokoro: opaque `<lang><gender>_name` ids read from `voices-v1.0.bin` when
//!     present (NEVER downloaded here), else a static fallback set. The leading
//!     char is the language family (`a` American + `b` British English; Kokoro
//!     ships no German voices, so German is intentionally absent for now), the
//!     second is the gender (`f`/`m`).
//!   - System: `say -v ?` (macOS) → [`SpeakerVoice`] carrying a BCP-47
//!     `language_tag` (`en-US`, `de-DE`, …); empty off-host.
//!
//! Everything except the disk read (`kokoro_voice_ids`) and the `say` shell-out
//! (`system_voices`) is PURE and unit-tested with no model, no audio, no network.

use ds_config::{TtsEngine, VoiceConfig};

use crate::{Gender, Quality, SpeakerVoice, say, voices};

/// One pickable voice for the current engine+language: the opaque engine `id`
/// (handed back to `speak`/`settings.json`) and a tidy human `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChoice {
    pub id: String,
    pub label: String,
}

/// A small fallback Kokoro id set shown when `voices-v1.0.bin` is absent, so the
/// list is never empty and the well-known English defaults always appear. Kokoro
/// ships no German voices, so none are listed here.
pub const KOKORO_FALLBACK_IDS: &[&str] = &[
    "af_sarah",
    "af_heart",
    "am_michael",
    "am_adam",
    "bf_emma",
    "bm_george",
];

// ── Kokoro id parsing (PURE) ─────────────────────────────────────────────────

/// The language subtag a Kokoro id belongs to, from its leading family char
/// (`af_sarah` → "en"). Unknown shapes → "other". German is intentionally NOT
/// mapped: Kokoro ships no German voices, so the `d` family stays "other" for now.
pub fn kokoro_language(id: &str) -> &'static str {
    match id.as_bytes().first() {
        // `a` American + `b` British English both reply in "en".
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

/// The full BCP-47 tag a Kokoro id implies. The English families carry a region
/// (`a` American → "en-US", `b` British → "en-GB"); every other family has no
/// region in Kokoro's ids, so it falls back to the bare [`kokoro_language`] subtag.
pub fn kokoro_language_tag(id: &str) -> String {
    match id.as_bytes().first() {
        Some(b'a') => "en-US".to_string(),
        Some(b'b') => "en-GB".to_string(),
        _ => kokoro_language(id).to_string(),
    }
}

/// The gender a Kokoro id encodes in its second char (`af_…` Female, `am_…`
/// Male). Unknown shapes → `None`.
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

/// A short accent hint for the English families (`a` American, `b` British);
/// `None` for every other family (the language already says it).
fn kokoro_accent(id: &str) -> Option<&'static str> {
    match id.as_bytes().first() {
        Some(b'a') => Some("American"),
        Some(b'b') => Some("British"),
        _ => None,
    }
}

/// Turn a Kokoro id into a tidy display name (`af_sarah` → "Sarah").
pub fn kokoro_display_name(id: &str) -> String {
    let raw = id.split_once('_').map(|(_, n)| n).unwrap_or(id);
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

/// A human label for a Kokoro id, e.g. "Sarah (American, Female)".
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

/// The serialized gender word for a [`Gender`] (`"female"`/`"male"`), or `None`.
pub fn gender_str(g: Option<Gender>) -> Option<&'static str> {
    match g {
        Some(Gender::Female) => Some("female"),
        Some(Gender::Male) => Some("male"),
        None => None,
    }
}

// ── Engine voice id sources (disk / shell — the only impure bits) ────────────

/// Read Kokoro voice ids from the downloaded `voices-v1.0.bin` if it is present;
/// otherwise return the static fallback set. NEVER downloads. Probes disk only.
pub fn kokoro_voice_ids() -> Vec<String> {
    if let Some(path) = ds_model::model_path(ds_model::KOKORO_VOICES_FILE)
        && path.is_file()
        && let Ok(bytes) = std::fs::read(&path)
        && let Ok(names) = voices::voice_names(&bytes)
        && !names.is_empty()
    {
        return names;
    }
    KOKORO_FALLBACK_IDS.iter().map(|s| s.to_string()).collect()
}

/// Enumerate System voices via `say -v ?` (macOS) — empty off-host. Shells out
/// to `say` (no network).
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

/// The BCP-47 primary subtag of a tag (`en-US` → "en"); the whole string if it
/// has no `-`. Lower-cased so comparisons are case-insensitive.
pub fn primary_subtag(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

/// Kokoro voices for `language` (e.g. "en", "de"), sorted by label. Reads the
/// voices bin once (or the fallback set).
pub fn kokoro_choices(language: &str) -> Vec<VoiceChoice> {
    kokoro_choices_from(&kokoro_voice_ids(), language)
}

/// PURE filter+label+sort of Kokoro `ids` to `language` (factored out of
/// [`kokoro_choices`] so it is unit-tested without the disk read).
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

/// System voices for `language`, sorted by label. Reads `say -v ?` once. The
/// label carries an Enhanced/Premium quality hint where the OS reports one.
pub fn system_choices(language: &str) -> Vec<VoiceChoice> {
    system_choices_from(&system_voices(), language)
}

/// PURE filter+label+sort of System `voices` to `language` (factored out of
/// [`system_choices`] so it is unit-tested without the `say` shell-out).
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

/// A human label for a System voice ("Samantha", "Ava (Premium)").
fn system_label(v: &SpeakerVoice) -> String {
    match v.quality {
        Some(Quality::Enhanced) if !v.name.contains("Enhanced") => format!("{} (Enhanced)", v.name),
        Some(Quality::Premium) if !v.name.contains("Premium") => format!("{} (Premium)", v.name),
        _ => v.name.clone(),
    }
}

// ── Current voice NAME for the active engine (the single cross-platform resolver) ─────────

/// Tidy a raw System-TTS voice name into a short, speakable form for the greeting/UI: drop the
/// `"Microsoft "` vendor prefix and the legacy `" Desktop"` suffix, plus any trailing
/// ` (Quality)` parenthetical — so `"Microsoft Hazel Desktop"` → `"Hazel"`, `"Ava (Premium)"`
/// → `"Ava"`, and a plain `"Samantha"` is unchanged.
pub fn friendly_system_name(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_prefix("Microsoft ").unwrap_or(s);
    let s = s.strip_suffix(" Desktop").unwrap_or(s);
    let s = s.split(" (").next().unwrap_or(s);
    s.trim().to_string()
}

/// The DISPLAY name of a resolved `(engine, voice)` — the ONE place that turns "what is
/// speaking" into a short, speakable name, shared by the greeting and the UI:
/// * Kokoro → the voice id's friendly name (`af_sarah` → "Sarah").
/// * System → the configured voice tidied, or — when empty — the OS DEFAULT voice's
///   name (the exact voice narration uses), tidied. `None` if the default can't be read.
/// * Off    → `None`.
pub fn voice_display_name(engine: TtsEngine, voice: &str) -> Option<String> {
    match engine {
        TtsEngine::Off => None,
        TtsEngine::Kokoro => Some(kokoro_display_name(voice)),
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

/// The name of the voice CURRENTLY selected for the active engine — the single source the UI
/// shows and the greeting names. Thin wrapper over [`voice_display_name`] for the engine's
/// default/current voice (Kokoro: `current_voice()`; System: `tts_system_voice`, else the OS
/// default).
pub fn current_voice_name(cfg: &VoiceConfig) -> Option<String> {
    // The active engine is the `tts_engine` ladder's first usable rung (None ⇒ TTS off).
    let engine = cfg.resolved_tts()?;
    let voice = match engine {
        TtsEngine::Off => return None, // unreachable: a resolved rung is usable
        TtsEngine::Kokoro => cfg.current_voice(),
        TtsEngine::System => cfg.tts_system_voice.clone(),
    };
    voice_display_name(engine, &voice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_language_from_family_char() {
        assert_eq!(kokoro_language("af_sarah"), "en");
        assert_eq!(kokoro_language("bm_george"), "en");
        assert_eq!(kokoro_language("ef_dora"), "es");
        // German is removed for now: the `d` family is not mapped to a language.
        assert_eq!(kokoro_language("df_anna"), "other");
        assert_eq!(kokoro_language("dm_klaus"), "other");
        // Unknown shapes never panic.
        assert_eq!(kokoro_language("weird"), "other");
        assert_eq!(kokoro_language(""), "other");
    }

    #[test]
    fn kokoro_language_tag_carries_english_region() {
        assert_eq!(kokoro_language_tag("af_sarah"), "en-US"); // American
        assert_eq!(kokoro_language_tag("am_adam"), "en-US");
        assert_eq!(kokoro_language_tag("bm_george"), "en-GB"); // British
        assert_eq!(kokoro_language_tag("ef_dora"), "es"); // no region for other families
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
        // A non-English family (no accent hint): just the gender is shown.
        assert_eq!(kokoro_label("ef_dora"), "Dora (Female)");
    }

    #[test]
    fn kokoro_choices_filter_by_language_and_sort() {
        // Drive the PURE filter/label/sort over a fixed id set (no disk read, so
        // the assertion holds regardless of which voices bin is installed).
        let ids: Vec<String> = ["am_michael", "af_sarah", "df_anna", "bm_george"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let en = kokoro_choices_from(&ids, "en");
        assert!(en.iter().all(|c| kokoro_language(&c.id) == "en"));
        assert!(en.iter().any(|c| c.id == "af_sarah"));
        assert!(en.iter().all(|c| c.id != "df_anna")); // the `d` family is excluded.
        // Sorted by label.
        let labels: Vec<&str> = en.iter().map(|c| c.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);

        // German is removed for now: no language selects the `d` family.
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
        assert_eq!(en.len(), 2); // both en-US and en-GB match "en".
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
        // Kokoro id → friendly first name.
        assert_eq!(
            voice_display_name(TtsEngine::Kokoro, "af_sarah").as_deref(),
            Some("Sarah")
        );
        // System with an EXPLICIT voice tidies it (no OS-default query path).
        assert_eq!(
            voice_display_name(TtsEngine::System, "Microsoft Zira Desktop").as_deref(),
            Some("Zira")
        );
        // Off never names a voice.
        assert_eq!(voice_display_name(TtsEngine::Off, "af_sarah"), None);
    }

    #[test]
    fn current_voice_name_off_and_resolved_engine() {
        // Off (empty `tts_engine` ladder) never names a voice — on every platform.
        let off = VoiceConfig {
            tts_engine: Vec::new(),
            ..Default::default()
        };
        assert_eq!(current_voice_name(&off), None);

        // A single-rung ladder names that engine's voice WHERE the rung is usable here, else
        // resolves to off → None. Asserting against `resolved_tts()` keeps this platform-robust
        // (Kokoro isn't usable on x86_64 macOS; System isn't wired on Linux).
        let kokoro = VoiceConfig {
            tts_engine: vec![TtsEngine::Kokoro],
            tts_built_in_voices: vec!["am_michael".into()],
            ..Default::default()
        };
        assert_eq!(
            current_voice_name(&kokoro),
            kokoro.resolved_tts().map(|_| "Michael".into())
        );

        let system = VoiceConfig {
            tts_engine: vec![TtsEngine::System],
            tts_system_voice: "Microsoft Zira Desktop".into(),
            ..Default::default()
        };
        assert_eq!(
            current_voice_name(&system),
            system.resolved_tts().map(|_| "Zira".into())
        );
    }
}
