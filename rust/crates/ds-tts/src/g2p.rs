//! The single English text frontend for both Kokoro synthesis backends.
//!
//! The released `voice-g2p` crate provides Misaki's contextual tokenizer, tagger, lexicon,
//! number handling, and morphology. DontSpeak disables its optional external `espeak-ng`
//! process and injects a small ONNX BART pronunciation only for unresolved words.
//!
//! The final stage drops unsupported characters with a warning, then creates model-bounded
//! [`KokoroPhonemeChunk`] values from the Kokoro-safe remainder. ONNX and Apple Core ML consume
//! those exact chunks; neither backend owns a second text frontend.

mod bart;

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

const MAX_OOV_CACHE: usize = 512;

/// DontSpeak-owned pronunciations that are more authoritative than a generic OOV heuristic.
/// These are injected into the contextual upstream pipeline instead of rebuilding an utterance
/// word by word after a miss.
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
    /// The spelling fallback came from a BART error, so a later occurrence must retry the
    /// model instead of treating this degraded pronunciation as a successful cache hit.
    retry_bart: bool,
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
        let g2p = voice_g2p::G2P::with_config(voice_g2p::G2PConfig {
            // An empty executable name cannot resolve through PATH. This keeps the released
            // crate's optional process fallback unreachable on every supported platform.
            espeak_path: String::new(),
        })
        .with_overrides(overrides.clone());
        Self {
            g2p: Some(g2p),
            overrides,
            oov_cache: HashMap::new(),
            oov_order: VecDeque::new(),
            next_placeholder: 0,
        }
    }

    fn convert(&mut self, text: &str) -> Result<String, String> {
        let mut rewritten = String::with_capacity(text.len());
        let mut overrides_changed = false;

        for token in voice_g2p::tokenizer::tokenize_simple(text) {
            let mut surface = token.text;
            if surface.chars().any(char::is_alphabetic) {
                let cache_key = surface.to_lowercase();
                let cached = self.oov_cache.get(&cache_key).cloned();
                // A retryable fallback is already installed as a voice-g2p override, so
                // `is_unresolved()` would now report false. Consult that cache state first:
                // every later occurrence retries BART until a real pronunciation replaces it.
                let needs_resolution = match &cached {
                    Some(entry) => entry.retry_bart,
                    None => self.is_unresolved(&surface),
                };
                let entry = if needs_resolution {
                    let entry = self.resolve_oov(&surface, cached.as_ref());
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

        if overrides_changed {
            let g2p = self
                .g2p
                .take()
                .ok_or_else(|| "English G2P frontend unavailable".to_string())?;
            self.g2p = Some(g2p.with_overrides(self.overrides.clone()));
        }
        self.g2p
            .as_ref()
            .ok_or_else(|| "English G2P frontend unavailable".to_string())?
            .convert(&rewritten)
            .map_err(|e| format!("English G2P failed: {e}"))
    }

    fn is_unresolved(&self, word: &str) -> bool {
        self.g2p
            .as_ref()
            .and_then(|g2p| g2p.convert(word).ok())
            .is_none_or(|phonemes| phonemes.trim().is_empty())
    }

    fn resolve_oov(&mut self, word: &str, previous: Option<&OovEntry>) -> OovEntry {
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
        OovEntry {
            phonemes,
            override_key,
            replacement,
            retry_bart,
        }
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
    static FRONTEND: OnceLock<Mutex<EnglishFrontend>> = OnceLock::new();
    let normalized = crate::normalize_kokoro_text(text);
    // voice-g2p intentionally maps punctuation to model pause tokens. A punctuation-only
    // request therefore looks non-empty after G2P even though there is no speech to synthesize.
    // Decide the no-op at the normalized text boundary, before loading either G2P or synth.
    if !normalized.chars().any(char::is_alphanumeric) {
        return Ok(String::new());
    }
    let mut frontend = FRONTEND
        .get_or_init(|| Mutex::new(EnglishFrontend::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let phonemes = frontend.convert(&normalized)?;
    // The early guard established there was something pronounceable, so silence here is a real
    // frontend failure rather than the successful no-op used for emoji/punctuation-only input.
    if phonemes.trim().is_empty() {
        return Err("English G2P returned no phonemes".to_string());
    }
    Ok(phonemes)
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

/// Convert English text to Kokoro-compatible IPA. Kept as an infallible convenience for
/// diagnostics/examples; synthesis uses [`phoneme_batches_for`] so a frontend error is reported
/// instead of becoming a successful empty utterance.
pub fn phonemize(text: &str) -> String {
    try_phonemize(text).unwrap_or_default()
}

/// `voice` is retained for call-site stability. DontSpeak's Kokoro surface is English-only, so
/// all compatible voices use the same contextual frontend.
pub fn phonemize_for(text: &str, _voice: &str) -> String {
    phonemize(text)
}

/// A backend-ready Kokoro IPA batch. Construction is private so callers receive only chunks
/// whose characters exist in [`crate::vocab::KOKORO_VOCAB`] and whose token count fits both
/// the ONNX voice-style table and FluidAudio's context window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KokoroPhonemeChunk(String);

impl KokoroPhonemeChunk {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Drop phonemes Kokoro's vocabulary doesn't contain, logging the set once.
///
/// LOAD-BEARING failure mode. Kokoro's vocabulary is the MODEL's, not the frontend's: the
/// contextual tokenizer passes some punctuation through verbatim (`„` U+201E is not in
/// [`crate::vocab::KOKORO_VOCAB`]), and an OOV pronunciation can carry a stress or length mark
/// the model never learned. Rejecting the utterance on one such character silenced the ENTIRE
/// reply — strictly worse than the lossy-but-audible behavior it replaced, and reachable from
/// ordinary agent text. Dropping the character keeps the reply speakable; the warning keeps the
/// loss observable instead of silent (a deterministic transliteration layer is the real fix;
/// see `docs/TTS-PIPELINE.md#known-limitations-and-planned-evolution`).
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
pub fn phoneme_batches_for(text: &str, _voice: &str) -> Result<Vec<KokoroPhonemeChunk>, String> {
    let phonemes = drop_unsupported_phonemes(&try_phonemize(text)?);
    Ok(crate::batch::stream_batches(&phonemes)
        .into_iter()
        .map(KokoroPhonemeChunk)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // Reproduce the production hazard state: the convert() that installed the fallback also
        // rebuilt the live g2p with it, so `is_unresolved` reports false for the word. A
        // regressed `needs_resolution` that re-consults `is_unresolved` instead of `retry_bart`
        // (the pre-fix shape) would treat the sentinel as resolved and never retry.
        let g2p = frontend.g2p.take().expect("live g2p");
        frontend.g2p = Some(g2p.with_overrides(frontend.overrides.clone()));
        assert!(
            !frontend.is_unresolved("Zorblax"),
            "premise: the installed override must make is_unresolved report false"
        );

        frontend.convert("Zorblax").expect("retryable OOV");
        let retried = frontend.oov_cache.get("zorblax").expect("cached OOV");
        assert_ne!(
            retried.phonemes, "sˈɛntɪnəl",
            "the retryable sentinel must be replaced by BART or the live spelling fallback"
        );
        assert_eq!(frontend.oov_order.len(), 1, "a retry updates in place");
    }

    /// Sibling guard to the retry test above: an entry cached WITHOUT `retry_bart` is a genuine
    /// hit — `convert` must serve it as-is, not re-resolve it into a spelling fallback.
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

        frontend.convert("Zorblax").expect("cached OOV");
        let cached = frontend.oov_cache.get("zorblax").expect("cached OOV");
        assert_eq!(
            cached.phonemes, "zˈɔɹblæks",
            "a genuine cache hit must not be re-resolved"
        );
    }

    #[test]
    #[ignore = "requires the checksum-pinned G2P graphs and ONNX Runtime in model_dir"]
    fn downloaded_bart_fills_only_the_unknown_word_inside_contextual_prose() {
        let phonemes = try_phonemize("A Zorblax arrived.").unwrap();
        assert!(phonemes.contains("zˈɔɹblæks"), "{phonemes}");
        assert!(phonemes.ends_with('.'), "{phonemes}");
        assert_vocab_safe(&phonemes);
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
        let chunks = phoneme_batches_for(&text, "af_heart").expect("valid Kokoro frontend");
        assert!(chunks.len() > 1);
        for chunk in chunks {
            assert!(chunk.as_str().chars().count() <= crate::vocab::MAX_PHONEME_LENGTH);
            assert_vocab_safe(chunk.as_str());
        }
    }

    #[test]
    fn empty_input_is_a_successful_empty_frontend() {
        assert!(phoneme_batches_for("", "af_heart").unwrap().is_empty());
    }

    /// Restores the coverage guard the frontend rewrite dropped. It matters MORE now than it
    /// did: an out-of-vocabulary character used to be discarded silently at tokenize time, so
    /// the word merely sounded wrong. Everything the G2P emits is now handed straight to the
    /// model, so a miss here is a mispronunciation for every user, on every reply.
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

    /// Regression: one character outside Kokoro's vocabulary used to fail the whole request, so
    /// a single `„` anywhere in a reply produced total silence. It must be dropped, not fatal.
    /// `„` (U+201E) is real: the contextual tokenizer passes it through verbatim as punctuation,
    /// and `KOKORO_VOCAB` has no entry for it.
    #[test]
    fn one_out_of_vocabulary_character_does_not_silence_the_reply() {
        assert!(
            crate::vocab::vocab_id('„').is_none(),
            "test premise stale: „ is now in the vocabulary"
        );
        assert_eq!(drop_unsupported_phonemes("hˈɛlO„ wˈɜɹld"), "hˈɛlO wˈɜɹld");

        let chunks = phoneme_batches_for("The „quoted“ build is ready.", "af_heart")
            .expect("an unsupported character must not fail the utterance");
        assert!(!chunks.is_empty(), "the reply was silenced");
        for chunk in &chunks {
            assert_vocab_safe(chunk.as_str());
        }
    }

    /// Text that renders to nothing speakable is a successful no-op, not a failure — the helper
    /// terminates it with DONE. Returning Err made `ds-helper "🎉"` exit nonzero.
    #[test]
    fn unspeakable_text_yields_no_chunks_rather_than_an_error() {
        for text in ["   ", "🎉", "![](img.png)", "...", "!?", "—"] {
            let chunks = phoneme_batches_for(text, "af_heart")
                .unwrap_or_else(|e| panic!("{text:?} must not be an error: {e}"));
            assert!(chunks.is_empty(), "{text:?} produced pause-only chunks");
        }
    }
}
