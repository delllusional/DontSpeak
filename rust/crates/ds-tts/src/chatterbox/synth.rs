//! Chatterbox Multilingual ONNX inference — the multi-session autoregressive pipeline.
//!
//! 1:1 port of the MIT reference in the model card (constants below are that contract).
//! Three RESIDENT sessions (embed_tokens, language_model, conditional_decoder) plus a
//! TRANSIENT speech_encoder run once per voice (`ensure_voice`, cached; keeps ~230 MB
//! of encoder weights out of steady-state RAM).
//!
//! Sessions load with `commit_from_file` — a deliberate divergence from `KokoroSynth`'s
//! `commit_from_memory`: each graph resolves its external `.onnx_data` weights RELATIVE
//! TO THE MODEL PATH, which an in-memory load has none of.
//!
//! Pure token-loop helpers (penalty/argmax/stop/KV-name enumeration/window) are factored
//! out and unit-tested without ORT; the sessions themselves need the real model.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use half::f16;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};

use super::tokenizer::ChatterboxTokenizer;

// Reference-pipeline constants (model card + generation_config.json).
pub(crate) const START_SPEECH_TOKEN: i64 = 6561;
pub(crate) const STOP_SPEECH_TOKEN: i64 = 6562;
const MAX_NEW_TOKENS: usize = 1024;
const REPETITION_PENALTY: f32 = 1.2;

/// A flat tensor + its shape, kept host-side between steps (KV cache, conditioning).
struct TensorData<T> {
    shape: Vec<i64>,
    data: Vec<T>,
}

#[derive(Default)]
struct CangjieConverter {
    word_to_code: HashMap<char, String>,
    code_to_words: HashMap<String, Vec<char>>,
}

impl CangjieConverter {
    fn load(path: &std::path::Path) -> Result<Self, String> {
        let entries: Vec<String> = serde_json::from_slice(
            &std::fs::read(path).map_err(|error| format!("cangjie mapping read: {error}"))?,
        )
        .map_err(|error| format!("cangjie mapping parse: {error}"))?;
        let mut converter = Self::default();
        for entry in entries {
            let mut fields = entry.split('\t');
            let Some(word) = fields.next().and_then(|word| word.chars().next()) else {
                continue;
            };
            let Some(code) = fields.next().filter(|code| !code.is_empty()) else {
                continue;
            };
            converter.word_to_code.insert(word, code.to_string());
            converter
                .code_to_words
                .entry(code.to_string())
                .or_default()
                .push(word);
        }
        if converter.word_to_code.is_empty() {
            return Err("cangjie mapping is empty".to_string());
        }
        Ok(converter)
    }

    fn convert(&self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for glyph in text.chars() {
            let Some(code) = self.word_to_code.get(&glyph) else {
                output.push(glyph);
                continue;
            };
            let index = self
                .code_to_words
                .get(code)
                .and_then(|words| words.iter().position(|word| *word == glyph))
                .unwrap_or(0);
            let encoded = if index > 0 {
                format!("{code}{index}")
            } else {
                code.clone()
            };
            for symbol in encoded.chars() {
                output.push_str("[cj_");
                output.push(symbol);
                output.push(']');
            }
            output.push_str("[cj_.]");
        }
        output
    }
}

fn prepare_language(text: &str, language: &str, cangjie: &CangjieConverter) -> String {
    let text = match language {
        "zh" => cangjie.convert(text),
        "ko" => decompose_hangul(text),
        _ => text.to_string(),
    };
    format!("[{language}]{text}")
}

fn decompose_hangul(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for character in text.chars() {
        let code = character as u32;
        if !(0xAC00..=0xD7AF).contains(&code) {
            output.push(character);
            continue;
        }
        let offset = code - 0xAC00;
        output.push(char::from_u32(0x1100 + offset / (21 * 28)).expect("valid Hangul initial"));
        output
            .push(char::from_u32(0x1161 + (offset % (21 * 28)) / 28).expect("valid Hangul medial"));
        let final_index = offset % 28;
        if final_index > 0 {
            output.push(char::from_u32(0x11A7 + final_index).expect("valid Hangul final"));
        }
    }
    output.trim().to_string()
}

impl<T: Clone + ort::value::PrimitiveTensorElementType + std::fmt::Debug + 'static> TensorData<T> {
    fn to_tensor(&self) -> Result<Tensor<T>, String> {
        if self.data.is_empty() {
            // Step 0's KV cache has a zero-length seq axis. `from_array` rejects zero
            // dims for raw data, but ORT itself supports empty tensors — allocate one
            // directly (zeroed by ort; no bytes to fill anyway).
            return Tensor::new(&ort::memory::Allocator::default(), self.shape.clone())
                .map_err(|e| format!("empty tensor: {e}"));
        }
        Tensor::from_array((self.shape.clone(), self.data.clone()))
            .map_err(|e| format!("tensor: {e}"))
    }
}

impl TensorData<f32> {
    /// Build an f16 tensor from this f32 data. The pinned FP16 language_model export takes
    /// its embeddings and KV cache as float16; the loop keeps f32 host-side (argmax/penalty
    /// math) and narrows only at the model boundary.
    fn to_tensor_f16(&self) -> Result<Tensor<f16>, String> {
        if self.data.is_empty() {
            return Tensor::new(&ort::memory::Allocator::default(), self.shape.clone())
                .map_err(|e| format!("empty f16 tensor: {e}"));
        }
        let data: Vec<f16> = self.data.iter().map(|&x| f16::from_f32(x)).collect();
        Tensor::from_array((self.shape.clone(), data)).map_err(|e| format!("f16 tensor: {e}"))
    }
}

/// Cached per-voice outputs of the transient speech encoder.
pub struct VoiceConditioning {
    audio_features: TensorData<f32>,
    audio_tokens: TensorData<i64>,
    speaker_embeddings: TensorData<f32>,
    speaker_features: TensorData<f32>,
}

pub struct ChatterboxSynth {
    dir: PathBuf,
    embed: Session,
    lm: Session,
    decoder: Session,
    tokenizer: ChatterboxTokenizer,
    cangjie: CangjieConverter,
    voices: HashMap<String, Arc<VoiceConditioning>>,
    /// `past_key_values.*` input names in session-declared order; present outputs pair
    /// by layer/key name so exporter output ordering cannot corrupt the cache.
    lm_past_names: Vec<String>,
    lm_past_shapes: Vec<Vec<i64>>,
    lm_present_indices: Vec<usize>,
    lm_logits_index: usize,
    provider: ds_config::RealizedProvider,
}

impl ChatterboxSynth {
    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.provider
    }

    /// Load the resident sessions + tokenizer from the variant's asset dir. Call
    /// [`ds_model::ensure_ort_dylib_gpu`] (or set the path) first; never downloads.
    pub fn load() -> Result<Self, String> {
        crate::ort_session::load_with_fallback("chatterbox", Self::load_with_provider)
    }

    pub fn load_with_provider(preference: &str) -> Result<Self, String> {
        let dir = ds_model::tts_model_dir(ds_config::TtsModel::Chatterbox)
            .ok_or("cannot resolve model_dir()")?;
        let mut sessions = crate::ort_session::OrtSessions::from_preference(
            ds_config::TtsModel::Chatterbox,
            preference,
        );
        let embed = Self::session(&mut sessions, &dir, "embed_tokens")?;
        let lm = Self::session(&mut sessions, &dir, "language_model")?;
        let decoder = Self::session(&mut sessions, &dir, "conditional_decoder")?;
        let provider = sessions.provider();
        let tokenizer = ChatterboxTokenizer::from_file(&dir.join("tokenizer.json"))?;
        let cangjie = CangjieConverter::load(&dir.join("Cangjie5_TC.json"))?;

        // Drive the KV plumbing from the ACTUAL input names — layer count and the
        // presence of `position_ids` vary between exports.
        let input_names: Vec<String> = lm.inputs().iter().map(|i| i.name().to_string()).collect();
        let lm_past_names = kv_input_names(&input_names);
        if lm_past_names.is_empty() {
            return Err("language_model has no past_key_values inputs".into());
        }
        let lm_past_shapes = lm_past_names
            .iter()
            .map(|name| empty_kv_shape(&lm, name))
            .collect::<Result<Vec<_>, _>>()?;
        let output_names: Vec<String> = lm.outputs().iter().map(|o| o.name().to_string()).collect();
        let lm_present_indices = lm_past_names
            .iter()
            .map(|name| present_output_index(name, &output_names))
            .collect::<Result<Vec<_>, _>>()?;
        let lm_logits_index = named_output_index("logits", &output_names)?;
        Ok(Self {
            dir,
            embed,
            lm,
            decoder,
            tokenizer,
            cangjie,
            voices: HashMap::new(),
            lm_past_names,
            lm_past_shapes,
            lm_present_indices,
            lm_logits_index,
            provider,
        })
    }

    /// One graph session by registry file-name prefix (`<prefix>*.onnx`), from FILE so
    /// its external `.onnx_data` resolves (see module doc).
    fn session(
        sessions: &mut crate::ort_session::OrtSessions,
        dir: &std::path::Path,
        prefix: &str,
    ) -> Result<Session, String> {
        let file = ds_model::tts_ort_asset_set(ds_config::TtsModel::Chatterbox)
            .files
            .iter()
            .find(|d| d.file_name.starts_with(prefix) && d.file_name.ends_with(".onnx"))
            .ok_or_else(|| format!("no `{prefix}` graph in the chatterbox registry"))?;
        sessions.load_file(&dir.join(file.file_name))
    }

    /// Voice conditioning from cache, else run the TRANSIENT speech encoder on the
    /// voice's pinned 24 kHz reference clip and cache the four outputs.
    fn ensure_voice(&mut self, voice: &str) -> Result<Arc<VoiceConditioning>, String> {
        if let Some(c) = self.voices.get(voice) {
            return Ok(c.clone());
        }
        if voice != "default" {
            return Err(format!("unknown chatterbox voice '{voice}'"));
        }
        let wav_path =
            ds_model::tts_model_file_path(ds_config::TtsModel::Chatterbox, "default_voice.wav")
                .ok_or("cannot resolve chatterbox voice path")?;
        let (rate, pcm) = crate::wav::read_wav_mono_f32(&wav_path)?;
        if rate != crate::SAMPLE_RATE {
            return Err(format!(
                "reference voice '{voice}' is {rate} Hz; need {}",
                crate::SAMPLE_RATE
            ));
        }
        if pcm.is_empty() {
            return Err(format!("reference voice '{voice}' is empty"));
        }
        let mut sessions = crate::ort_session::OrtSessions::from_realized(
            ds_config::TtsModel::Chatterbox,
            self.provider,
        );
        let mut encoder = Self::session(&mut sessions, &self.dir, "speech_encoder")?;
        // Resolve the four outputs by declared name (like the LM's logits/present
        // resolution) so exporter output ordering cannot swap conditioning tensors.
        let output_names: Vec<String> = encoder
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();
        let [
            audio_features_index,
            audio_tokens_index,
            speaker_embeddings_index,
            speaker_features_index,
        ] = speech_encoder_output_indices(&output_names)?;
        let n = pcm.len();
        let audio = Tensor::from_array((vec![1i64, n as i64], pcm))
            .map_err(|e| format!("audio tensor: {e}"))?;
        let outputs = encoder
            .run(ort::inputs! { "audio_values" => audio })
            .map_err(|e| format!("speech_encoder run: {e}"))?;
        let audio_features = extract_f32(&outputs[audio_features_index], "audio_features")?;
        let audio_tokens = extract_i64(&outputs[audio_tokens_index], "audio_tokens")?;
        let speaker_embeddings =
            extract_f32(&outputs[speaker_embeddings_index], "speaker_embeddings")?;
        let speaker_features = extract_f32(&outputs[speaker_features_index], "speaker_features")?;
        drop(outputs);
        drop(encoder); // transient: free ~230 MB of encoder weights
        let cond = Arc::new(VoiceConditioning {
            audio_features,
            audio_tokens,
            speaker_embeddings,
            speaker_features,
        });
        self.voices.insert(voice.to_string(), cond.clone());
        Ok(cond)
    }

    fn run_embed(
        &mut self,
        ids: &[i64],
        positions: &[i64],
        exaggeration: f32,
    ) -> Result<TensorData<f32>, String> {
        let t = Tensor::from_array((vec![1i64, ids.len() as i64], ids.to_vec()))
            .map_err(|e| format!("input_ids tensor: {e}"))?;
        let outputs = self
            .embed
            .run(ort::inputs! {
                "input_ids" => t,
                "position_ids" => Tensor::from_array((
                    vec![1i64, positions.len() as i64],
                    positions.to_vec(),
                )).map_err(|e| format!("position_ids tensor: {e}"))?,
                "exaggeration" => Tensor::from_array((vec![1i64], vec![exaggeration]))
                    .map_err(|e| format!("exaggeration tensor: {e}"))?,
            })
            .map_err(|e| format!("embed_tokens run: {e}"))?;
        extract_f32(&outputs[0], "inputs_embeds")
    }

    /// One text chunk → trimmed 24 kHz mono PCM. `cancelled` is polled EVERY token (a
    /// chunk can take ~10 s+ on CPU; barge-in must not wait it out) — on cancel this
    /// returns an empty Ok the caller's own post-inference cancel check discards.
    pub fn synthesize(
        &mut self,
        text: &str,
        voice: &str,
        language: &str,
        params: &ds_config::ResolvedTtsParams,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        if cancelled() {
            return Ok(Vec::new());
        }
        let cond = self.ensure_voice(voice)?;
        if cancelled() {
            return Ok(Vec::new());
        }
        let exaggeration = params.float(ds_config::TtsModel::Chatterbox, "exaggeration");
        let text = prepare_language(text, language, &self.cangjie);
        let ids = self.tokenizer.encode_ids(&text)?;
        if ids.is_empty() {
            return Err("chatterbox tokenizer produced no tokens".into());
        }
        let initial_positions: Vec<i64> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                if *id >= START_SPEECH_TOKEN {
                    0
                } else {
                    index as i64 - 1
                }
            })
            .collect();
        let text_embeds = self.run_embed(&ids, &initial_positions, exaggeration)?;

        // LM prefix: conditioning embeddings ++ text embeddings along the seq axis.
        let d = *text_embeds.shape.get(2).ok_or("inputs_embeds not 3-D")? as usize;
        if cond.audio_features.shape.get(2) != Some(&(d as i64)) {
            return Err("audio_features / text embedding dim mismatch".into());
        }
        let prefix_len = (cond.audio_features.shape[1] + text_embeds.shape[1]) as usize;
        let mut inputs_embeds = TensorData {
            shape: vec![1, prefix_len as i64, d as i64],
            data: {
                let mut v = Vec::with_capacity(prefix_len * d);
                v.extend_from_slice(&cond.audio_features.data);
                v.extend_from_slice(&text_embeds.data);
                v
            },
        };

        let mut past: Vec<TensorData<f32>> = self
            .lm_past_shapes
            .iter()
            .map(|shape| TensorData {
                shape: shape.clone(),
                data: Vec::new(),
            })
            .collect();
        let mut generated: Vec<i64> = vec![START_SPEECH_TOKEN];
        let mut stopped = false;

        // `attn_len` = prefix + tokens fed back so far (one per completed step).
        for attn_len in (prefix_len..).take(MAX_NEW_TOKENS) {
            if cancelled() {
                return Ok(Vec::new());
            }
            let mut feed: Vec<(String, SessionInputValue)> = Vec::new();
            feed.push((
                "inputs_embeds".to_string(),
                lm_input(&self.lm, "inputs_embeds", &inputs_embeds)?,
            ));
            let mask = Tensor::from_array((vec![1i64, attn_len as i64], vec![1i64; attn_len]))
                .map_err(|e| format!("attention_mask tensor: {e}"))?;
            feed.push(("attention_mask".to_string(), mask.into()));
            for (name, kv) in self.lm_past_names.iter().zip(&past) {
                feed.push((name.clone(), lm_input(&self.lm, name, kv)?));
            }
            let outputs = self
                .lm
                .run(feed)
                .map_err(|e| format!("language_model run: {e}"))?;

            // logits [1, seq, vocab] → copy only the last row.
            let mut row = extract_last_f32_row(&outputs[self.lm_logits_index], "logits")?;
            apply_repetition_penalty(&mut row, &generated, REPETITION_PENALTY);
            let next = argmax(&row) as i64;
            generated.push(next);
            if next == STOP_SPEECH_TOKEN {
                stopped = true;
                break;
            }

            // Feed back: presents → past (positionally after logits), grow the mask,
            // advance the single next position, embed the new token.
            let mut presents = Vec::with_capacity(self.lm_past_names.len());
            for index in &self.lm_present_indices {
                presents.push(extract_f32(&outputs[*index], "present kv")?);
            }
            drop(outputs);
            past = presents;
            inputs_embeds =
                self.run_embed(&[next], &[(generated.len() - 1) as i64], exaggeration)?;
        }
        if !stopped {
            log::warn!(
                target: "tts",
                "chatterbox exhausted MAX_NEW_TOKENS ({MAX_NEW_TOKENS}) without STOP; audio tail may be truncated"
            );
        }

        let speech = speech_token_window(&generated);
        if speech.is_empty() {
            return Err("chatterbox generated no speech tokens".into());
        }
        if cancelled() {
            return Ok(Vec::new());
        }

        // Decode: prompt tokens followed by generated speech.
        let mut tokens: Vec<i64> = Vec::with_capacity(cond.audio_tokens.data.len() + speech.len());
        tokens.extend_from_slice(&cond.audio_tokens.data);
        tokens.extend_from_slice(speech);
        let tokens_t = Tensor::from_array((vec![1i64, tokens.len() as i64], tokens))
            .map_err(|e| format!("speech_tokens tensor: {e}"))?;
        let outputs = self
            .decoder
            .run(ort::inputs! {
                "speech_tokens" => tokens_t,
                "speaker_embeddings" => cond.speaker_embeddings.to_tensor()?,
                "speaker_features" => cond.speaker_features.to_tensor()?,
            })
            .map_err(|e| format!("conditional_decoder run: {e}"))?;
        let wav = extract_f32(&outputs[0], "wav")?;
        Ok(crate::trim::trim_silence(&wav.data))
    }
}

/// Build one language_model input from f32 host data, narrowed to float16 when the graph
/// declares that input as f16. The pinned FP16 export mixes dtypes — the embeddings are
/// float16 while the KV cache stays float32 — so each input must match its own declaration
/// rather than assume one dtype for the whole feed.
fn lm_input(
    session: &Session,
    name: &str,
    data: &TensorData<f32>,
) -> Result<SessionInputValue<'static>, String> {
    let wants_f16 = session.inputs().iter().any(|input| {
        input.name() == name
            && matches!(
                input.dtype(),
                ValueType::Tensor {
                    ty: TensorElementType::Float16,
                    ..
                }
            )
    });
    if wants_f16 {
        Ok(data.to_tensor_f16()?.into())
    } else {
        Ok(data.to_tensor()?.into())
    }
}

/// Extract a float output as f32, accepting either an f32 or an f16 tensor — the FP16
/// language_model emits float16 logits and KV, while the encoder/decoder emit float32.
fn extract_f32(v: &ort::value::DynValue, what: &str) -> Result<TensorData<f32>, String> {
    if let Ok((shape, data)) = v.try_extract_tensor::<f32>() {
        return Ok(TensorData {
            shape: shape.to_vec(),
            data: data.to_vec(),
        });
    }
    let (shape, data) = v
        .try_extract_tensor::<f16>()
        .map_err(|e| format!("extract {what}: {e}"))?;
    Ok(TensorData {
        shape: shape.to_vec(),
        data: data.iter().map(|h| h.to_f32()).collect(),
    })
}

fn extract_i64(v: &ort::value::DynValue, what: &str) -> Result<TensorData<i64>, String> {
    let (shape, data) = v
        .try_extract_tensor::<i64>()
        .map_err(|e| format!("extract {what}: {e}"))?;
    Ok(TensorData {
        shape: shape.to_vec(),
        data: data.to_vec(),
    })
}

fn extract_last_f32_row(v: &ort::value::DynValue, what: &str) -> Result<Vec<f32>, String> {
    let tensor = extract_f32(v, what)?;
    let width = tensor
        .shape
        .last()
        .copied()
        .filter(|width| *width > 0)
        .ok_or_else(|| format!("{what} has no row width"))? as usize;
    let data = &tensor.data;
    data.get(data.len().saturating_sub(width)..)
        .filter(|row| row.len() == width)
        .map(<[f32]>::to_vec)
        .ok_or_else(|| format!("{what} produced no complete row"))
}

/// Reference `RepetitionPenaltyLogitsProcessor`: scale each id once from the original score
/// (`*p` if negative, `/p` otherwise); duplicates never compound.
pub(crate) fn apply_repetition_penalty(logits: &mut [f32], generated: &[i64], penalty: f32) {
    let original = logits.to_vec();
    for &id in generated {
        let Some(v) = original.get(id as usize).copied() else {
            continue;
        };
        logits[id as usize] = if v < 0.0 { v * penalty } else { v / penalty };
    }
}

/// First-max wins ties (numpy).
pub(crate) fn argmax(row: &[f32]) -> usize {
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in row.iter().enumerate() {
        if v > best_v {
            best = i;
            best_v = v;
        }
    }
    best
}

/// `past_key_values.*` input names, in the given (session-declared) order.
pub(crate) fn kv_input_names(input_names: &[String]) -> Vec<String> {
    input_names
        .iter()
        .filter(|n| n.contains("past_key_values"))
        .cloned()
        .collect()
}

fn empty_kv_shape(session: &Session, name: &str) -> Result<Vec<i64>, String> {
    let input = session
        .inputs()
        .iter()
        .find(|input| input.name() == name)
        .ok_or_else(|| format!("missing KV input metadata for {name}"))?;
    let ValueType::Tensor { shape, .. } = input.dtype() else {
        return Err(format!("KV input {name} is not a tensor"));
    };
    if shape.len() != 4 {
        return Err(format!("KV input {name} is not rank 4"));
    }
    let mut dims = shape.to_vec();
    for (axis, dim) in dims.iter_mut().enumerate() {
        if *dim < 0 {
            *dim = match axis {
                0 => 1,
                2 => 0,
                _ => return Err(format!("KV input {name} has dynamic axis {axis}")),
            };
        }
    }
    dims[2] = 0;
    Ok(dims)
}

fn present_output_index(past_name: &str, outputs: &[String]) -> Result<usize, String> {
    let suffix = past_name
        .strip_prefix("past_key_values.")
        .unwrap_or(past_name);
    outputs
        .iter()
        .position(|output| {
            output
                .strip_prefix("present_key_values.")
                .or_else(|| output.strip_prefix("present."))
                .is_some_and(|candidate| candidate == suffix)
        })
        .ok_or_else(|| format!("no present output for {past_name}"))
}

fn named_output_index(name: &str, outputs: &[String]) -> Result<usize, String> {
    outputs
        .iter()
        .position(|output| output == name)
        .ok_or_else(|| format!("no {name} output"))
}

fn speech_encoder_output_indices(outputs: &[String]) -> Result<[usize; 4], String> {
    Ok([
        named_output_index("audio_features", outputs)?,
        named_output_index("audio_tokens", outputs)?,
        named_output_index("speaker_embeddings", outputs)?,
        named_output_index("speaker_features", outputs)?,
    ])
}

/// The decodable window of a finished generation: strip the leading START and the
/// trailing STOP (when the loop stopped on it rather than the token budget).
pub(crate) fn speech_token_window(generated: &[i64]) -> &[i64] {
    let body = generated
        .strip_prefix(&[START_SPEECH_TOKEN])
        .unwrap_or(generated);
    body.strip_suffix(&[STOP_SPEECH_TOKEN]).unwrap_or(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_frontend_handles_korean_and_chinese_contracts() {
        assert_eq!(decompose_hangul("한"), "한");
        let mut cangjie = CangjieConverter::default();
        cangjie.word_to_code.insert('日', "A".to_string());
        cangjie.code_to_words.insert("A".to_string(), vec!['日']);
        assert_eq!(prepare_language("日", "zh", &cangjie), "[zh][cj_A][cj_.]");
        assert_eq!(prepare_language("hello", "en", &cangjie), "[en]hello");
    }

    #[test]
    fn repetition_penalty_matches_the_reference_and_never_compounds() {
        let mut logits = vec![1.0f32, -1.0, 2.0, 0.0];
        // id 1 appears twice: applied once, from the ORIGINAL value.
        apply_repetition_penalty(&mut logits, &[1, 1, 2], 1.2);
        assert!((logits[0] - 1.0).abs() < 1e-6, "untouched id");
        assert!((logits[1] - (-1.2)).abs() < 1e-6, "negative × penalty");
        assert!((logits[2] - (2.0 / 1.2)).abs() < 1e-6, "positive ÷ penalty");
        assert_eq!(logits[3], 0.0);
        // Out-of-range ids (e.g. START on a truncated fixture row) are ignored.
        let mut small = vec![1.0f32];
        apply_repetition_penalty(&mut small, &[START_SPEECH_TOKEN], 1.2);
        assert_eq!(small, vec![1.0]);
    }

    #[test]
    fn argmax_takes_the_first_maximum() {
        assert_eq!(argmax(&[0.0, 3.0, 3.0, 1.0]), 1);
        assert_eq!(argmax(&[-5.0, -2.0]), 1);
        assert_eq!(argmax(&[f32::NEG_INFINITY, -1.0]), 1);
    }

    #[test]
    fn kv_names_enumerate_in_declared_order() {
        let names: Vec<String> = [
            "inputs_embeds",
            "attention_mask",
            "position_ids",
            "past_key_values.0.key",
            "past_key_values.0.value",
            "past_key_values.11.key",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            kv_input_names(&names),
            vec![
                "past_key_values.0.key".to_string(),
                "past_key_values.0.value".to_string(),
                "past_key_values.11.key".to_string(),
            ]
        );
    }

    #[test]
    fn present_outputs_pair_by_layer_and_key_name() {
        let outputs = vec![
            "logits".to_string(),
            "present.1.value".to_string(),
            "present.0.key".to_string(),
        ];
        assert_eq!(
            present_output_index("past_key_values.0.key", &outputs).unwrap(),
            2
        );
        assert_eq!(
            present_output_index("past_key_values.1.value", &outputs).unwrap(),
            1
        );
        assert_eq!(named_output_index("logits", &outputs).unwrap(), 0);
    }

    #[test]
    fn speech_encoder_outputs_match_the_pinned_export() {
        let outputs = vec![
            "audio_tokens".to_string(),
            "speaker_features".to_string(),
            "audio_features".to_string(),
            "speaker_embeddings".to_string(),
        ];
        assert_eq!(
            speech_encoder_output_indices(&outputs).unwrap(),
            [2, 0, 3, 1]
        );
    }

    #[test]
    fn speech_token_window_strips_start_and_trailing_stop() {
        // Stopped on STOP: both sentinels stripped.
        assert_eq!(
            speech_token_window(&[START_SPEECH_TOKEN, 7, 8, STOP_SPEECH_TOKEN]),
            &[7, 8]
        );
        // Hit the token budget (no STOP): the full tail is kept — the reference's
        // unconditional `[1:-1]` would silently drop a real token here.
        assert_eq!(speech_token_window(&[START_SPEECH_TOKEN, 7, 8]), &[7, 8]);
        // Degenerate: nothing generated.
        assert!(speech_token_window(&[START_SPEECH_TOKEN, STOP_SPEECH_TOKEN]).is_empty());
    }
}
