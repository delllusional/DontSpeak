//! Voice / language enumeration (reads the Kokoro voices bin + `say` directly; no
//! engine and no config write). Used by the `list_voices` and `set_voice` tools.

use ds_config::TtsEngine;
use ds_tts::enumerate;
use serde_json::{Value, json};

/// Distinct strings in first-seen order, then sorted — the set of language subtags
/// present in an engine's voices when listing across all languages.
fn distinct_sorted(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for it in items {
        if !out.contains(&it) {
            out.push(it);
        }
    }
    out.sort();
    out
}

/// Build voice groups for `engine`, grouped by language subtag and sorted, filtered
/// to one `language` primary subtag — or, when `language` is `None`, every language
/// the engine offers. Each group is `(subtag, voices)`; empty groups are dropped, so
/// an explicit filter that matches nothing yields no group. The voices carry no
/// `language` field of their own — the group's subtag is the language.
pub(crate) fn voice_groups(engine: TtsEngine, language: Option<&str>) -> Vec<(String, Vec<Value>)> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    match engine {
        // Off has no voices to list.
        TtsEngine::Off => {}
        TtsEngine::Kokoro => {
            let ids = enumerate::kokoro_voice_ids();
            let langs = match language {
                Some(l) => vec![l.to_string()],
                None => distinct_sorted(
                    ids.iter()
                        .map(|id| enumerate::kokoro_language(id).to_string()),
                ),
            };
            for lang in langs {
                let voices: Vec<Value> = enumerate::kokoro_choices_from(&ids, &lang)
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
                    groups.push((lang, voices));
                }
            }
        }
        TtsEngine::System => {
            let sys = enumerate::system_voices();
            let langs = match language {
                Some(l) => vec![l.to_string()],
                None => distinct_sorted(
                    sys.iter()
                        .map(|v| enumerate::primary_subtag(&v.language_tag)),
                ),
            };
            for lang in langs {
                let voices: Vec<Value> = enumerate::system_choices_from(&sys, &lang)
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
                    groups.push((lang, voices));
                }
            }
        }
    }
    groups
}

/// Resolve `(engine, label)` for a voice id/name: honor an explicit `tts_engine`
/// hint, else infer from whichever engine actually has the voice. Errors with
/// guidance when the voice isn't found in the chosen/any engine.
pub(crate) fn resolve_voice_engine(
    voice: &str,
    explicit: Option<&str>,
) -> Result<(TtsEngine, String), String> {
    let kokoro_hit = enumerate::kokoro_voice_ids().iter().any(|id| id == voice);
    let sys = enumerate::system_voices();
    let sys_hit = sys
        .iter()
        .find(|v| v.id == voice || v.name == voice)
        .cloned();

    match explicit {
        Some(e) if e.eq_ignore_ascii_case("kokoro") => kokoro_hit
            .then(|| (TtsEngine::Kokoro, enumerate::kokoro_display_name(voice)))
            .ok_or_else(|| format!("`{voice}` is not a known Kokoro voice — see list_voices with tts_engine=built_in.")),
        Some(e) if e.eq_ignore_ascii_case("system") => sys_hit
            .map(|v| (TtsEngine::System, v.name))
            .ok_or_else(|| format!("`{voice}` is not an available System voice — see list_voices with tts_engine=system.")),
        _ => {
            if kokoro_hit {
                Ok((TtsEngine::Kokoro, enumerate::kokoro_display_name(voice)))
            } else if let Some(v) = sys_hit {
                Ok((TtsEngine::System, v.name))
            } else {
                Err(format!("`{voice}` is not a known Kokoro or System voice — see list_voices."))
            }
        }
    }
}
