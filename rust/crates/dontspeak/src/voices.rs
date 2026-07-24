//! Voice and language enumeration used by the `voices` MCP tool.

use ds_config::{TtsEngine, TtsModel};
use ds_voices::enumerate;
use serde_json::{Value, json};

/// Voice groups for `engine` and an OPTIONAL language filter (`None` = System only:
/// every subtag present). Empty groups are dropped; voices carry no own language field
/// because the group identifies it.
pub(crate) fn voice_groups(
    engine: TtsEngine,
    model: TtsModel,
    language: Option<&str>,
) -> Vec<(String, Vec<Value>)> {
    // Enumerate the Kokoro catalog only for a query that is actually for it — every other
    // combination is registry- or `say`-backed and must not pay a disk read.
    let kokoro_ids = (engine == TtsEngine::BuiltIn && model == TtsModel::Kokoro)
        .then(enumerate::kokoro_voice_ids)
        .unwrap_or_default();
    let system_voices = (engine == TtsEngine::System)
        .then(enumerate::system_voices)
        .unwrap_or_default();
    voice_groups_from(engine, model, language, &kokoro_ids, &system_voices)
}

/// [`voice_groups`] over injected catalogs, so tests need neither the model cache nor `say`.
pub(crate) fn voice_groups_from(
    engine: TtsEngine,
    model: TtsModel,
    language: Option<&str>,
    kokoro_ids: &[String],
    system_voices: &[ds_voices::SpeakerVoice],
) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    match engine {
        TtsEngine::BuiltIn => {
            let language = language.unwrap_or(model.descriptor().default_language);
            let choices = enumerate::built_in_choices_from(model, language, kokoro_ids);
            let voices: Vec<Value> = choices
                .into_iter()
                .map(|c| {
                    let (language_tag, gender) = if model == TtsModel::Kokoro {
                        (
                            Value::String(enumerate::kokoro_language_tag(&c.id)),
                            serde_json::to_value(enumerate::gender_str(enumerate::kokoro_gender(
                                &c.id,
                            )))
                            .unwrap_or(Value::Null),
                        )
                    } else {
                        (Value::String(language.to_string()), Value::Null)
                    };
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "language_tag": language_tag,
                        "gender": gender,
                        "engine": "built_in",
                        "model": model.as_str(),
                    })
                })
                .collect();
            if !voices.is_empty() {
                groups.push((language.to_string(), voices));
            }
        }
        TtsEngine::System => {
            for subtag in system_group_subtags(system_voices, language) {
                let voices: Vec<Value> = enumerate::system_choices_from(system_voices, &subtag)
                    .into_iter()
                    .map(|c| {
                        let voice = system_voices.iter().find(|v| v.id == c.id);
                        let gender = voice.and_then(|v| enumerate::gender_str(v.gender));
                        let language_tag = voice.map(|v| v.language_tag.clone());
                        json!({
                            "id": c.id,
                            "label": c.label,
                            "language_tag": language_tag,
                            "gender": gender,
                            "engine": "system",
                        })
                    })
                    .collect();
                if !voices.is_empty() {
                    groups.push((subtag, voices));
                }
            }
        }
    }
    groups
}

/// Subtag groups the System engine renders: an explicit filter yields that single group;
/// `None` yields every distinct subtag present. The catalog never inherits a built-in
/// model default here — OmniVoice's "auto" would filter every system voice out.
fn system_group_subtags(voices: &[ds_voices::SpeakerVoice], language: Option<&str>) -> Vec<String> {
    match language {
        Some(want) => vec![want.to_string()],
        None => {
            let mut tags: Vec<String> = voices
                .iter()
                .map(|v| enumerate::primary_subtag(&v.language_tag))
                .collect();
            tags.sort_unstable();
            tags.dedup();
            tags
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_language_matches_nothing_group_is_dropped() {
        // Pure filter: synthetic ids + unmatched language → empty (no disk).
        let ids: Vec<String> = ["af_sarah", "am_michael", "bm_george"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(enumerate::kokoro_choices_from(&ids, "xx").is_empty());
    }

    #[test]
    fn system_without_a_language_filter_lists_every_subtag() {
        // Injected fixture — never call real `system_voices()` (shells out to `say -v ?`).
        let mk = |id: &str, tag: &str| ds_voices::SpeakerVoice {
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
        // No filter: every distinct primary subtag, deduped + sorted.
        assert_eq!(system_group_subtags(&voices, None), ["de", "en"]);
        // Explicit filter: exactly that one group.
        assert_eq!(system_group_subtags(&voices, Some("en")), ["en"]);
        assert!(system_group_subtags(&[], None).is_empty());
    }

    #[test]
    fn system_language_matches_nothing_group_is_dropped() {
        // Injected fixture — never call real `system_voices()` (shells out to `say -v ?`).
        let voices = vec![ds_voices::SpeakerVoice {
            id: "Samantha".into(),
            name: "Samantha".into(),
            language_tag: "en-US".into(),
            downloadable: false,
            gender: None,
            quality: None,
        }];
        assert!(enumerate::system_choices_from(&voices, "xx").is_empty());
    }
}
