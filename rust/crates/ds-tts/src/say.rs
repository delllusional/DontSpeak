//! Pure parser for macOS `say -v ?` output (no process; unit-tested).
//!
//! Each line looks like:
//! ```text
//! Samantha            en_US    # Hello! My name is Samantha.
//! Allison (Enhanced)  en_US    # Hello! My name is Allison.
//! Aman (English (India)) en_IN # Hello! My name is Aman.
//! ```
//! Layout: `Name[ (Quality | descriptor)]<run of spaces>locale<spaces># sample`.
//!
//! Strategy:
//!   1. Split on the FIRST `#` — the sample (right) is discarded.
//!   2. On the left, the locale is the LAST whitespace-delimited token (e.g.
//!      `en_US`, `en_IN`); the name is everything before it, trimmed.
//!   3. A trailing `(Enhanced)` / `(Premium)` on the name → [`Quality`]; any
//!      other trailing paren group (e.g. `(English (India))`) is a descriptor and
//!      kept as part of the id/name (NOT a quality). Default quality = `Default`.
//!   4. `language_tag` = locale with `_`→`-` (`en_US` → `en-US`).
//!   5. `id` = the full name INCLUDING any quality suffix — what `say -v` expects
//!      back. `gender` is unknown from `say` ⇒ `None`. `downloadable` = false
//!      (already installed).
//!
//! Malformed lines (no locale token, blank) are skipped.

use crate::{Quality, SpeakerVoice};

/// Parse the full `say -v ?` output into voices. Lines that don't fit the
/// `name … locale # sample` shape are skipped.
pub fn parse_say_voices(out: &str) -> Vec<SpeakerVoice> {
    out.lines().filter_map(parse_line).collect()
}

/// Parse one `say -v ?` line, or `None` if it doesn't match.
fn parse_line(raw: &str) -> Option<SpeakerVoice> {
    let line = raw.trim_end();
    if line.trim().is_empty() {
        return None;
    }
    // Drop the sample after the first '#'.
    let left = match line.split_once('#') {
        Some((l, _)) => l,
        None => line, // some lines may lack a sample; still try.
    };
    let left = left.trim_end();
    if left.trim().is_empty() {
        return None;
    }

    // The locale is the LAST whitespace-delimited token on the left.
    let mut it = left.rsplitn(2, char::is_whitespace);
    let locale = it.next()?.trim();
    let name_region = it.next()?.trim();
    if locale.is_empty() || name_region.is_empty() {
        return None;
    }
    // A locale token must look like a locale (letters, then `_`/`-`, letters):
    // reject anything else so a name with no real locale column is skipped.
    if !looks_like_locale(locale) {
        return None;
    }

    let (quality, _name_without_quality) = split_quality(name_region);
    let language_tag = locale.replace('_', "-");

    Some(SpeakerVoice {
        // `say -v <id>` expects the FULL displayed name incl. any quality suffix.
        id: name_region.to_string(),
        name: name_region.to_string(),
        language_tag,
        downloadable: false,
        gender: None, // `say` does not report gender.
        quality: Some(quality),
    })
}

/// A locale token is `xx`, `xx_YY`, `xx-YY` (ASCII letters + one `_`/`-`).
fn looks_like_locale(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    let mut seen_sep = false;
    for &b in bytes {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' => {}
            b'_' | b'-' if !seen_sep => seen_sep = true,
            _ => return false,
        }
    }
    true
}

/// Detect a trailing `(Enhanced)` / `(Premium)` quality suffix on the name.
/// Returns the quality (Default if none) and the name with the suffix removed.
/// A non-quality trailing paren group (descriptor) is left intact → Default.
fn split_quality(name: &str) -> (Quality, String) {
    let trimmed = name.trim_end();
    if let Some(open) = trimmed.rfind('(') {
        // Only treat it as a quality if the paren group is the LAST thing and
        // closes at the end of the string.
        if trimmed.ends_with(')') {
            let inner = trimmed[open + 1..trimmed.len() - 1].trim();
            let q = match inner {
                "Enhanced" => Some(Quality::Enhanced),
                "Premium" => Some(Quality::Premium),
                _ => None,
            };
            if let Some(q) = q {
                let base = trimmed[..open].trim_end().to_string();
                return (q, base);
            }
        }
    }
    (Quality::Default, trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_line() {
        let v = parse_line("Samantha            en_US    # Hello! My name is Samantha.").unwrap();
        assert_eq!(v.name, "Samantha");
        assert_eq!(v.id, "Samantha");
        assert_eq!(v.language_tag, "en-US");
        assert_eq!(v.quality, Some(Quality::Default));
        assert_eq!(v.gender, None);
        assert!(!v.downloadable);
    }

    #[test]
    fn parses_enhanced_and_premium_quality() {
        let e = parse_line("Allison (Enhanced)  en_US    # Hi.").unwrap();
        assert_eq!(e.quality, Some(Quality::Enhanced));
        // id keeps the suffix so `say -v "Allison (Enhanced)"` round-trips.
        assert_eq!(e.id, "Allison (Enhanced)");
        let p = parse_line("Ava (Premium)       en_US    # Hi.").unwrap();
        assert_eq!(p.quality, Some(Quality::Premium));
        assert_eq!(p.id, "Ava (Premium)");
    }

    #[test]
    fn multi_word_and_nested_paren_name() {
        // "Bad News" — multi-word name, default quality.
        let bn = parse_line("Bad News            en_US    # Hi.").unwrap();
        assert_eq!(bn.name, "Bad News");
        assert_eq!(bn.quality, Some(Quality::Default));
        // "Aman (English (India))" — descriptor paren, NOT a quality.
        let aman = parse_line("Aman (English (India)) en_IN    # Hi.").unwrap();
        assert_eq!(aman.id, "Aman (English (India))");
        assert_eq!(aman.language_tag, "en-IN");
        assert_eq!(aman.quality, Some(Quality::Default));
    }

    #[test]
    fn non_english_locales() {
        let anna = parse_line("Anna                de_DE    # Hallo!").unwrap();
        assert_eq!(anna.language_tag, "de-DE");
        let amelie = parse_line("Amélie              fr_CA    # Bonjour!").unwrap();
        assert_eq!(amelie.name, "Amélie");
        assert_eq!(amelie.language_tag, "fr-CA");
    }

    #[test]
    fn skips_malformed_lines() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        // No locale-looking token before the '#'.
        assert!(parse_line("JustAName # sample").is_none());
        // Only one token, no locale.
        assert!(parse_line("Lonely").is_none());
    }

    #[test]
    fn parses_full_block() {
        let out = "\
Agnes               en_US    # Hello! My name is Agnes.
Allison (Enhanced)  en_US    # Hello! My name is Allison.
Anna                de_DE    # Hallo! Ich heiße Anna.

garbage line with no hash and no locale-token-shape !!!
Ava (Premium)       en_US    # Hello! My name is Ava.";
        let voices = parse_say_voices(out);
        // Blank + the truly-malformed line are skipped; 4 real voices remain.
        assert_eq!(voices.len(), 4);
        assert_eq!(voices[0].name, "Agnes");
        assert_eq!(voices[1].quality, Some(Quality::Enhanced));
        assert_eq!(voices[2].language_tag, "de-DE");
        assert_eq!(voices[3].quality, Some(Quality::Premium));
    }
}
