//! Native Mandarin frontend matching Misaki's legacy pinyin-to-IPA path.

use std::sync::OnceLock;

use chinese_number::{ChineseCase, ChineseCountMethod, ChineseVariant, NumberToChinese};
use jieba_rs::Jieba;
use pinyin::ToPinyin;

fn split_tone(pinyin: &str) -> (&str, u8) {
    if let Some(tone) = pinyin
        .chars()
        .last()
        .and_then(|character| character.to_digit(10))
    {
        (&pinyin[..pinyin.len() - 1], tone as u8)
    } else {
        (pinyin, 5)
    }
}

fn restore_final(pinyin: &str) -> String {
    let pinyin = match pinyin.strip_prefix('y') {
        Some(rest) if rest.starts_with('u') => format!("ü{}", &rest[1..]),
        Some(rest) if rest.starts_with('i') => rest.to_string(),
        Some(rest) => format!("i{rest}"),
        None => match pinyin.strip_prefix('w') {
            Some(rest) if rest.starts_with('u') => rest.to_string(),
            Some(rest) => format!("u{rest}"),
            None => pinyin.to_string(),
        },
    };
    let (initial, final_part) = split_initial(&pinyin);
    let initial = initial.to_string();
    let mut final_part = final_part.to_string();
    if matches!(initial.as_str(), "j" | "q" | "x") && final_part.starts_with('u') {
        final_part.replace_range(..1, "ü");
    }
    final_part = match final_part.as_str() {
        "iu" => "iou".to_string(),
        "ui" => "uei".to_string(),
        "un" => "uen".to_string(),
        _ => final_part,
    };
    format!("{initial}{final_part}")
}

fn split_initial(pinyin: &str) -> (&str, &str) {
    for initial in [
        "zh", "ch", "sh", "b", "c", "d", "f", "g", "h", "j", "k", "l", "m", "n", "p", "q", "r",
        "s", "t", "x", "z",
    ] {
        if let Some(final_part) = pinyin.strip_prefix(initial) {
            return (initial, final_part);
        }
    }
    ("", pinyin)
}

fn initial_ipa(initial: &str) -> Option<&'static str> {
    match initial {
        "" => Some(""),
        "b" => Some("p"),
        "c" => Some("ʦʰ"),
        "ch" => Some("ꭧʰ"),
        "d" => Some("t"),
        "f" => Some("f"),
        "g" => Some("k"),
        "h" => Some("x"),
        "j" => Some("ʨ"),
        "k" => Some("kʰ"),
        "l" => Some("l"),
        "m" => Some("m"),
        "n" => Some("n"),
        "p" => Some("pʰ"),
        "q" => Some("ʨʰ"),
        "r" => Some("ɻ"),
        "s" => Some("s"),
        "sh" => Some("ʂ"),
        "t" => Some("tʰ"),
        "x" => Some("ɕ"),
        "z" => Some("ʦ"),
        "zh" => Some("ꭧ"),
        _ => None,
    }
}

fn final_ipa(initial: &str, final_part: &str) -> Option<&'static str> {
    if final_part == "i" && matches!(initial, "zh" | "ch" | "sh" | "r" | "z" | "c" | "s") {
        return Some("ɨ0");
    }
    match final_part {
        "a" => Some("a0"),
        "ai" => Some("ai0"),
        "an" => Some("a0n"),
        "ang" => Some("a0ŋ"),
        "ao" => Some("au0"),
        "e" => Some("ɤ0"),
        "ei" => Some("ei0"),
        "en" => Some("ə0n"),
        "eng" => Some("ə0ŋ"),
        "i" => Some("i0"),
        "ia" => Some("ja0"),
        "ian" => Some("jɛ0n"),
        "iang" => Some("ja0ŋ"),
        "iao" => Some("jau0"),
        "ie" => Some("je0"),
        "in" => Some("i0n"),
        "iou" => Some("jou0"),
        "ing" => Some("i0ŋ"),
        "iong" => Some("jʊ0ŋ"),
        "ong" => Some("ʊ0ŋ"),
        "ou" => Some("ou0"),
        "u" => Some("u0"),
        "uei" => Some("wei0"),
        "ua" => Some("wa0"),
        "uai" => Some("wai0"),
        "uan" => Some("wa0n"),
        "uen" => Some("wə0n"),
        "uang" => Some("wa0ŋ"),
        "ueng" => Some("wə0ŋ"),
        "uo" | "o" => Some("wo0"),
        "ü" => Some("y0"),
        "üe" => Some("ɥe0"),
        "üan" => Some("ɥɛ0n"),
        "ün" => Some("y0n"),
        "er" => Some("ɚ0"),
        "ê" => Some("ɛ0"),
        _ => None,
    }
}

fn tone_mark(tone: u8) -> &'static str {
    match tone {
        1 => "→",
        2 => "↗",
        3 => "↓",
        4 => "↘",
        _ => "",
    }
}

fn pinyin_to_ipa(pinyin: &str) -> Result<String, String> {
    let (pinyin, tone) = split_tone(pinyin);
    let pinyin = restore_final(pinyin);
    if let Some(consonant) = match pinyin.as_str() {
        "hm" => Some("hm"),
        "hng" => Some("hŋ"),
        "m" => Some("m"),
        "n" => Some("n"),
        "ng" => Some("ŋ"),
        _ => None,
    } {
        return Ok(format!("{consonant}{}", tone_mark(tone)));
    }
    if let Some(interjection) = match pinyin.as_str() {
        "io" => Some("jɔ"),
        "o" => Some("ɔ"),
        _ => None,
    } {
        return Ok(format!("{interjection}{}", tone_mark(tone)));
    }
    let (initial_code, final_part) = split_initial(&pinyin);
    let initial = initial_ipa(initial_code)
        .ok_or_else(|| format!("unsupported Mandarin initial in `{pinyin}`"))?;
    let final_part = final_ipa(initial_code, final_part)
        .ok_or_else(|| format!("unsupported Mandarin final in `{pinyin}`"))?;
    Ok(format!(
        "{initial}{}",
        final_part.replace('0', tone_mark(tone))
    ))
}

fn map_punctuation(character: char) -> Option<&'static str> {
    match character {
        '、' | '，' => Some(", "),
        '。' | '．' => Some(". "),
        '！' => Some("! "),
        '：' => Some(": "),
        '；' => Some("; "),
        '？' => Some("? "),
        '«' | '《' | '「' | '〖' | '【' => Some(" “"),
        '»' | '》' | '」' | '〗' | '】' => Some("” "),
        '（' => Some(" ("),
        '）' => Some(") "),
        _ => None,
    }
}

fn is_han(character: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&character)
}

fn expand_number(number: &str) -> String {
    if number.contains('.') {
        number.parse::<f64>().ok().and_then(|value| {
            value
                .to_chinese(
                    ChineseVariant::Traditional,
                    ChineseCase::Lower,
                    ChineseCountMethod::Low,
                )
                .ok()
        })
    } else {
        number.parse::<i64>().ok().and_then(|value| {
            value
                .to_chinese(
                    ChineseVariant::Traditional,
                    ChineseCase::Lower,
                    ChineseCountMethod::Low,
                )
                .ok()
        })
    }
    .unwrap_or_else(|| number.to_string())
}

fn expand_numbers(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut number = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() || (character == '.' && !number.is_empty()) {
            number.push(character);
        } else {
            if !number.is_empty() {
                output.push_str(&expand_number(&number));
                number.clear();
            }
            output.push(character);
        }
    }
    if !number.is_empty() {
        output.push_str(&expand_number(&number));
    }
    output
}

fn phonemize_han(segment: &str) -> Result<String, String> {
    static JIEBA: OnceLock<Jieba> = OnceLock::new();
    let jieba = JIEBA.get_or_init(Jieba::new);
    let mut words = Vec::new();
    for token in jieba.cut(segment, false) {
        let mut word = String::new();
        for character in token.word.chars() {
            match character.to_pinyin() {
                Some(pinyin) => word.push_str(&pinyin_to_ipa(pinyin.with_tone_num_end())?),
                None => word.push(character),
            }
        }
        words.push(word);
    }
    Ok(words.join(" "))
}

pub(super) fn phonemize(text: &str) -> Result<String, String> {
    let text = expand_numbers(text);
    let mut output = String::new();
    let mut segment = String::new();
    let mut segment_is_han = None;
    let flush = |output: &mut String,
                 segment: &mut String,
                 is_han_segment: Option<bool>|
     -> Result<(), String> {
        if segment.is_empty() {
            return Ok(());
        }
        if is_han_segment == Some(true) {
            output.push_str(&phonemize_han(segment)?);
        } else {
            output.push_str(segment);
        }
        segment.clear();
        Ok(())
    };

    for character in text.chars() {
        if let Some(mark) = map_punctuation(character) {
            flush(&mut output, &mut segment, segment_is_han)?;
            segment_is_han = None;
            output.push_str(mark);
            continue;
        }
        let current_is_han = is_han(character);
        if segment_is_han.is_some_and(|value| value != current_is_han) {
            flush(&mut output, &mut segment, segment_is_han)?;
        }
        segment_is_han = Some(current_is_han);
        segment.push(character);
    }
    flush(&mut output, &mut segment, segment_is_han)?;
    while output.contains("  ") {
        output = output.replace("  ", " ");
    }
    Ok(output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinyin_transcription_matches_misaki_tokens() {
        assert_eq!(pinyin_to_ipa("ni3").unwrap(), "ni↓");
        assert_eq!(pinyin_to_ipa("hao3").unwrap(), "xau↓");
        assert_eq!(pinyin_to_ipa("shi4").unwrap(), "ʂɨ↘");
        assert_eq!(pinyin_to_ipa("jie4").unwrap(), "ʨje↘");
        assert_eq!(pinyin_to_ipa("o1").unwrap(), "ɔ→");
        assert_eq!(pinyin_to_ipa("hng2").unwrap(), "hŋ↗");
    }

    #[test]
    fn mandarin_frontend_phonemizes_words_numbers_and_punctuation() {
        let phonemes = phonemize("你好世界，123！").unwrap();
        assert!(phonemes.starts_with("ni↓xau↓ ʂɨ↘ʨje↘,"), "{phonemes}");
        assert!(phonemes.ends_with('!'), "{phonemes}");
        assert!(phonemes.chars().all(|character| {
            character.is_whitespace() || crate::vocab::vocab_id(character).is_some()
        }));
    }
}
