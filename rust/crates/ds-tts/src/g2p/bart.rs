//! Tiny BART unknown-word inference for the shared English Kokoro frontend.
//!
//! The model is intentionally narrower than a text frontend: it sees one unresolved word and
//! emits Kokoro/Misaki IPA. Tokenization, lexicon lookup, homographs, initialisms, numbers, and
//! punctuation stay in [`super`]. The encoder/decoder assets are ordinary checksum-pinned model
//! downloads in `ds-model`; no Python or external phonemizer runs in production.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;

const BOS: i64 = 1;
const EOS: i64 = 2;
const MAX_POSITIONS: usize = 64;
const HIDDEN_SIZE: usize = 128;
const MODEL_VOCAB_SIZE: usize = 63;
const VALID_OUTPUT_IDS: usize = 50;

// IDs are their character positions. The four leading placeholders reserve pad/BOS/EOS/unk.
const GRAPHEME_CHARS: &str = "____AIOWYbdfhijklmnpstuvwz'-.BCDEFGHJKLMNPQRSTUVXZacegoqrxy";
const PHONEME_CHARS: &str = "____AIOWYbdfhijklmnpstuvwzæðŋɑɔəɛɜɡɪɹɾʃʊʌʒʔʤʧˈˌθᵊᵻ";

struct BartG2p {
    encoder: Session,
    decoder: Session,
}

/// Cache the loaded model, but NEVER cache a load failure.
///
/// `OnceLock<Result<..>>` memoized the `Err` too, so one transient fault — an AV scanner holding
/// the dylib, an ORT version race, a `Session::builder()` hiccup — permanently downgraded every
/// unknown word to letter-by-letter spelling for the rest of this (long-lived) helper process,
/// with nothing logged. Successful pronunciations are memoized by the caller, while degraded
/// spelling fallbacks remain retryable; the presence check behind `load()` is a cached checksum
/// lookup.
pub(super) fn phonemize(word: &str) -> Result<String, String> {
    static MODEL: OnceLock<Mutex<Option<BartG2p>>> = OnceLock::new();
    static LOAD_FAILURE_LOGGED: AtomicBool = AtomicBool::new(false);

    let mut model = MODEL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if model.is_none() {
        match BartG2p::load() {
            Ok(loaded) => *model = Some(loaded),
            Err(error) => {
                // Log once: an absent model is the ordinary pre-download state, and every
                // distinct unknown word would otherwise repeat it.
                if !LOAD_FAILURE_LOGGED.swap(true, Ordering::SeqCst) {
                    log::warn!(
                        target: "tts",
                        "unknown-word G2P unavailable, spelling words out instead: {error}"
                    );
                }
                return Err(error);
            }
        }
    }
    model.as_mut().expect("loaded above").predict(word)
}

impl BartG2p {
    fn load() -> Result<Self, String> {
        if !ds_model::is_kokoro_g2p_present() {
            return Err("Kokoro G2P model is not downloaded".to_string());
        }
        // G2P runs before the synth backend loads. Select the same runtime library that synth
        // will use so ORT's process-global API cannot be initialized on CPU and later asked to
        // switch to the separately packaged CUDA build.
        let want_gpu = ds_config::provider_pref_wants_gpu(
            &std::env::var("DONTSPEAK_PROVIDER").unwrap_or_else(|_| "auto".to_string()),
        );
        ds_model::ensure_ort_dylib_gpu(want_gpu)?;
        let encoder = load_session(ds_model::KOKORO_G2P_ENCODER_FILE)?;
        let decoder = load_session(ds_model::KOKORO_G2P_DECODER_FILE)?;
        Ok(Self { encoder, decoder })
    }

    fn predict(&mut self, word: &str) -> Result<String, String> {
        let input_ids = encode_word(word)?;
        let attention = vec![1_i64; input_ids.len()];
        let input_len = input_ids.len();

        let hidden = {
            let ids = Tensor::from_array((vec![1_i64, input_len as i64], input_ids))
                .map_err(|e| format!("Kokoro G2P input tensor: {e}"))?;
            let mask = Tensor::from_array((vec![1_i64, input_len as i64], attention.clone()))
                .map_err(|e| format!("Kokoro G2P attention tensor: {e}"))?;
            let outputs = self
                .encoder
                .run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
                .map_err(|e| format!("Kokoro G2P encoder: {e}"))?;
            let (_, data) = outputs["last_hidden_state"]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("Kokoro G2P encoder output: {e}"))?;
            let expected = input_len * HIDDEN_SIZE;
            if data.len() != expected {
                return Err(format!(
                    "Kokoro G2P encoder returned {} values, expected {expected}",
                    data.len()
                ));
            }
            data.to_vec()
        };

        let mut decoder_ids = vec![BOS];
        let mut finished = false;
        for _ in 1..MAX_POSITIONS {
            let decoder_len = decoder_ids.len();
            let ids = Tensor::from_array((vec![1_i64, decoder_len as i64], decoder_ids.clone()))
                .map_err(|e| format!("Kokoro G2P decoder input: {e}"))?;
            let mask = Tensor::from_array((vec![1_i64, input_len as i64], attention.clone()))
                .map_err(|e| format!("Kokoro G2P decoder attention: {e}"))?;
            let encoder_hidden = Tensor::from_array((
                vec![1_i64, input_len as i64, HIDDEN_SIZE as i64],
                hidden.clone(),
            ))
            .map_err(|e| format!("Kokoro G2P hidden state: {e}"))?;
            let outputs = self
                .decoder
                .run(ort::inputs! {
                    "encoder_attention_mask" => mask,
                    "input_ids" => ids,
                    "encoder_hidden_states" => encoder_hidden,
                })
                .map_err(|e| format!("Kokoro G2P decoder: {e}"))?;
            let (_, logits) = outputs["logits"]
                .try_extract_tensor::<f32>()
                .map_err(|e| format!("Kokoro G2P logits: {e}"))?;
            let expected = decoder_len * MODEL_VOCAB_SIZE;
            if logits.len() != expected {
                return Err(format!(
                    "Kokoro G2P decoder returned {} logits, expected {expected}",
                    logits.len()
                ));
            }
            let next = valid_argmax(&logits[(decoder_len - 1) * MODEL_VOCAB_SIZE..]) as i64;
            decoder_ids.push(next);
            if next == EOS {
                finished = true;
                break;
            }
        }
        if !finished {
            return Err("Kokoro G2P output reached its token limit without EOS".to_string());
        }
        decode_phonemes(&decoder_ids)
    }
}

fn load_session(file_name: &str) -> Result<Session, String> {
    let path = ds_model::model_path(file_name).ok_or("cannot resolve model_dir()")?;
    let bytes = ds_model::read_model_file(&path)?;
    let mut builder = Session::builder().map_err(|e| format!("Kokoro G2P session builder: {e}"))?;
    builder = builder
        .with_intra_threads(1)
        .map_err(|e| format!("Kokoro G2P session threads: {e}"))?;
    builder
        .commit_from_memory(&bytes)
        .map_err(|e| format!("Kokoro G2P load {file_name}: {e}"))
}

fn normalize_word(word: &str) -> String {
    word.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' => '-',
            other => other,
        })
        .collect()
}

fn encode_word(word: &str) -> Result<Vec<i64>, String> {
    let word = normalize_word(word);
    let count = word.chars().count();
    if count == 0 || count + 2 > MAX_POSITIONS {
        return Err(format!("unsupported Kokoro G2P word length: {count}"));
    }
    let mut ids = Vec::with_capacity(count + 2);
    ids.push(BOS);
    for c in word.chars() {
        let id = GRAPHEME_CHARS
            .chars()
            .position(|candidate| candidate == c)
            .filter(|&id| id >= 4)
            .ok_or_else(|| format!("unsupported Kokoro G2P character {c:?}"))?;
        ids.push(id as i64);
    }
    ids.push(EOS);
    Ok(ids)
}

fn valid_argmax(logits: &[f32]) -> usize {
    logits[..VALID_OUTPUT_IDS]
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index)
        .unwrap_or(EOS as usize)
}

fn decode_phonemes(ids: &[i64]) -> Result<String, String> {
    let mut output = String::new();
    for &id in ids {
        if id == EOS {
            break;
        }
        if id < 4 {
            continue;
        }
        let phoneme = PHONEME_CHARS
            .chars()
            .nth(id as usize)
            .ok_or_else(|| format!("invalid Kokoro G2P output token {id}"))?;
        output.push(phoneme);
    }
    if output.is_empty() {
        Err("Kokoro G2P returned no phonemes".to_string())
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer_reserves_bos_and_eos_and_normalizes_smart_punctuation() {
        let ids = encode_word("hunter\u{2019}s").unwrap();
        assert_eq!(ids.first(), Some(&BOS));
        assert_eq!(ids.last(), Some(&EOS));
        assert_eq!(ids.len(), "hunter's".chars().count() + 2);
        assert!(encode_word("naïve").is_err());
    }

    /// Drift guard. `GRAPHEME_CHARS`/`PHONEME_CHARS` mirror the pinned checkpoint's
    /// `config.json` by hand, so these exact-map assertions catch reorders and substitutions in
    /// normal CI. A one-character error here yields plausible-but-wrong pronunciations,
    /// not an error.
    #[test]
    fn phoneme_vocabulary_is_kokoro_safe_and_bounds_line_up() {
        // Exact token order from the pinned checkpoint's config.json. Length/vocabulary-only
        // checks cannot catch a reorder or a substitution by another valid Kokoro character;
        // either changes every affected model id while still looking superficially valid.
        assert_eq!(
            GRAPHEME_CHARS,
            "____AIOWYbdfhijklmnpstuvwz'-.BCDEFGHJKLMNPQRSTUVXZacegoqrxy"
        );
        assert_eq!(
            PHONEME_CHARS,
            "____AIOWYbdfhijklmnpstuvwzæðŋɑɔəɛɜɡɪɹɾʃʊʌʒʔʤʧˈˌθᵊᵻ"
        );
        assert_eq!(PHONEME_CHARS.chars().count(), VALID_OUTPUT_IDS);
        assert!(GRAPHEME_CHARS.chars().count() <= MODEL_VOCAB_SIZE);

        // Index 34 is U+0261 LATIN SMALL LETTER SCRIPT G, NOT ASCII 'g'. Kokoro's vocabulary
        // deliberately has no ASCII 'g', so typing this as a plain 'g' would make every word
        // containing /ɡ/ unspeakable.
        assert!(PHONEME_CHARS.contains('\u{0261}'));
        assert!(!PHONEME_CHARS.contains('g'));

        // Everything the decoder can emit must be a phoneme Kokoro actually knows.
        for (id, c) in PHONEME_CHARS.chars().enumerate().skip(4) {
            assert!(
                crate::vocab::vocab_id(c).is_some(),
                "phoneme id {id} ({c:?}) is outside Kokoro's vocabulary"
            );
        }
    }

    #[test]
    fn invalid_shared_projection_ids_cannot_win_argmax() {
        let mut logits = vec![0.0_f32; MODEL_VOCAB_SIZE];
        logits[5] = 10.0;
        logits[62] = 100.0;
        assert_eq!(valid_argmax(&logits), 5);
    }

    #[test]
    fn decoder_ignores_special_tokens_and_rejects_invalid_or_empty_output() {
        assert_eq!(decode_phonemes(&[BOS, BOS, 4, 5, EOS]).unwrap(), "AI");
        assert!(decode_phonemes(&[BOS, EOS]).is_err());
        assert!(decode_phonemes(&[BOS, 50, EOS]).is_err());
    }

}
