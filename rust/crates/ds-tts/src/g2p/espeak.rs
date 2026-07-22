//! MLX Audio-compatible eSpeak frontend for Kokoro's European and Hindi voices.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use libloading::Library;

fn espeak_language(language: &str) -> Option<&'static str> {
    match language {
        "es" => Some("es"),
        "fr" => Some("fr-fr"),
        "hi" => Some("hi"),
        "it" => Some("it"),
        "pt" => Some("pt-br"),
        _ => None,
    }
}

#[repr(C)]
struct EspeakVoice {
    name: *const c_char,
    languages: *const c_char,
    identifier: *const c_char,
    gender: u8,
    age: u8,
    variant: u8,
    reserved: u8,
    score: c_int,
    spare: *mut c_void,
}

type Initialize = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type SetVoiceByProperties = unsafe extern "C" fn(*mut EspeakVoice) -> c_int;
type TextToPhonemes = unsafe extern "C" fn(*mut *const c_void, c_int, c_int) -> *const c_char;

struct EspeakEngine {
    _library: Library,
    set_voice_by_properties: SetVoiceByProperties,
    text_to_phonemes: TextToPhonemes,
}

impl EspeakEngine {
    fn load() -> Result<Self, String> {
        let library_path = ds_model::espeak_library_path()
            .filter(|path| path.is_file())
            .ok_or_else(|| "Kokoro eSpeak frontend is not installed".to_string())?;
        let root = ds_model::espeak_root_dir()
            .filter(|path| path.is_dir())
            .ok_or_else(|| "Kokoro eSpeak data is not installed".to_string())?;
        let root = root
            .to_str()
            .ok_or_else(|| "Kokoro eSpeak data path is not valid UTF-8".to_string())?;
        let root = CString::new(root).map_err(|error| format!("Kokoro eSpeak path: {error}"))?;

        // SAFETY: the path is a checksum-pinned espeakng-loader shared library selected for
        // this target. Symbols below are copied while `library` remains owned by the engine.
        let library = unsafe { Library::new(&library_path) }
            .map_err(|error| format!("load {}: {error}", library_path.display()))?;
        // SAFETY: eSpeak NG's stable C ABI defines these exact signatures.
        let initialize = unsafe {
            *library
                .get::<Initialize>(b"espeak_Initialize\0")
                .map_err(|error| format!("resolve espeak_Initialize: {error}"))?
        };
        // SAFETY: eSpeak NG's stable C ABI defines these exact signatures.
        let set_voice_by_properties = unsafe {
            *library
                .get::<SetVoiceByProperties>(b"espeak_SetVoiceByProperties\0")
                .map_err(|error| format!("resolve espeak_SetVoiceByProperties: {error}"))?
        };
        // SAFETY: eSpeak NG's stable C ABI defines these exact signatures.
        let text_to_phonemes = unsafe {
            *library
                .get::<TextToPhonemes>(b"espeak_TextToPhonemes\0")
                .map_err(|error| format!("resolve espeak_TextToPhonemes: {error}"))?
        };
        // AUDIO_OUTPUT_SYNCHRONOUS (2) matches phonemizer-fork; `root` contains
        // `espeak-ng-data`, which is the path contract of espeak_Initialize.
        // SAFETY: all pointers are live for the duration of the call.
        let sample_rate = unsafe { initialize(2, 0, root.as_ptr(), 0) };
        if sample_rate <= 0 {
            return Err("initialize Kokoro eSpeak frontend".to_string());
        }
        Ok(Self {
            _library: library,
            set_voice_by_properties,
            text_to_phonemes,
        })
    }

    fn phonemize_raw(&mut self, text: &str, language: &str) -> Result<String, String> {
        let language = CString::new(language)
            .map_err(|error| format!("invalid eSpeak language code: {error}"))?;
        let mut voice = EspeakVoice {
            name: std::ptr::null(),
            languages: language.as_ptr(),
            identifier: std::ptr::null(),
            gender: 0,
            age: 0,
            variant: 0,
            reserved: 0,
            score: 0,
            spare: std::ptr::null_mut(),
        };
        // SAFETY: `voice` matches the public espeak_VOICE C layout and its language pointer
        // stays live for the call. eSpeak copies the selected global voice state.
        let status = unsafe { (self.set_voice_by_properties)(&raw mut voice) };
        if status != 0 {
            return Err(format!(
                "eSpeak has no `{}` voice",
                language.to_string_lossy()
            ));
        }

        let input = CString::new(text).map_err(|error| format!("eSpeak input: {error}"))?;
        let mut input_ptr: *const c_void = input.as_ptr().cast();
        let mut output = Vec::new();
        let mut calls = 0usize;
        while !input_ptr.is_null() {
            calls += 1;
            if calls > input.as_bytes().len().saturating_add(1) {
                return Err("eSpeak did not advance through the input".to_string());
            }
            // UTF-8 input (1), IPA output (2), and `^` ties (0x80 plus separator in bits
            // 8..23) are the exact modes used by phonemizer-fork for Misaki.
            let phoneme_mode = 0x82 | (c_int::from(b'^') << 8);
            // SAFETY: eSpeak mutates `input_ptr` only within the live NUL-terminated input
            // and returns a library-owned NUL-terminated buffer valid until the next call.
            let phonemes = unsafe { (self.text_to_phonemes)(&raw mut input_ptr, 1, phoneme_mode) };
            if !phonemes.is_null() {
                // SAFETY: the eSpeak API guarantees a NUL-terminated result pointer.
                let phonemes = unsafe { CStr::from_ptr(phonemes) }
                    .to_str()
                    .map_err(|error| format!("eSpeak returned invalid UTF-8: {error}"))?;
                if !phonemes.trim().is_empty() {
                    output.push(phonemes.trim().to_string());
                }
            }
        }
        Ok(output.join(" "))
    }
}

fn engine_state() -> &'static Mutex<Option<EspeakEngine>> {
    static ENGINE: OnceLock<Mutex<Option<EspeakEngine>>> = OnceLock::new();
    ENGINE.get_or_init(|| Mutex::new(None))
}

fn is_opening_punctuation(character: char) -> bool {
    matches!(character, '(' | '“' | '«')
}

fn is_punctuation(character: char) -> bool {
    matches!(
        character,
        ';' | ':' | ',' | '.' | '!' | '?' | '—' | '…' | '"' | '(' | ')' | '“' | '”' | '«' | '»'
    )
}

fn append_phonemes(output: &mut String, phonemes: &str) {
    if phonemes.is_empty() {
        return;
    }
    if !output.is_empty()
        && !output.ends_with(char::is_whitespace)
        && !output.ends_with(['(', '“', '«'])
    {
        output.push(' ');
    }
    output.push_str(phonemes);
}

fn mlx_punctuation(character: char) -> char {
    match character {
        '«' => '“',
        '»' => '”',
        '(' => '«',
        ')' => '»',
        other => other,
    }
}

fn remove_language_switch_flags(phonemes: &str) -> String {
    let mut output = String::with_capacity(phonemes.len());
    let mut rest = phonemes;
    while let Some(open) = rest.find('(') {
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close) = after_open.find(')') else {
            output.push_str(&rest[open..]);
            return output;
        };
        let candidate = &after_open[..close];
        if !candidate.is_empty()
            && candidate
                .chars()
                .all(|character| character.is_ascii_lowercase() || character == '-')
        {
            rest = &after_open[close + 1..];
        } else {
            output.push('(');
            rest = after_open;
        }
    }
    output.push_str(rest);
    output
}

fn phonemize_preserving_punctuation(
    engine: &mut EspeakEngine,
    text: &str,
    language: &str,
) -> Result<String, String> {
    let mut output = String::new();
    let mut segment = String::new();
    for character in text.chars() {
        if !is_punctuation(character) {
            segment.push(character);
            continue;
        }
        append_phonemes(&mut output, &engine.phonemize_raw(&segment, language)?);
        segment.clear();
        if is_opening_punctuation(character) {
            if !output.is_empty() && !output.ends_with(char::is_whitespace) {
                output.push(' ');
            }
        } else {
            while output.ends_with(char::is_whitespace) {
                output.pop();
            }
        }
        output.push(mlx_punctuation(character));
        if !is_opening_punctuation(character) {
            output.push(' ');
        }
    }
    append_phonemes(&mut output, &engine.phonemize_raw(&segment, language)?);
    Ok(output.trim().to_string())
}

/// Map eSpeak IPA ties to Kokoro symbols the same way Misaki's `EspeakG2P` does
/// (`misaki/espeak.py` `e2m` for non-English languages). Keep this table in lockstep
/// with Misaki — Kokoro was trained on that frontend, not raw eSpeak IPA.
fn normalize_for_kokoro(phonemes: &str) -> String {
    let mut phonemes = remove_language_switch_flags(phonemes)
        .replace("a^ɪ", "I")
        .replace("a^ʊ", "W")
        .replace("d^z", "ʣ")
        .replace("d^ʒ", "ʤ")
        .replace("e^ɪ", "A")
        .replace("o^ʊ", "O")
        .replace("ə^ʊ", "Q")
        .replace("s^s", "S")
        .replace("t^s", "ʦ")
        .replace("t^ʃ", "ʧ")
        .replace("ɔ^ɪ", "Y")
        .replace(['\u{0361}', '\u{035c}', '^'], "")
        .replace('-', "")
        // Kokoro vocab has U+0261 script g only (ASCII `g` would be dropped).
        .replace('g', "ɡ")
        .replace('«', "(")
        .replace('»', ")");

    let mut syllabic = String::with_capacity(phonemes.len());
    for character in phonemes.chars() {
        if character == '\u{0329}' {
            if let Some(previous) = syllabic.pop() {
                syllabic.push('ᵊ');
                syllabic.push(previous);
            }
        } else {
            syllabic.push(character);
        }
    }
    phonemes = syllabic;
    while phonemes.contains("  ") {
        phonemes = phonemes.replace("  ", " ");
    }
    phonemes.trim().to_string()
}

pub(super) fn phonemize(text: &str, language: &str) -> Result<String, String> {
    let language = espeak_language(language)
        .ok_or_else(|| format!("unsupported eSpeak Kokoro language `{language}`"))?;
    let mut state = engine_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.is_none() {
        *state = Some(EspeakEngine::load()?);
    }
    let engine = state
        .as_mut()
        .ok_or_else(|| "Kokoro eSpeak frontend did not initialize".to_string())?;
    phonemize_preserving_punctuation(engine, text, language).map(|p| normalize_for_kokoro(&p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_dispatch_matches_mlx_audio() {
        assert_eq!(espeak_language("es"), Some("es"));
        assert_eq!(espeak_language("fr"), Some("fr-fr"));
        assert_eq!(espeak_language("hi"), Some("hi"));
        assert_eq!(espeak_language("it"), Some("it"));
        assert_eq!(espeak_language("pt"), Some("pt-br"));
        assert_eq!(espeak_language("ja"), None);
        assert_eq!(espeak_language("zh"), None);
        assert_eq!(espeak_language("en"), None);
    }

    #[test]
    fn mlx_espeak_normalization_maps_to_kokoro_symbols() {
        assert_eq!(
            normalize_for_kokoro("a^ɪ d^ʒ e^ɪ o^ʊ t^s t^ʃ ɔ^ɪ g n̩"),
            "I ʤ A O ʦ ʧ Y ɡ ᵊn"
        );
    }

    #[test]
    fn punctuation_classification_preserves_kokoro_tokens() {
        for punctuation in [';', ':', ',', '.', '!', '?', '—', '…', '(', ')', '“', '”'] {
            assert!(is_punctuation(punctuation));
            assert!(crate::vocab::vocab_id(punctuation).is_some());
        }
        assert_eq!(mlx_punctuation('«'), '“');
        assert_eq!(mlx_punctuation('('), '«');
        assert_eq!(normalize_for_kokoro("(en)həlˈo^ʊ(es)"), "həlˈO");
    }
}
