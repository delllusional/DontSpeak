//! Voice / language enumeration (Kokoro voices bin + `say` directly; no engine). Used by
//! `voices` (MCP).

use ds_config::TtsEngine;
use ds_voices::enumerate;
use serde_json::{Value, json};

/// Voice groups for `engine`, filtered to one `language` primary subtag. Empty groups dropped.
/// Voices carry no own `language` field — the group's subtag is the language. Build is
/// English-only; sole caller passes `"en"`.
pub(crate) fn voice_groups(engine: TtsEngine, language: &str) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    match engine {
        TtsEngine::BuiltIn => {
            let ids = enumerate::kokoro_voice_ids();
            let voices: Vec<Value> = enumerate::kokoro_choices_from(&ids, language)
                .into_iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "language_tag": enumerate::kokoro_language_tag(&c.id),
                        "gender": enumerate::gender_str(enumerate::kokoro_gender(&c.id)),
                        "engine": "built_in",
                    })
                })
                .collect();
            if !voices.is_empty() {
                groups.push((language.to_string(), voices));
            }
        }
        TtsEngine::System => {
            let sys = enumerate::system_voices();
            let voices: Vec<Value> = enumerate::system_choices_from(&sys, language)
                .into_iter()
                .map(|c| {
                    let voice = sys.iter().find(|v| v.id == c.id);
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
                groups.push((language.to_string(), voices));
            }
        }
    }
    groups
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
