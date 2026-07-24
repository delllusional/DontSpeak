//! English number → words for TTS.
//!
//! WHY: neither G2P expands digits — ONNX/misaki drops them at vocab (silent);
//! The BART G2P frontend is alphabetic; lexicons never contain numeric tokens such as `"12"`.
//!
//! Runs via [`crate::normalize_kokoro_text`] before split/phonemize. English-only.
//!
//! Cardinals, thousands-grouped (a comma groups only with exactly three digits after
//! it), decimals, ordinals, leading minus; multi-dot keeps separators; alphanumerics
//! get word boundaries; leading-zero / overlong runs digit-by-digit; non-number tokens
//! pass through.

/// `0..=19` spelled out (index = value).
const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

/// Tens words indexed by the tens digit (`TENS[2]` = "twenty"); 0/1 unused.
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Thousands-group scale names, ascending. Index 0 (units) has no word. A number
/// with more groups than this (> 10^21) is read digit-by-digit instead.
const SCALES: [&str; 7] = [
    "",
    " thousand",
    " million",
    " billion",
    " trillion",
    " quadrillion",
    " quintillion",
];

/// Spell a 0..=999 group. Empty for 0 (callers skip zero groups). No leading or
/// trailing spaces.
fn three_digits(n: u64) -> String {
    debug_assert!(n < 1000);
    let mut out = String::new();
    let hundreds = (n / 100) as usize;
    let rest = n % 100;
    if hundreds > 0 {
        out.push_str(ONES[hundreds]);
        out.push_str(" hundred");
    }
    if rest > 0 {
        if hundreds > 0 {
            out.push(' ');
        }
        if rest < 20 {
            out.push_str(ONES[rest as usize]);
        } else {
            out.push_str(TENS[(rest / 10) as usize]);
            if !rest.is_multiple_of(10) {
                out.push('-');
                out.push_str(ONES[(rest % 10) as usize]);
            }
        }
    }
    out
}

/// Spell a non-negative integer in cardinal form. `0` → "zero".
fn cardinal(n: u64) -> String {
    if n == 0 {
        return "zero".to_string();
    }
    let mut groups: Vec<u64> = Vec::new();
    let mut v = n;
    while v > 0 {
        groups.push(v % 1000);
        v /= 1000;
    }
    if groups.len() > SCALES.len() {
        // Beyond named scales; expand_numbers digit-paths before this.
        return n.to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for i in (0..groups.len()).rev() {
        if groups[i] == 0 {
            continue;
        }
        parts.push(format!("{}{}", three_digits(groups[i]), SCALES[i]));
    }
    parts.join(" ")
}

/// Read each digit of `digits` individually ("007" → "zero zero seven"). Used for
/// leading-zero runs (codes/IDs) and the fractional part of a decimal.
fn digit_by_digit(digits: &str) -> String {
    digits
        .chars()
        .filter(|c| c.is_ascii_digit())
        .map(|c| ONES[(c as u8 - b'0') as usize])
        .collect::<Vec<_>>()
        .join(" ")
}

/// Inflect the last space-separated word ("twenty-one" → "twenty-first").
fn to_ordinal(cardinal: &str) -> String {
    let (head, last) = match cardinal.rsplit_once(' ') {
        Some((h, l)) => (Some(h), l),
        None => (None, cardinal),
    };
    let inflected = ordinal_word(last);
    match head {
        Some(h) => format!("{h} {inflected}"),
        None => inflected,
    }
}

/// Single cardinal word (may be hyphenated). Irregulars + `-y` → `-ieth`.
fn ordinal_word(word: &str) -> String {
    if let Some((tens, unit)) = word.split_once('-') {
        return format!("{tens}-{}", ordinal_word(unit));
    }
    match word {
        "one" => "first".into(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    }
}

/// A `,` at `at` separates thousands only with a 1..=3-digit group before it (exactly three
/// after the first comma) and EXACTLY three digits after: "1,2,3" is a digit list, not 123.
/// Side effect of the head rule: "1234,567" and Indian grouping ("12,34,567") stop fusing and
/// read as separate numbers — the expander is English-only (issue #227), and English text
/// never groups that way.
fn thousands_comma(chars: &[char], at: usize, group_len: usize, grouped: bool) -> bool {
    let head_ok = if grouped {
        group_len == 3
    } else {
        (1..=3).contains(&group_len)
    };
    head_ok
        && chars
            .get(at + 1..at + 4)
            .is_some_and(|group| group.iter().all(char::is_ascii_digit))
        && !chars.get(at + 4).is_some_and(char::is_ascii_digit)
}

/// Expand every plain number token in `text` to its English words, leaving all
/// other characters untouched. Idempotent on number-free text.
///
/// Scans by `char` (not bytes) so non-ASCII text is preserved verbatim; every
/// character that participates in number syntax (digits, `,`, `.`, the `st`/`nd`/
/// `rd`/`th` suffix letters, a sign `-`) is ASCII, so a `char` scan is exact.
pub fn expand_numbers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Preceding '-' is a sign only at start / after space or opening delimiter ("3-4" stays range).
        let mut minus = false;
        if out.ends_with('-') {
            let before = out[..out.len() - 1].chars().next_back();
            if matches!(before, None | Some(' ') | Some('(') | Some('[')) {
                out.truncate(out.len() - 1);
                minus = true;
            }
        }
        if !minus
            && out
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphabetic())
        {
            out.push(' ');
        }

        let mut int_digits = String::new();
        let mut group_len = 0usize; // digits since the last accepted comma
        let mut grouped = false; // a thousands comma was accepted
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                int_digits.push(chars[i]);
                group_len += 1;
                i += 1;
            } else if chars[i] == ','
                && !int_digits.is_empty()
                && thousands_comma(&chars, i, group_len, grouped)
            {
                i += 1; // skip thousands comma
                group_len = 0;
                grouped = true;
            } else {
                break;
            }
        }

        let mut frac_digits = String::new();
        if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                frac_digits.push(chars[i]);
                i += 1;
            }
        }

        // Ordinal suffix after integer only; next char must not be alphanumeric ("1street").
        let mut ordinal = false;
        if frac_digits.is_empty() && i + 1 < chars.len() {
            let s0 = chars[i].to_ascii_lowercase();
            let s1 = chars[i + 1].to_ascii_lowercase();
            let is_suffix = matches!((s0, s1), ('s', 't') | ('n', 'd') | ('r', 'd') | ('t', 'h'));
            let next_ok = i + 2 >= chars.len() || !chars[i + 2].is_ascii_alphanumeric();
            if is_suffix && next_ok {
                ordinal = true;
                i += 2;
            }
        }

        if minus {
            out.push_str("minus ");
        }
        let leading_zero = int_digits.len() > 1 && int_digits.starts_with('0');
        let too_long = int_digits.len() > SCALES.len() * 3;
        let int_words = if leading_zero || too_long {
            digit_by_digit(&int_digits)
        } else {
            // 20–21 digits can clear `too_long` yet overflow u64; digit-path, never "zero".
            match int_digits.parse::<u64>() {
                Ok(n) => cardinal(n),
                Err(_) => digit_by_digit(&int_digits),
            }
        };

        if !frac_digits.is_empty() {
            out.push_str(&int_words);
            out.push_str(" point ");
            out.push_str(&digit_by_digit(&frac_digits));
            // Further dots are version-like components ("0.2.2" → "zero point two point two").
            while i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                let mut component = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    component.push(chars[i]);
                    i += 1;
                }
                out.push_str(" point ");
                out.push_str(&digit_by_digit(&component));
            }
        } else if ordinal {
            out.push_str(&to_ordinal(&int_words));
        } else {
            out.push_str(&int_words);
        }
        if i < chars.len() && chars[i].is_ascii_alphabetic() {
            out.push(' ');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cardinals() {
        assert_eq!(expand_numbers("0"), "zero");
        assert_eq!(expand_numbers("7"), "seven");
        assert_eq!(expand_numbers("12"), "twelve");
        assert_eq!(expand_numbers("42"), "forty-two");
        assert_eq!(expand_numbers("100"), "one hundred");
        assert_eq!(expand_numbers("305"), "three hundred five");
        assert_eq!(expand_numbers("2025"), "two thousand twenty-five");
        assert_eq!(expand_numbers("1000000"), "one million");
        assert_eq!(
            expand_numbers("1234567"),
            "one million two hundred thirty-four thousand five hundred sixty-seven"
        );
    }

    #[test]
    fn grouped_and_decimal_and_minus() {
        assert_eq!(expand_numbers("1,000"), "one thousand");
        assert_eq!(
            expand_numbers("12,345"),
            "twelve thousand three hundred forty-five"
        );
        assert_eq!(
            expand_numbers("1,234,567"),
            "one million two hundred thirty-four thousand five hundred sixty-seven"
        );
        assert_eq!(
            expand_numbers("1,234.56"),
            "one thousand two hundred thirty-four point five six"
        );
        assert_eq!(expand_numbers("3.14"), "three point one four");
        assert_eq!(expand_numbers("0.5"), "zero point five");
        assert_eq!(expand_numbers("-5"), "minus five");
        assert_eq!(expand_numbers("(-5)"), "(minus five)");
    }

    #[test]
    fn digit_lists_are_not_fused_into_one_number() {
        // A comma groups thousands only with exactly three digits after it, so an enumeration
        // stays an enumeration instead of reading as one large number.
        assert_eq!(expand_numbers("1,2,3"), "one,two,three");
        assert_eq!(expand_numbers("1,23"), "one,twenty-three");
        assert_eq!(
            expand_numbers("12,3456"),
            "twelve,three thousand four hundred fifty-six"
        );
        // Documented side effect of the head rule (see `thousands_comma`).
        assert_eq!(
            expand_numbers("1234,567"),
            "one thousand two hundred thirty-four,five hundred sixty-seven"
        );
    }

    #[test]
    fn ordinals() {
        assert_eq!(expand_numbers("1st"), "first");
        assert_eq!(expand_numbers("2nd"), "second");
        assert_eq!(expand_numbers("3rd"), "third");
        assert_eq!(expand_numbers("4th"), "fourth");
        assert_eq!(expand_numbers("21st"), "twenty-first");
        assert_eq!(expand_numbers("12th"), "twelfth");
        assert_eq!(expand_numbers("40th"), "fortieth");
    }

    #[test]
    fn leading_zero_reads_digits() {
        assert_eq!(expand_numbers("007"), "zero zero seven");
        assert_eq!(expand_numbers("00"), "zero zero");
    }

    #[test]
    fn u64_overflow_reads_digits_instead_of_zero() {
        // 20 digits > u64::MAX but < too_long (22+); must digit-speak, not "zero".
        let big = "99999999999999999999"; // 20 nines
        assert_eq!(big.len(), 20);
        assert!(
            big.parse::<u64>().is_err(),
            "fixture must actually overflow u64"
        );
        let words = expand_numbers(big);
        assert_ne!(words, "zero", "overflow must not silently read as zero");
        assert_eq!(words, digit_by_digit(big));
    }

    #[test]
    fn embedded_in_sentences() {
        assert_eq!(
            expand_numbers("room 42, at 3 today, 100 items"),
            "room forty-two, at three today, one hundred items"
        );
        assert_eq!(expand_numbers("3-4"), "three-four"); // mid-token hyphen = range
        assert_eq!(expand_numbers("down -3"), "down minus three"); // after space = sign
    }

    #[test]
    fn passthrough_non_numbers() {
        assert_eq!(expand_numbers("hello world"), "hello world");
        assert_eq!(expand_numbers(""), "");
        assert_eq!(expand_numbers("first place"), "first place");
        // Alphanumeric: audible word boundary before digits.
        assert_eq!(expand_numbers("v2"), "v two");
        assert_eq!(expand_numbers("UTF8"), "UTF eight");
        assert_eq!(expand_numbers("42items"), "forty-two items");
    }

    #[test]
    fn dotted_versions_pronounce_every_separator() {
        assert_eq!(expand_numbers("0.2.2"), "zero point two point two");
        assert_eq!(
            expand_numbers("version 10.12.3"),
            "version ten point one two point three"
        );
    }
}
