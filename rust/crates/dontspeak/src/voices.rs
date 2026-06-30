//! Voice / language enumeration (reads the Kokoro voices bin + `say` directly; no
//! engine and no config write). Used by the `list_voices` tool.

use ds_config::TtsEngine;
use ds_tts::enumerate;
use serde_json::{Value, json};

/// Build voice groups for `engine`, filtered to one `language` primary subtag. Each
/// group is `(subtag, voices)`; an empty group (no voice matches) is dropped, so a
/// language the engine doesn't offer yields no group. The voices carry no `language`
/// field of their own — the group's subtag is the language. This build is English-only,
/// so the sole caller passes `"en"`.
pub(crate) fn voice_groups(engine: TtsEngine, language: &str) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    match engine {
        // Off has no voices to list.
        TtsEngine::Off => {}
        TtsEngine::Kokoro => {
            let ids = enumerate::kokoro_voice_ids();
            let voices: Vec<Value> = enumerate::kokoro_choices_from(&ids, language)
                .into_iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "label": c.label,
                        "language_tag": enumerate::kokoro_language_tag(&c.id),
                        "gender": enumerate::gender_str(enumerate::kokoro_gender(&c.id)),
                        "engine": "kokoro",
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
