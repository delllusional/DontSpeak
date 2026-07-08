//! Voice / language enumeration (reads the Kokoro voices bin + `say` directly; no
//! engine and no config write). Used by the `list_voices` tool.

use ds_config::TtsEngine;
use ds_voices::enumerate;
use serde_json::{Value, json};

/// Build voice groups for `engine`, filtered to one `language` primary subtag. Each
/// group is `(subtag, voices)`; an empty group (no voice matches) is dropped, so a
/// language the engine doesn't offer yields no group. The voices carry no `language`
/// field of their own — the group's subtag is the language. This build is English-only,
/// so the sole caller passes `"en"`.
pub(crate) fn voice_groups(engine: TtsEngine, language: &str) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    match engine {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_language_matches_nothing_group_is_dropped() {
        // Pins the "language matches nothing → group dropped" composition that
        // `voice_groups`'s Kokoro arm relies on, driving the underlying PURE
        // filter (`kokoro_choices_from`) directly with a synthetic id list and an
        // unmatched language tag — fully pure/injected, no disk read.
        let ids: Vec<String> = ["af_sarah", "am_michael", "bm_george"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(enumerate::kokoro_choices_from(&ids, "xx").is_empty());
    }

    #[test]
    fn system_language_matches_nothing_group_is_dropped() {
        // Same "language matches nothing → group dropped" branch for the System
        // engine. Drives the PURE `system_choices_from` seam with an injected
        // `SpeakerVoice` fixture rather than `voice_groups(TtsEngine::System, ..)`
        // directly — the latter would call the real, unmocked
        // `enumerate::system_voices()`, which shells out to `say -v ?` on macOS.
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
