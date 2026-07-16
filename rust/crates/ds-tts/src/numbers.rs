//! English number → words for TTS.
//!
//! WHY: neither G2P expands digits — ONNX/misaki drops them at vocab (silent);
//! ANE BART (alpha-only) turns `12` into garbage ("X"). Lexicons never have `"12"`.
//!
//! Runs via [`crate::normalize_kokoro_text`] before split/phonemize. English-only.
//!
//! Cardinals, thousands-grouped, decimals, ordinals, leading minus; multi-dot
//! keeps separators; alphanumerics get word boundaries; leading-zero / overlong
//! runs digit-by-digit; non-number tokens pass through.

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
    // Split into thousands groups, least significant first.
    let mut groups: Vec<u64> = Vec::new();
    let mut v = n;
    while v > 0 {
        groups.push(v % 1000);
        v /= 1000;
    }
    if groups.len() > SCALES.len() {
        // Beyond our named scales — caller handles via digit-by-digit; shouldn't reach here.
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

/// Turn a cardinal phrase into its ordinal form by transforming the final word
/// ("twenty-one" → "twenty-first", "two" → "second", "forty" → "fortieth").
fn to_ordinal(cardinal: &str) -> String {
    // The ordinal inflection applies to the LAST space-separated word; a trailing
    // hyphenated unit ("twenty-one") inflects its unit part ("twenty-first").
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

/// Ordinal of a single cardinal word, which may itself be hyphenated
/// ("twenty-one" → "twenty-first"). Handles the irregular forms.
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
        // "twenty" → "twentieth", "forty" → "fortieth", … (-y → -ieth).
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    }
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

        // At the first digit of a run. Fold a directly-preceding '-' that we already
        // emitted into a spoken "minus", but only when it reads as a sign: at the
        // start, or right after a space / opening delimiter (so "3-4" stays a range).
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

        // Integer part, allowing comma group separators between digits.
        let mut int_digits = String::new();
        while i < chars.len() {
            if chars[i].is_ascii_digit() {
                int_digits.push(chars[i]);
                i += 1;
            } else if chars[i] == ','
                && i + 1 < chars.len()
                && chars[i + 1].is_ascii_digit()
                && !int_digits.is_empty()
            {
                i += 1; // skip the thousands comma
            } else {
                break;
            }
        }

        // Optional decimal part: a '.' followed by at least one digit.
        let mut frac_digits = String::new();
        if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
            i += 1; // skip '.'
            while i < chars.len() && chars[i].is_ascii_digit() {
                frac_digits.push(chars[i]);
                i += 1;
            }
        }

        // Optional ordinal suffix immediately after an integer (no decimal): 1st, 21st.
        // The suffix must not run into another alphanumeric (so "1street" isn't ordinal).
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

        // Render. Leading-zero runs (and over-long ones) read digit-by-digit; the
        // sign and decimal forms compose around the integer rendering.
        if minus {
            out.push_str("minus ");
        }
        let leading_zero = int_digits.len() > 1 && int_digits.starts_with('0');
        let too_long = int_digits.len() > SCALES.len() * 3;
        let int_words = if leading_zero || too_long {
            digit_by_digit(&int_digits)
        } else {
            // A 20-21 digit run is short enough to skip the `too_long` digit-by-digit
            // path above, but can still exceed `u64::MAX` (~1.8×10^19, 20 digits) — e.g.
            // a long account/transaction ID. Falling back to `unwrap_or(0)` there would
            // silently mispronounce it as "zero"; read it digit-by-digit instead, same
            // as any other number this module can't name.
            match int_digits.parse::<u64>() {
                Ok(n) => cardinal(n),
                Err(_) => digit_by_digit(&int_digits),
            }
        };

        if !frac_digits.is_empty() {
            out.push_str(&int_words);
            out.push_str(" point ");
            out.push_str(&digit_by_digit(&frac_digits));
            // A second dot means a version/address-like dotted number, not sentence
            // punctuation. Preserve every component as "point <digits>" instead of
            // producing the glued and misleading "zero point two.two" for `0.2.2`.
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
        assert_eq!(expand_numbers("3.14"), "three point one four");
        assert_eq!(expand_numbers("0.5"), "zero point five");
        assert_eq!(expand_numbers("-5"), "minus five");
        assert_eq!(expand_numbers("(-5)"), "(minus five)");
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
        // A 20-digit run that overflows u64::MAX (~1.8×10^19) but is short of the
        // `too_long` (22+ digit) digit-by-digit cutoff. Must NOT silently become "zero".
        let big = "99999999999999999999"; // 20 nines, > u64::MAX
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
        // Hyphen between numbers is NOT a sign mid-token.
        assert_eq!(expand_numbers("3-4"), "three-four");
        // A hyphen that IS a sign (after a space) reads as minus.
        assert_eq!(expand_numbers("down -3"), "down minus three");
    }

    #[test]
    fn passthrough_non_numbers() {
        assert_eq!(expand_numbers("hello world"), "hello world");
        assert_eq!(expand_numbers(""), "");
        // A trailing bare suffix-looking word that isn't after a number is untouched.
        assert_eq!(expand_numbers("first place"), "first place");
        // Technical alphanumeric tokens keep an audible word boundary.
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
