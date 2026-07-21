//! Shared multilingual text frontend for both Kokoro backends.
//!
//! English uses `voice-g2p` (Misaki tokenizer/tagger/lexicon) plus ONNX BART for
//! unresolved words. Spanish, French, Hindi, Italian, and Portuguese use the same
//! eSpeak path as MLX Audio. Final output is vocabulary-filtered and emitted as
//! model-bounded chunks.

mod bart;
mod espeak;

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const MAX_OOV_CACHE: usize = 512;

/// DontSpeak pronunciations injected into the upstream pipeline (not post-miss rebuild).
const OVERRIDES: &[(&str, &str)] = &[
    ("nicole", "nɪkˈOl"),
    ("aoede", "Aˈidi"),
    ("eric", "ˈɛɹɪk"),
    ("fenrir", "fˈɛnɹɪɹ"),
    ("santa", "sˈæntə"),
];

#[derive(Clone)]
struct OovEntry {
    phonemes: String,
    override_key: String,
    replacement: Option<String>,
    /// BART error produced spelling fallback — later occurrences must retry, not cache-hit.
    retry_bart: bool,
}

enum Cancellable<T> {
    Finished(T),
    Cancelled,
}

struct EnglishFrontend {
    g2p: Option<voice_g2p::G2P>,
    overrides: HashMap<String, String>,
    oov_cache: HashMap<String, OovEntry>,
    oov_order: VecDeque<String>,
    next_placeholder: usize,
}

impl EnglishFrontend {
    fn new() -> Self {
        let overrides: HashMap<String, String> = OVERRIDES
            .iter()
            .map(|&(word, phonemes)| (word.to_string(), phonemes.to_string()))
            .collect();
        let g2p = voice_g2p::G2P::with_config(external_espeak_disabled_config())
            .with_overrides(overrides.clone());
        Self {
            g2p: Some(g2p),
            overrides,
            oov_cache: HashMap::new(),
            oov_order: VecDeque::new(),
            next_placeholder: 0,
        }
    }

    fn convert(
        &mut self,
        text: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Cancellable<String>, String> {
        let mut rewritten = String::with_capacity(text.len());
        let mut overrides_changed = false;

        for token in voice_g2p::tokenizer::tokenize_simple(text) {
            if cancelled() {
                self.refresh_overrides(overrides_changed)?;
                return Ok(Cancellable::Cancelled);
            }
            let mut surface = token.text;
            if surface.chars().any(char::is_alphabetic) {
                let cache_key = surface.to_lowercase();
                let cached = self.oov_cache.get(&cache_key).cloned();
                // Installed retryable override makes `is_unresolved` false — consult
                // `retry_bart` so later occurrences re-try BART until a real hit lands.
                let needs_resolution = match &cached {
                    Some(entry) => entry.retry_bart,
                    None => self.is_unresolved(&surface),
                };
                let entry = if needs_resolution {
                    let entry = match self.resolve_oov(&surface, cached.as_ref(), cancelled) {
                        Cancellable::Finished(entry) => entry,
                        Cancellable::Cancelled => {
                            self.refresh_overrides(overrides_changed)?;
                            return Ok(Cancellable::Cancelled);
                        }
                    };
                    self.insert_oov(cache_key, entry.clone());
                    overrides_changed = true;
                    Some(entry)
                } else {
                    cached
                };
                if let Some(replacement) = entry.and_then(|entry| entry.replacement) {
                    surface = replacement;
                }
            }
            rewritten.push_str(&surface);
            rewritten.push_str(&token.whitespace);
        }

        self.refresh_overrides(overrides_changed)?;
        if cancelled() {
            return Ok(Cancellable::Cancelled);
        }
        let phonemes = self
            .g2p
            .as_ref()
            .ok_or_else(|| "English G2P frontend unavailable".to_string())?
            .convert(&rewritten)
            .map_err(|e| format!("English G2P failed: {e}"))?;
        if cancelled() {
            Ok(Cancellable::Cancelled)
        } else {
            Ok(Cancellable::Finished(phonemes))
        }
    }

    fn is_unresolved(&self, word: &str) -> bool {
        self.g2p
            .as_ref()
            .and_then(|g2p| g2p.convert(word).ok())
            .is_none_or(|phonemes| phonemes.trim().is_empty())
    }

    fn resolve_oov(
        &mut self,
        word: &str,
        previous: Option<&OovEntry>,
        cancelled: &impl Fn() -> bool,
    ) -> Cancellable<OovEntry> {
        if cancelled() {
            return Cancellable::Cancelled;
        }
        let (phonemes, retry_bart) = match bart::phonemize(word)
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            Some(phonemes) => (phonemes, false),
            None => {
                let spelling = spell_out(word);
                let fallback = if spelling.is_empty() {
                    "ʌnˈnOn".to_string()
                } else {
                    spelling
                };
                (fallback, true)
            }
        };
        if cancelled() {
            return Cancellable::Cancelled;
        }
        let grouped = voice_g2p::tokenizer::subtokenize(word).len() > 1;
        let (override_key, replacement) = if grouped {
            match previous {
                Some(entry) => (entry.override_key.clone(), entry.replacement.clone()),
                None => {
                    let placeholder = self.next_placeholder();
                    (placeholder.clone(), Some(placeholder))
                }
            }
        } else {
            (word.to_lowercase(), None)
        };
        Cancellable::Finished(OovEntry {
            phonemes,
            override_key,
            replacement,
            retry_bart,
        })
    }

    fn refresh_overrides(&mut self, changed: bool) -> Result<(), String> {
        if changed {
            let g2p = self
                .g2p
                .take()
                .ok_or_else(|| "English G2P frontend unavailable".to_string())?;
            self.g2p = Some(g2p.with_overrides(self.overrides.clone()));
        }
        Ok(())
    }

    fn insert_oov(&mut self, cache_key: String, entry: OovEntry) {
        if let Some(old) = self.oov_cache.get(&cache_key) {
            if old.override_key != entry.override_key {
                self.overrides.remove(&old.override_key);
            }
            self.overrides
                .insert(entry.override_key.clone(), entry.phonemes.clone());
            self.oov_cache.insert(cache_key, entry);
            return;
        }
        if self.oov_cache.len() >= MAX_OOV_CACHE
            && let Some(oldest) = self.oov_order.pop_front()
            && let Some(old) = self.oov_cache.remove(&oldest)
        {
            self.overrides.remove(&old.override_key);
        }
        self.overrides
            .insert(entry.override_key.clone(), entry.phonemes.clone());
        self.oov_order.push_back(cache_key.clone());
        self.oov_cache.insert(cache_key, entry);
    }

    fn next_placeholder(&mut self) -> String {
        let mut value = self.next_placeholder;
        self.next_placeholder = self.next_placeholder.wrapping_add(1);
        let mut suffix = String::new();
        loop {
            suffix.push((b'a' + (value % 26) as u8) as char);
            value /= 26;
            if value == 0 {
                break;
            }
        }
        format!("dontspeakoov{suffix}")
    }
}

fn try_phonemize(text: &str) -> Result<String, String> {
    match try_phonemize_english_cancellable(text, &|| false)? {
        Cancellable::Finished(phonemes) => Ok(phonemes),
        Cancellable::Cancelled => unreachable!("the non-cancellable frontend cannot cancel"),
    }
}

fn try_phonemize_english_cancellable(
    text: &str,
    cancelled: &impl Fn() -> bool,
) -> Result<Cancellable<String>, String> {
    static FRONTEND: OnceLock<Mutex<EnglishFrontend>> = OnceLock::new();
    let normalized = crate::normalize_kokoro_text(text);
    // Punctuation maps to pause tokens; no-op before loading G2P/synth if nothing alphanumeric.
    if !normalized.chars().any(char::is_alphanumeric) {
        return Ok(Cancellable::Finished(String::new()));
    }
    if cancelled() {
        return Ok(Cancellable::Cancelled);
    }
    let mut frontend = FRONTEND
        .get_or_init(|| Mutex::new(EnglishFrontend::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let phonemes = match frontend.convert(&normalized, cancelled)? {
        Cancellable::Finished(phonemes) => phonemes,
        Cancellable::Cancelled => return Ok(Cancellable::Cancelled),
    };
    // Past the alphanumeric guard: empty phonemes is a real frontend failure.
    if phonemes.trim().is_empty() {
        return Err("English G2P returned no phonemes".to_string());
    }
    Ok(Cancellable::Finished(phonemes))
}

/// English OOV resolution stays on the pinned BART graphs; multilingual eSpeak is called
/// in-process through [`espeak`] rather than through `voice-g2p`'s external process hook.
fn external_espeak_disabled_config() -> voice_g2p::G2PConfig {
    voice_g2p::G2PConfig {
        espeak_path: String::new(),
    }
}

fn spell_out(word: &str) -> String {
    word.chars()
        .filter_map(letter_phonemes)
        .collect::<Vec<_>>()
        .join(" ")
}

fn letter_phonemes(c: char) -> Option<&'static str> {
    Some(match c.to_ascii_lowercase() {
        'a' => "ˈA",
        'b' => "bˈi",
        'c' => "sˈi",
        'd' => "dˈi",
        'e' => "ˈi",
        'f' => "ˈɛf",
        'g' => "ʤˈi",
        'h' => "ˈAʧ",
        'i' => "ˈI",
        'j' => "ʤˈA",
        'k' => "kˈA",
        'l' => "ˌɛl",
        'm' => "ˈɛm",
        'n' => "ˈɛn",
        'o' => "ˈO",
        'p' => "pˈi",
        'q' => "kjˈu",
        'r' => "ɑɹ",
        's' => "ˈɛs",
        't' => "tˈi",
        'u' => "ju",
        'v' => "vˈi",
        'w' => "dˈʌbᵊl ju",
        'x' => "ˈɛks",
        'y' => "wˌI",
        'z' => "zˈi",
        _ => return None,
    })
}

/// Diagnostics/examples only; synthesis uses [`phoneme_batches_for`] (errors surface).
pub fn phonemize(text: &str) -> String {
    try_phonemize(text).unwrap_or_default()
}

/// `voice` kept for call-site stability; English-only frontend is shared across voices.
pub fn phonemize_for(text: &str, _voice: &str) -> String {
    phonemize(text)
}

/// Backend-ready IPA chunk: vocab-safe chars within Kokoro's token limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KokoroPhonemeChunk(String);

impl KokoroPhonemeChunk {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Cancellable Markdown→IPA. Cancel = success (helper protocol: stopped speech is DONE).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhonemeBatchesOutcome {
    Finished(Vec<KokoroPhonemeChunk>),
    Cancelled,
}

/// Drop model-unknown phonemes (log once). Vocab is the model's: tokenizer may pass
/// punctuation (`„` U+201E) or marks the model never learned. One OOV char used to
/// silence the whole reply; drop keeps speakable output (see TTS-PIPELINE limitations).
fn drop_unsupported_phonemes(phonemes: &str) -> String {
    let mut dropped: Vec<char> = Vec::new();
    let kept: String = phonemes
        .chars()
        .filter(|&c| {
            if crate::vocab::vocab_id(c).is_some() {
                return true;
            }
            if !dropped.contains(&c) {
                dropped.push(c);
            }
            false
        })
        .collect();
    if !dropped.is_empty() {
        log::warn!(target: "tts", "dropped phonemes outside Kokoro's vocabulary: {dropped:?}");
    }
    kept
}

/// Normalize and phonemize once, validate the IPA, then return model-bounded chunks. Both
/// synthesis backends consume this representation; backend code must never split or phonemize
/// raw text independently.
///
/// An empty result means "nothing speakable" (an image-only or punctuation-only block), which
/// is a successful no-op — callers must not report it as a synthesis failure.
pub fn phoneme_batches_for(
    text: &str,
    voice: &str,
    language: &str,
) -> Result<Vec<KokoroPhonemeChunk>, String> {
    match phoneme_batches_for_cancellable(text, voice, language, || false)? {
        PhonemeBatchesOutcome::Finished(batches) => Ok(batches),
        PhonemeBatchesOutcome::Cancelled => {
            unreachable!("the non-cancellable frontend cannot cancel")
        }
    }
}

/// Cancellable form of [`phoneme_batches_for`]. The callback is checked between frontend
/// tokens and immediately before and after every potentially slow BART OOV inference.
pub fn phoneme_batches_for_cancellable(
    text: &str,
    voice: &str,
    language: &str,
    cancelled: impl Fn() -> bool,
) -> Result<PhonemeBatchesOutcome, String> {
    let voice_language = ds_voices::enumerate::kokoro_language(voice);
    if voice_language != "other" && voice_language != language {
        log::warn!(
            target: "tts",
            "Kokoro voice '{voice}' is {voice_language}, but the text language is {language}"
        );
    }
    let phonemes = match try_phonemize_for_cancellable(text, language, &cancelled)? {
        Cancellable::Finished(phonemes) => phonemes,
        Cancellable::Cancelled => return Ok(PhonemeBatchesOutcome::Cancelled),
    };
    let batches = crate::batch::stream_batches(&drop_unsupported_phonemes(&phonemes))
        .into_iter()
        .map(KokoroPhonemeChunk)
        .collect();
    Ok(PhonemeBatchesOutcome::Finished(batches))
}

/// Which frontend owns a Kokoro language. Split out from the dispatch so the mapping can
/// be pinned against the languages ds-config publishes without loading a G2P runtime — a
/// language added there but not routed here would otherwise fail only at synthesis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KokoroFrontend {
    English,
    Espeak,
}

impl KokoroFrontend {
    fn for_language(language: &str) -> Option<Self> {
        Some(match language {
            "en" => Self::English,
            "es" | "fr" | "hi" | "it" | "pt" => Self::Espeak,
            _ => return None,
        })
    }
}

fn try_phonemize_for_cancellable(
    text: &str,
    language: &str,
    cancelled: &impl Fn() -> bool,
) -> Result<Cancellable<String>, String> {
    if KokoroFrontend::for_language(language) == Some(KokoroFrontend::English) {
        return try_phonemize_english_cancellable(text, cancelled);
    }
    let normalized = crate::normalize_spoken_text(text);
    if !normalized.chars().any(char::is_alphanumeric) {
        return Ok(Cancellable::Finished(String::new()));
    }
    if cancelled() {
        return Ok(Cancellable::Cancelled);
    }
    let phonemes = match KokoroFrontend::for_language(language) {
        Some(KokoroFrontend::Espeak) => espeak::phonemize(&normalized, language)?,
        Some(KokoroFrontend::English) => unreachable!("English returns above"),
        None => return Err(format!("unsupported Kokoro language: {language}")),
    };
    if cancelled() {
        return Ok(Cancellable::Cancelled);
    }
    if phonemes.trim().is_empty() {
        return Err(format!("{language} G2P returned no phonemes"));
    }
    Ok(Cancellable::Finished(phonemes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ds-config publishes Kokoro's languages; this dispatch must route every one of
    /// them. Adding a language there without wiring a frontend here would surface only
    /// as a failed utterance at runtime.
    #[test]
    fn every_published_kokoro_language_is_routed() {
        for language in ds_config::TtsModel::Kokoro.descriptor().languages {
            assert!(
                KokoroFrontend::for_language(language).is_some(),
                "no G2P frontend routes published language '{language}'"
            );
        }
        assert_eq!(KokoroFrontend::for_language("cs"), None);
    }

    fn assert_vocab_safe(phonemes: &str) {
        let unsupported: Vec<char> = phonemes
            .chars()
            .filter(|&c| crate::vocab::vocab_id(c).is_none())
            .collect();
        assert!(
            unsupported.is_empty(),
            "unsupported Kokoro phonemes {unsupported:?} in {phonemes:?}"
        );
    }

    #[test]
    fn common_and_context_sensitive_prose_is_vocab_safe() {
        for text in [
            "Hello world.",
            "I read every day, but yesterday I read the report.",
            "Please record the new record.",
            "Don't lose contractions, punctuation, or sentence context.",
        ] {
            let phonemes = try_phonemize(text).expect("contextual G2P");
            assert!(!phonemes.trim().is_empty());
            assert_vocab_safe(&phonemes);
        }
    }

    #[test]
    fn known_voice_names_use_dontspeak_overrides() {
        for &(name, expected) in OVERRIDES {
            assert_eq!(try_phonemize(name).unwrap().trim(), expected, "{name}");
        }
    }

    #[test]
    fn unknown_word_inside_sentence_is_audible_without_losing_the_sentence() {
        let phonemes = try_phonemize("The Yanchenko build is ready.").unwrap();
        assert!(!phonemes.trim().is_empty());
        assert_vocab_safe(&phonemes);
        assert!(
            phonemes.contains('.'),
            "sentence punctuation was lost: {phonemes}"
        );
    }

    #[test]
    fn retryable_oov_fallback_is_not_treated_as_a_successful_cache_hit() {
        let mut frontend = EnglishFrontend::new();
        frontend.insert_oov(
            "zorblax".to_string(),
            OovEntry {
                phonemes: "sˈɛntɪnəl".to_string(),
                override_key: "zorblax".to_string(),
                replacement: None,
                retry_bart: true,
            },
        );

        // Override installed → `is_unresolved` false; must still re-resolve via `retry_bart`.
        let g2p = frontend.g2p.take().expect("live g2p");
        frontend.g2p = Some(g2p.with_overrides(frontend.overrides.clone()));
        assert!(
            !frontend.is_unresolved("Zorblax"),
            "premise: the installed override must make is_unresolved report false"
        );

        assert!(matches!(
            frontend
                .convert("Zorblax", &|| false)
                .expect("retryable OOV"),
            Cancellable::Finished(_)
        ));
        let retried = frontend.oov_cache.get("zorblax").expect("cached OOV");
        assert_ne!(
            retried.phonemes, "sˈɛntɪnəl",
            "the retryable sentinel must be replaced by BART or the live spelling fallback"
        );
        assert_eq!(frontend.oov_order.len(), 1, "a retry updates in place");
    }

    /// Genuine cache hit (`retry_bart` false) is served as-is.
    #[test]
    fn cached_pronunciation_without_retry_flag_is_served_not_re_resolved() {
        let mut frontend = EnglishFrontend::new();
        frontend.insert_oov(
            "zorblax".to_string(),
            OovEntry {
                phonemes: "zˈɔɹblæks".to_string(),
                override_key: "zorblax".to_string(),
                replacement: None,
                retry_bart: false,
            },
        );
        let g2p = frontend.g2p.take().expect("live g2p");
        frontend.g2p = Some(g2p.with_overrides(frontend.overrides.clone()));

        assert!(matches!(
            frontend.convert("Zorblax", &|| false).expect("cached OOV"),
            Cancellable::Finished(_)
        ));
        let cached = frontend.oov_cache.get("zorblax").expect("cached OOV");
        assert_eq!(
            cached.phonemes, "zˈɔɹblæks",
            "a genuine cache hit must not be re-resolved"
        );
    }

    /// Cancel at OOV boundary skips BART (identifier-heavy stop path).
    #[test]
    fn cancellation_before_oov_inference_is_a_distinct_outcome() {
        let mut frontend = EnglishFrontend::new();
        assert!(matches!(
            frontend.resolve_oov("Zorblax", None, &|| true),
            Cancellable::Cancelled
        ));
    }

    #[test]
    fn cancellable_batching_reports_cancellation_without_an_error() {
        assert!(matches!(
            phoneme_batches_for_cancellable("Zorblax", "af_heart", "en", || true)
                .expect("cancellation is not a frontend error"),
            PhonemeBatchesOutcome::Cancelled
        ));
    }

    #[test]
    fn english_does_not_spawn_an_external_espeak_process() {
        assert!(
            external_espeak_disabled_config().espeak_path.is_empty(),
            "English OOV must stay on the in-process BART path"
        );
    }

    /// Hand-authored IPA (letters, OVERRIDES, unknown fallback) must stay in KOKORO_VOCAB.
    #[test]
    fn hand_authored_phoneme_tables_are_vocab_safe() {
        let mut all: Vec<(String, &str)> = ('a'..='z')
            .map(|c| (c.to_string(), letter_phonemes(c).expect("letter covered")))
            .collect();
        all.extend(OVERRIDES.iter().map(|&(w, p)| (w.to_string(), p)));
        all.push(("unknown-word fallback".to_string(), "ʌnˈnOn"));
        for (label, phonemes) in all {
            assert!(
                !crate::vocab::tokenize(phonemes).is_empty(),
                "{label}: {phonemes:?} tokenizes to nothing"
            );
            for ch in phonemes.chars().filter(|ch| !ch.is_whitespace()) {
                assert!(
                    crate::vocab::vocab_id(ch).is_some(),
                    "{label}: {ch:?} in {phonemes:?} is not in the Kokoro vocab"
                );
            }
        }
    }

    #[test]
    fn technical_identifiers_and_numbers_remain_audible() {
        for text in [
            "Kokoro and ONNX share one G2P pipeline.",
            "Build 57 in room 1,000.",
            "Use JSON, MCP, WebRTC, and UTF8.",
            "The version is 0.2.2.",
        ] {
            let phonemes = try_phonemize(text).unwrap();
            assert!(!phonemes.trim().is_empty(), "silent output for {text:?}");
            assert_vocab_safe(&phonemes);
        }
    }

    #[test]
    fn shared_frontend_returns_only_valid_model_bounded_chunks() {
        let text =
            "A long narration sentence with several technical words and identifiers. ".repeat(30);
        let chunks = phoneme_batches_for(&text, "af_heart", "en").expect("valid Kokoro frontend");
        assert!(chunks.len() > 1);
        for chunk in chunks {
            assert!(chunk.as_str().chars().count() <= crate::vocab::MAX_PHONEME_LENGTH);
            assert_vocab_safe(chunk.as_str());
        }
    }

    #[test]
    fn empty_input_is_a_successful_empty_frontend() {
        assert!(
            phoneme_batches_for("", "af_heart", "en")
                .unwrap()
                .is_empty()
        );
    }

    /// G2P output goes straight to the model; OOV chars mispronounce every reply.
    #[test]
    fn g2p_output_is_vocab_safe_for_common_and_edge_words() {
        for text in [
            "rhythm",
            "syzygy",
            "colonel",
            "onomatopoeia",
            "Wednesday",
            "February",
            "schedule",
            "sixths",
            "queue",
            "choir",
            "yacht",
            "epitome",
            "hyperbole",
            "salmon",
            "receipt",
            "answer",
            "island",
            "subtle",
            "knead",
            "gnome",
            "psalm",
        ] {
            let phonemes = try_phonemize(text).expect("contextual G2P");
            assert!(!phonemes.trim().is_empty(), "silent output for {text:?}");
            assert_vocab_safe(&phonemes);
        }
    }

    /// One OOV char (`„` U+201E from tokenizer) must drop, not silence the reply.
    #[test]
    fn one_out_of_vocabulary_character_does_not_silence_the_reply() {
        assert!(
            crate::vocab::vocab_id('„').is_none(),
            "test premise stale: „ is now in the vocabulary"
        );
        assert_eq!(drop_unsupported_phonemes("hˈɛlO„ wˈɜɹld"), "hˈɛlO wˈɜɹld");

        let chunks = phoneme_batches_for("The „quoted“ build is ready.", "af_heart", "en")
            .expect("an unsupported character must not fail the utterance");
        assert!(!chunks.is_empty(), "the reply was silenced");
        for chunk in &chunks {
            assert_vocab_safe(chunk.as_str());
        }
    }

    /// Nothing speakable → empty success (DONE); Err would fail `ds-helper "🎉"`.
    #[test]
    fn unspeakable_text_yields_no_chunks_rather_than_an_error() {
        for text in ["   ", "🎉", "![](img.png)", "...", "!?", "—"] {
            let chunks = phoneme_batches_for(text, "af_heart", "en")
                .unwrap_or_else(|e| panic!("{text:?} must not be an error: {e}"));
            assert!(chunks.is_empty(), "{text:?} produced pause-only chunks");
        }
    }
}
