//! Voice list for selected engine + language (host picker + MCP). Kokoro ids from
//! on-disk ONNX/MLX model files (or static fallback); system via `say -v ?`.
//! Pure filter/label except disk/`say`.

use ds_config::{TtsEngine, TtsModel};

use crate::{Gender, Quality, SpeakerVoice, say, voices};

/// Pickable voice: opaque engine `id` plus human `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceChoice {
    pub id: String,
    pub label: String,
}

/// Fallback when `voices-v1.0.bin` is absent so the list stays non-empty.
pub const KOKORO_FALLBACK_IDS: &[&str] = &[
    "af_sarah",
    "af_heart",
    "am_michael",
    "am_adam",
    "bf_emma",
    "bm_george",
];

/// Filename only; pin is `ds_model::KOKORO_VOICES_FILE` (#5: no ds-model dep).
/// Drift: `kokoro_voices_filename_matches_ds_model_registry`.
const KOKORO_VOICES_FILE: &str = "voices-v1.0.bin";
/// Dir name; drift-guarded vs ds-model without a runtime dep.
const KOKORO_MLX_DIR_NAME: &str = "kokoro-82m";

// ── Kokoro id parsing ────────────────────────────────────────────────────────

/// Language from the leading family char (`af_sarah` → "en"), including unrouted
/// `j`/`z` (router locks them via [`is_routable_kokoro_voice`]). Unknown → "other".
pub fn kokoro_language(id: &str) -> &'static str {
    match id.as_bytes().first() {
        Some(b'a') | Some(b'b') => "en", // American + British
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

/// BCP-47 tag; English families keep region (`a` → en-US, `b` → en-GB).
pub fn kokoro_language_tag(id: &str) -> String {
    match id.as_bytes().first() {
        Some(b'a') => "en-US".to_string(),
        Some(b'b') => "en-GB".to_string(),
        _ => kokoro_language(id).to_string(),
    }
}

/// Gender from second char (`af_…`/`am_…`); unknown → `None`.
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

/// English-family accent only (`a` American, `b` British).
fn kokoro_accent(id: &str) -> Option<&'static str> {
    match id.as_bytes().first() {
        Some(b'a') => Some("American"),
        Some(b'b') => Some("British"),
        _ => None,
    }
}

/// `af_sarah` → "Sarah".
pub fn kokoro_display_name(id: &str) -> String {
    let raw = id.split_once('_').map(|(_, n)| n).unwrap_or(id);
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => id.to_string(),
    }
}

/// e.g. "Sarah (American, Female)".
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

/// `"female"` / `"male"`, or `None`.
pub fn gender_str(g: Option<Gender>) -> Option<&'static str> {
    match g {
        Some(Gender::Female) => Some("female"),
        Some(Gender::Male) => Some("male"),
        None => None,
    }
}

// ── Engine voice id sources (disk / shell) ───────────────────────────────────

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

/// On-disk Kokoro ids (ONNX voices bin, else MLX voices dir). `None` on fresh install
/// so validation uses shape rules, not the static fallback. Disk probe only.
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

/// [`kokoro_disk_voice_ids`] or [`KOKORO_FALLBACK_IDS`].
pub fn kokoro_voice_ids() -> Vec<String> {
    kokoro_disk_voice_ids()
        .unwrap_or_else(|| KOKORO_FALLBACK_IDS.iter().map(|s| s.to_string()).collect())
}

/// System voices via `say -v ?` (macOS); empty off-host.
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

// ── Language-filtered choice lists ───────────────────────────────────────────

/// BCP-47 primary subtag (`en-US` → "en"), lower-cased.
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

/// Filter+label+sort of Kokoro `ids` (disk-free for tests).
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

/// Filter+label+sort of System `voices` (`say`-free for tests).
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

fn system_label(v: &SpeakerVoice) -> String {
    match v.quality {
        Some(Quality::Enhanced) if !v.name.contains("Enhanced") => format!("{} (Enhanced)", v.name),
        Some(Quality::Premium) if !v.name.contains("Premium") => format!("{} (Premium)", v.name),
        _ => v.name.clone(),
    }
}

/// Built-in voices for a model and language.
pub fn built_in_choices(model: TtsModel, language: &str) -> Vec<VoiceChoice> {
    let kokoro_ids = (model == TtsModel::Kokoro)
        .then(kokoro_voice_ids)
        .unwrap_or_default();
    built_in_choices_from(model, language, &kokoro_ids)
}

/// Built-in voices with injected Kokoro ids; other models use static registries.
pub fn built_in_choices_from(
    model: TtsModel,
    language: &str,
    kokoro_ids: &[String],
) -> Vec<VoiceChoice> {
    if model == TtsModel::Kokoro {
        return kokoro_choices_from(kokoro_ids, &primary_subtag(language));
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

/// Voice-pool catalog. `System` holds enumerated voices so language rules stay pure.
#[derive(Debug, Clone, Copy)]
pub enum VoiceCatalog<'a> {
    BuiltIn(TtsModel),
    /// Built-in over caller-supplied Kokoro ids; other models keep their registry.
    BuiltInFrom(TtsModel, &'a [String]),
    System(&'a [SpeakerVoice]),
}

impl VoiceCatalog<'_> {
    /// Language `voice` is locked to, or `None` if it follows the synthesis language.
    /// Kokoro: family char; System: locale tag; unlocked models: always `None`.
    /// Unresolved System names stay unlocked; unrouted Kokoro families report their
    /// language so [`Self::pool_for_language`] drops them for routed languages.
    pub fn voice_language(&self, voice: &str) -> Option<String> {
        match self {
            Self::BuiltIn(TtsModel::Kokoro) | Self::BuiltInFrom(TtsModel::Kokoro, _) => {
                match kokoro_language(voice) {
                    "other" => None,
                    language => Some(language.to_string()),
                }
            }
            Self::BuiltIn(_) | Self::BuiltInFrom(_, _) => None,
            Self::System(voices) => voices
                .iter()
                .find(|candidate| candidate.id == voice)
                .map(|candidate| primary_subtag(&candidate.language_tag)),
        }
    }

    /// Pool entries that can speak `language`. Unlocked voices always qualify.
    pub fn pool_for_language(&self, pool: &[String], language: &str) -> Vec<String> {
        let want = primary_subtag(language);
        pool.iter()
            .filter(|voice| self.voice_language(voice).is_none_or(|owned| owned == want))
            .cloned()
            .collect()
    }

    /// Shipped voices for `language` — same listing as picker / `voices` tool.
    pub fn voices_for_language(&self, language: &str) -> Vec<String> {
        match self {
            Self::BuiltIn(model) => built_in_choices(*model, language)
                .into_iter()
                .map(|choice| choice.id)
                .collect(),
            Self::BuiltInFrom(model, kokoro_ids) => {
                built_in_choices_from(*model, language, kokoro_ids)
                    .into_iter()
                    .map(|choice| choice.id)
                    .collect()
            }
            Self::System(voices) => system_choices_from(voices, &primary_subtag(language))
                .into_iter()
                .map(|choice| choice.id)
                .collect(),
        }
    }
}

/// Full model-catalog membership (not the configured pool). Kokoro uses disk ids and
/// accepts anything while the pack is missing (same as `set_config`); other models use
/// registry `voices`. Rejects stale per-utterance ids after a model switch.
pub fn is_model_voice(model: TtsModel, voice: &str) -> bool {
    if model == TtsModel::Kokoro {
        return kokoro_disk_voice_ids().is_none_or(|ids| ids.iter().any(|id| id == voice));
    }
    model.descriptor().voices.contains(&voice)
}

/// Router pool for `model` plus configured entries this build cannot route.
/// Unrouted languages drop out; empty non-empty config substitutes model defaults
/// (`VoiceConfig::clamp`). Unknown shapes stay (weaker evidence than a known lock).
/// Single source for router / `status` / `voices`.
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
    /// Router pick list.
    pub voices: Vec<String>,
    /// Configured but unroutable here.
    pub ignored: Vec<String>,
}

/// Language this build cannot route for `id` (`ja`/`zh`), or `None` when routable.
/// Unknown family → `None` (unlocked), matching [`effective_builtin_pool`]'s keep rule.
fn kokoro_unrouted_language(id: &str) -> Option<&'static str> {
    match kokoro_language(id) {
        "other" => None,
        language if !TtsModel::Kokoro.descriptor().supports_language(language) => Some(language),
        _ => None,
    }
}

/// Whether this build ships a frontend for `id` (dropped `ja`/`zh` pipelines still
/// publish real ids — language rules alone gate them in pool and `speak`).
pub fn is_routable_kokoro_voice(id: &str) -> bool {
    kokoro_unrouted_language(id).is_none()
}

/// Refusal sentence for new Kokoro input (`set_config` + `speak` share it).
/// `None` iff [`is_routable_kokoro_voice`].
pub fn kokoro_route_refusal(id: &str) -> Option<String> {
    kokoro_unrouted_language(id).map(|language| {
        format!("`{id}` speaks {language}, a language this build cannot route; see voices")
    })
}

/// Reject unroutable per-utterance Kokoro voice. System is freeform; fixed models
/// are registry-gated in `TtsArgPools::parse` and re-clamped via [`supported_voice`].
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

/// Clamp a per-utterance voice to `model` (voice twin of `ds_tts::supported_language`).
/// Model can change between queue and synth; foreign voices become the first default
/// rather than drop (Chatterbox/Qwen/OmniVoice have no per-voice fallback).
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

// ── Display name resolver ────────────────────────────────────────────────────

/// Strip vendor/quality noise: `"Microsoft Hazel Desktop"` → `"Hazel"`.
pub fn friendly_system_name(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_prefix("Microsoft ").unwrap_or(s);
    let s = s.strip_suffix(" Desktop").unwrap_or(s);
    let s = s.split(" (").next().unwrap_or(s);
    s.trim().to_string()
}

/// Short speakable name for greeting/UI (Kokoro display, registry label, or tidied System).
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
        // Drift guard for local filename duplicates (no ds-model runtime dep).
        assert_eq!(KOKORO_VOICES_FILE, ds_model::KOKORO_VOICES_FILE);
        assert_eq!(KOKORO_MLX_DIR_NAME, ds_model::mlx_repo::KOKORO_MLX_DIR_NAME);
    }

    #[test]
    fn is_model_voice_rejects_ids_from_a_different_model() {
        // Stale per-utterance id after model switch. Non-Kokoro = pure registry.
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
        // Owned voice passes; foreign → first default. Non-Kokoro = pure registry.
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
        // Unrouted families still report real language for the router lock.
        assert_eq!(kokoro_language("jf_alpha"), "ja");
        assert_eq!(kokoro_language("zf_xiaobei"), "zh");
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
        assert_eq!(kokoro_label("ef_dora"), "Dora (Female)");
    }

    #[test]
    fn kokoro_choices_filter_by_language_and_sort() {
        let ids: Vec<String> = ["am_michael", "af_sarah", "df_anna", "bm_george"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let en = kokoro_choices_from(&ids, "en");
        assert!(en.iter().all(|c| kokoro_language(&c.id) == "en"));
        assert!(en.iter().any(|c| c.id == "af_sarah"));
        assert!(en.iter().all(|c| c.id != "df_anna"));
        let labels: Vec<&str> = en.iter().map(|c| c.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);

        // Unknown `d` family is never selected by language.
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
        // #222: pool of only unrouted voices is empty.
        assert!(
            kokoro
                .pool_for_language(&["jf_alpha".into()], "en")
                .is_empty()
        );
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
        // Unknown shapes stay routable so pool and `speak` agree.
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

        // All locked → model defaults (`VoiceConfig::clamp` same rule).
        let locked = effective_builtin_pool(TtsModel::Kokoro, &["jf_alpha".into()]);
        assert_eq!(locked.voices, TtsModel::Kokoro.descriptor().default_voices);
        assert_eq!(locked.ignored, ["jf_alpha"]);

        // Unknown shape kept (#222 MLX pack ids).
        let unknown_shape = effective_builtin_pool(TtsModel::Kokoro, &["custom_v1".into()]);
        assert_eq!(unknown_shape.voices, ["custom_v1"]);
        assert!(unknown_shape.ignored.is_empty());

        let unlocked = effective_builtin_pool(TtsModel::Chatterbox, &["default".into()]);
        assert_eq!(unlocked.voices, ["default"]);
        assert!(unlocked.ignored.is_empty());

        // Empty config stays empty (user intent, not emptied-by-filter).
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
        // Unknown family: keep (#222); refuse would state a false reason.
        for id in ["df_anna", "custom_v1", "xq_bogus"] {
            assert_eq!(kokoro_route_refusal(id), None, "{id}");
        }
        assert_eq!(kokoro_route_refusal("af_sarah"), None);
        assert_eq!(kokoro_route_refusal("if_sara"), None);
    }

    /// Predicate and sentence builder share one reason — drift guard.
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
        // Unknown family accepted (#222); synth clamps missing-disk ids later.
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
        assert_eq!(en.len(), 2); // en-US + en-GB
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
        assert_eq!(kokoro.voice_language("weird"), None);

        let system = system_fixture();
        let system = VoiceCatalog::System(&system);
        assert_eq!(system.voice_language("Alice").as_deref(), Some("it"));
        // Freeform System names stay unlocked when absent from catalog.
        assert_eq!(system.voice_language("Unknown Voice"), None);
    }

    #[test]
    fn unlocked_catalogs_never_narrow_the_pool() {
        // Chatterbox/Qwen/OmniVoice condition on language arg — filter is a no-op.
        let pool = vec!["sohee".to_string(), "ryan".to_string()];
        for model in [TtsModel::Chatterbox, TtsModel::Qwen, TtsModel::OmniVoice] {
            let catalog = VoiceCatalog::BuiltIn(model);
            assert_eq!(catalog.voice_language("sohee"), None);
            assert_eq!(catalog.pool_for_language(&pool, "it"), pool);
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
        // Regional tags → primary; en-US/en-GB are one language.
        assert_eq!(
            catalog.pool_for_language(&pool, "en-GB"),
            vec!["af_sarah", "bf_emma"]
        );
        assert!(catalog.pool_for_language(&pool, "es").is_empty());
    }

    #[test]
    fn injected_kokoro_catalog_supplies_language_voices() {
        let ids = vec![
            "af_sarah".to_string(),
            "ef_dora".to_string(),
            "if_sara".to_string(),
        ];
        let catalog = VoiceCatalog::BuiltInFrom(TtsModel::Kokoro, &ids);
        assert_eq!(catalog.voices_for_language("it"), ["if_sara"]);
        assert_eq!(catalog.voices_for_language("es"), ["ef_dora"]);
        assert!(catalog.voices_for_language("fr").is_empty());
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
