//! Native Japanese frontend using the OpenJTalk-compatible jpreprocess pipeline.

use std::sync::OnceLock;

use jpreprocess::{DefaultTokenizer, JPreprocess, SystemDictionaryConfig};

type Frontend = JPreprocess<DefaultTokenizer>;

fn frontend() -> Result<&'static Frontend, String> {
    static FRONTEND: OnceLock<Result<Frontend, String>> = OnceLock::new();
    FRONTEND
        .get_or_init(|| {
            let path = ds_model::japanese_dictionary_dir()
                .filter(|path| path.is_dir())
                .ok_or_else(|| "Kokoro Japanese dictionary is not installed".to_string())?;
            let dictionary = SystemDictionaryConfig::File(path)
                .load()
                .map_err(|error| format!("load Kokoro Japanese dictionary: {error}"))?;
            Ok(JPreprocess::with_dictionaries(dictionary, None))
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn punctuation(surface: &str) -> Option<&'static str> {
    match surface {
        "、" | "，" => Some(","),
        "。" | "．" => Some("."),
        "！" => Some("!"),
        "？" => Some("?"),
        "：" => Some(":"),
        "；" => Some(";"),
        "「" | "『" | "〈" | "《" | "〖" | "【" | "«" => Some("“"),
        "」" | "』" | "〉" | "》" | "〗" | "】" | "»" => Some("”"),
        "（" => Some("("),
        "）" => Some(")"),
        "・" => Some(" "),
        "〜" | "～" => Some("—"),
        _ => None,
    }
}

fn consonant_ipa(consonant: &str, vowel: &str) -> &'static str {
    match (consonant, vowel) {
        ("", _) => "",
        ("v", "i") => "vʲ",
        ("v", _) => "v",
        ("w", _) => "β",
        ("r", "i") => "ɾʲ",
        ("r", _) => "ɾ",
        ("ry", _) => "ɾʲ",
        ("y", _) => "j",
        ("m", "i") => "mʲ",
        ("m", _) => "m",
        ("my", _) => "mʲ",
        ("p", "i") => "pʲ",
        ("p", _) => "p",
        ("b", "i") => "bʲ",
        ("b", _) => "b",
        ("h", "i") | ("hy", _) => "ç",
        ("h", _) => "h",
        ("f", "i") => "ɸʲ",
        ("f", _) => "ɸ",
        ("py", _) => "pʲ",
        ("by", _) => "bʲ",
        ("n", "i") | ("ny", _) => "ɲ",
        ("n", _) => "n",
        ("d", "i") => "dʲ",
        ("d", _) => "d",
        ("t", "i") => "tʲ",
        ("t", _) => "t",
        ("dy", _) => "dʲ",
        ("ty", _) => "tʲ",
        ("ts", _) => "ʦ",
        ("ch", _) => "ʨ",
        ("z", "a" | "e" | "o") => "ʣ",
        ("z", _) => "z",
        ("s", _) => "s",
        ("j", _) => "ʥ",
        ("sh", _) => "ɕ",
        ("g", "i") => "ɡʲ",
        ("g", _) => "ɡ",
        ("k", "i") => "kʲ",
        ("k", _) => "k",
        ("gy", _) => "ɡʲ",
        ("ky", _) => "kʲ",
        ("gw", _) => "ɡᵝ",
        ("kw", _) => "kᵝ",
        ("cl", _) => "ʔ",
        ("-", _) => "ː",
        _ => "",
    }
}

fn vowel_ipa(consonant: &str, vowel: &str) -> &'static str {
    match vowel.to_ascii_lowercase().as_str() {
        "a" => "a",
        "i" => "i",
        "u" if matches!(consonant, "s" | "z" | "ts" | "ch" | "j" | "sh")
            || consonant.ends_with('y') =>
        {
            "ɨ"
        }
        "u" => "ɯ",
        "e" => "e",
        "o" => "o",
        _ => "",
    }
}

fn nasal_ipa(next_consonant: Option<&str>) -> &'static str {
    match next_consonant {
        Some("m" | "p" | "b" | "my" | "py" | "by") => "m",
        Some("k" | "g" | "ky" | "gy" | "kw" | "gw") => "ŋ",
        Some("ny" | "ch" | "j") => "ɲ",
        Some("n" | "t" | "d" | "r" | "z" | "ry" | "dy" | "ty") => "n",
        _ => "ɴ",
    }
}

fn mora_ipa(consonant: Option<&str>, vowel: Option<&str>, next: Option<&str>) -> String {
    let consonant = consonant.unwrap_or("");
    if consonant == "N" {
        return nasal_ipa(next).to_string();
    }
    let vowel = vowel.unwrap_or("");
    format!(
        "{}{}",
        consonant_ipa(consonant, &vowel.to_ascii_lowercase()),
        vowel_ipa(consonant, vowel)
    )
}

pub(super) fn phonemize(text: &str) -> Result<String, String> {
    let mut njd = frontend()?
        .text_to_njd(text)
        .map_err(|error| format!("Japanese G2P tokenize: {error}"))?;
    njd.preprocess();

    let mut result = String::new();
    for (node_index, node) in njd.nodes.iter().enumerate() {
        if let Some(mark) = punctuation(node.get_string()) {
            while result.ends_with(char::is_whitespace) {
                result.pop();
            }
            result.push_str(mark);
            if !matches!(mark, "(" | "“") {
                result.push(' ');
            }
            continue;
        }

        let moras = node.get_pron().moras();
        let mut phonemes = String::new();
        for (mora_index, mora) in moras.iter().enumerate() {
            let (consonant, vowel) = mora.phonemes();
            let consonant = consonant.map(|value| value.to_string());
            let vowel = vowel.map(|value| value.to_string());
            let next_in_node = moras.get(mora_index + 1).and_then(|next| {
                let (consonant, _) = next.phonemes();
                consonant.map(|value| value.to_string())
            });
            let next_in_utterance = njd.nodes.get(node_index + 1).and_then(|next| {
                next.get_pron().moras().first().and_then(|mora| {
                    let (consonant, _) = mora.phonemes();
                    consonant.map(|value| value.to_string())
                })
            });
            let next = next_in_node.as_deref().or(next_in_utterance.as_deref());
            phonemes.push_str(&mora_ipa(consonant.as_deref(), vowel.as_deref(), next));
        }
        if phonemes.is_empty() {
            continue;
        }
        let chained = node.get_chain_flag().unwrap_or(false);
        if !result.is_empty()
            && !result.ends_with(char::is_whitespace)
            && !result.ends_with(['(', '“'])
            && !chained
        {
            result.push(' ');
        }
        result.push_str(&phonemes);
    }
    Ok(result.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_mora_mapping_uses_kokoro_symbols() {
        assert_eq!(mora_ipa(Some("k"), Some("i"), None), "kʲi");
        assert_eq!(mora_ipa(Some("sh"), Some("u"), None), "ɕɨ");
        assert_eq!(mora_ipa(Some("w"), Some("i"), None), "βi");
        assert_eq!(mora_ipa(Some("f"), Some("i"), None), "ɸʲi");
        assert_eq!(mora_ipa(Some("t"), Some("i"), None), "tʲi");
        assert_eq!(mora_ipa(Some("N"), None, Some("k")), "ŋ");
        assert_eq!(mora_ipa(Some("cl"), None, Some("t")), "ʔ");
        assert_eq!(mora_ipa(Some("-"), None, None), "ː");
    }

    #[test]
    fn japanese_punctuation_maps_to_kokoro_tokens() {
        for (source, expected) in [("。", "."), ("、", ","), ("「", "“"), ("」", "”")] {
            assert_eq!(punctuation(source), Some(expected));
            assert!(
                expected
                    .chars()
                    .all(|c| crate::vocab::vocab_id(c).is_some())
            );
        }
    }
}
