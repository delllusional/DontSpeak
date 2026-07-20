//! OmniVoice int4 ONNX inference with confidence-weighted iterative unmasking.

use half::f16;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};

/// Extract a float output as f32, accepting either f32 or f16 — the FP16 audio sub-models
/// (embeddings/heads/decoder) emit float16 while the int4 LLM path emits float32.
fn extract_floats(value: &ort::value::DynValue, label: &str) -> Result<Vec<f32>, String> {
    if let Ok((_, data)) = value.try_extract_tensor::<f32>() {
        return Ok(data.to_vec());
    }
    let (_, data) = value
        .try_extract_tensor::<f16>()
        .map_err(|error| format!("{label}: {error}"))?;
    Ok(data.iter().map(|half| half.to_f32()).collect())
}

/// Build a float model input from f32 host data (possibly empty, for a zero-length KV
/// cache), narrowed to float16 when the graph declares that input as f16.
fn float_input(
    session: &Session,
    name: &str,
    shape: Vec<i64>,
    data: Vec<f32>,
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
    let alloc = ort::memory::Allocator::default();
    if wants_f16 {
        if data.is_empty() {
            return Tensor::<f16>::new(&alloc, shape)
                .map(Into::into)
                .map_err(|error| format!("{name} empty f16: {error}"));
        }
        let data: Vec<f16> = data.iter().map(|&x| f16::from_f32(x)).collect();
        Tensor::from_array((shape, data))
            .map(Into::into)
            .map_err(|error| format!("{name} f16 tensor: {error}"))
    } else if data.is_empty() {
        Tensor::<f32>::new(&alloc, shape)
            .map(Into::into)
            .map_err(|error| format!("{name} empty f32: {error}"))
    } else {
        Tensor::from_array((shape, data))
            .map(Into::into)
            .map_err(|error| format!("{name} tensor: {error}"))
    }
}

const CODEBOOKS: usize = 8;
const CODEBOOK_SIZE: usize = 1024;
const MASK_TOKEN: i64 = 1024;
/// Generation frame budget per model run; text is pre-split to fit it
/// (`split_for_frame_budget`) because clamping the estimate truncates audio.
const MAX_FRAMES: usize = 600;
const STEPS: usize = 32;
const CONFIDENCE_WEIGHTS: [f32; CODEBOOKS] = [8.0, 8.0, 6.0, 6.0, 4.0, 4.0, 2.0, 2.0];

pub struct OmniVoiceSynth {
    embeddings: Session,
    llm: Session,
    heads: Session,
    decoder: Session,
    tokenizer: tokenizers::Tokenizer,
    llm_past: Vec<(String, Vec<i64>)>,
    hidden_output: usize,
    provider: ds_config::RealizedProvider,
}

impl OmniVoiceSynth {
    pub fn load() -> Result<Self, String> {
        crate::ort_session::load_with_fallback("omnivoice", Self::load_with_provider)
    }

    pub fn load_with_provider(preference: &str) -> Result<Self, String> {
        let dir = ds_model::tts_model_dir(ds_config::TtsModel::OmniVoice)
            .ok_or("cannot resolve model_dir()")?;
        let mut sessions = crate::ort_session::OrtSessions::from_preference(
            ds_config::TtsModel::OmniVoice,
            preference,
        );
        let embeddings = sessions.load_file(&dir.join("audio_embeddings_encoder.onnx"))?;
        let llm = sessions.load_file(&dir.join("llm_decoder.onnx"))?;
        let heads = sessions.load_file(&dir.join("audio_heads_decoder.onnx"))?;
        let decoder = sessions.load_file(&dir.join("higgs_decoder.onnx"))?;
        let provider = sessions.provider();
        let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|error| format!("omnivoice tokenizer load: {error}"))?;
        let llm_past = llm
            .inputs()
            .iter()
            .filter(|input| input.name().contains("past"))
            .map(|input| Ok((input.name().to_string(), empty_cache_shape(input)?)))
            .collect::<Result<Vec<_>, String>>()?;
        let hidden_output = output_index(&llm, "hidden_states")?;
        Ok(Self {
            embeddings,
            llm,
            heads,
            decoder,
            tokenizer,
            llm_past,
            hidden_output,
            provider,
        })
    }

    pub fn provider(&self) -> ds_config::RealizedProvider {
        self.provider
    }

    pub fn synthesize(
        &mut self,
        text: &str,
        _language: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        let mut waveform = Vec::new();
        for piece in split_for_frame_budget(text, MAX_FRAMES) {
            if cancelled() {
                return Ok(Vec::new());
            }
            waveform.extend(self.synthesize_piece(&piece, cancelled)?);
        }
        Ok(waveform)
    }

    fn synthesize_piece(
        &mut self,
        text: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        let encoded = self
            .tokenizer
            .encode(text, true)
            .map_err(|error| format!("omnivoice tokenize: {error}"))?;
        let text_tokens: Vec<i64> = encoded.get_ids().iter().map(|id| i64::from(*id)).collect();
        if text_tokens.is_empty() {
            return Err("omnivoice tokenizer produced no tokens".to_string());
        }
        let estimate = estimate_audio_frames(text);
        if estimate > MAX_FRAMES {
            // Backstop only: split_for_frame_budget should make this unreachable.
            log::warn!(
                target: "tts",
                "omnivoice frame estimate {estimate} exceeds {MAX_FRAMES} after splitting; audio will truncate"
            );
        }
        let frames = estimate.clamp(10, MAX_FRAMES);
        let sequence = text_tokens.len() + frames;
        let mut ids = vec![0i64; CODEBOOKS * sequence];
        for codebook in 0..CODEBOOKS {
            let row = &mut ids[codebook * sequence..(codebook + 1) * sequence];
            row[..text_tokens.len()].copy_from_slice(&text_tokens);
            row[text_tokens.len()..].fill(MASK_TOKEN);
        }
        let mut audio_mask = vec![false; sequence];
        audio_mask[text_tokens.len()..].fill(true);
        let mut masked = frames;

        for step in 0..STEPS {
            if cancelled() {
                return Ok(Vec::new());
            }
            if masked == 0 {
                break;
            }
            let logits = self.run_step(&ids, &audio_mask, sequence)?;
            let mut confidence = vec![0.0f32; frames];
            for (codebook, weight) in CONFIDENCE_WEIGHTS.iter().copied().enumerate() {
                let weight = weight / 40.0;
                for (frame, confidence) in confidence.iter_mut().enumerate() {
                    let offset =
                        (codebook * sequence + text_tokens.len() + frame) * (CODEBOOK_SIZE + 1);
                    *confidence += weight * max_softmax(&logits[offset..offset + CODEBOOK_SIZE]);
                }
            }
            let mut candidates: Vec<usize> = (0..frames)
                .filter(|frame| ids[text_tokens.len() + frame] == MASK_TOKEN)
                .collect();
            candidates.sort_by(|left, right| {
                confidence[*right]
                    .partial_cmp(&confidence[*left])
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.cmp(right))
            });
            let remaining_steps = (STEPS - step).max(1);
            let count = candidates.len().div_ceil(remaining_steps).max(1);
            for frame in candidates.into_iter().take(count) {
                for codebook in 0..CODEBOOKS {
                    let offset =
                        (codebook * sequence + text_tokens.len() + frame) * (CODEBOOK_SIZE + 1);
                    ids[codebook * sequence + text_tokens.len() + frame] =
                        argmax(&logits[offset..offset + CODEBOOK_SIZE]) as i64;
                }
                masked -= 1;
            }
        }
        if masked != 0 {
            return Err("omnivoice left masked audio frames".to_string());
        }
        if cancelled() {
            return Ok(Vec::new());
        }
        let mut codes = Vec::with_capacity(CODEBOOKS * frames);
        for codebook in 0..CODEBOOKS {
            let start = codebook * sequence + text_tokens.len();
            codes.extend_from_slice(&ids[start..start + frames]);
        }
        let tensor = Tensor::from_array((vec![CODEBOOKS as i64, 1, frames as i64], codes))
            .map_err(|error| format!("omnivoice codes tensor: {error}"))?;
        let outputs = self
            .decoder
            .run(ort::inputs! { "codes" => tensor })
            .map_err(|error| format!("omnivoice decoder run: {error}"))?;
        let waveform = extract_floats(&outputs[0], "omnivoice waveform")?;
        Ok(crate::trim::trim_silence(&waveform))
    }

    fn run_step(
        &mut self,
        ids: &[i64],
        audio_mask: &[bool],
        sequence: usize,
    ) -> Result<Vec<f32>, String> {
        let ids = Tensor::from_array((vec![1, CODEBOOKS as i64, sequence as i64], ids.to_vec()))
            .map_err(|error| format!("omnivoice input tensor: {error}"))?;
        let mask = Tensor::from_array((vec![1, sequence as i64], audio_mask.to_vec()))
            .map_err(|error| format!("omnivoice audio mask: {error}"))?;
        let embedded = self
            .embeddings
            .run(ort::inputs! { "input_ids" => ids, "audio_mask" => mask })
            .map_err(|error| format!("omnivoice embeddings run: {error}"))?;
        let embedded = extract_floats(&embedded[0], "omnivoice embeddings")?;
        let embeds = float_input(
            &self.llm,
            "inputs_embeds",
            vec![1, sequence as i64, 1024],
            embedded,
        )?;
        let mut feed: Vec<(String, SessionInputValue)> = vec![("inputs_embeds".into(), embeds)];
        let input_names: Vec<&str> = self.llm.inputs().iter().map(|input| input.name()).collect();
        if input_names.contains(&"attention_mask") {
            let attention = Tensor::from_array((vec![1, sequence as i64], vec![1i64; sequence]))
                .map_err(|error| format!("omnivoice attention mask: {error}"))?;
            feed.push(("attention_mask".into(), attention.into()));
        }
        if input_names.contains(&"position_ids") {
            let positions: Vec<i64> = (0..sequence as i64).collect();
            let positions = Tensor::from_array((vec![1, sequence as i64], positions))
                .map_err(|error| format!("omnivoice positions: {error}"))?;
            feed.push(("position_ids".into(), positions.into()));
        }
        for (name, shape) in &self.llm_past {
            let tensor = float_input(&self.llm, name, shape.clone(), Vec::new())?;
            feed.push((name.clone(), tensor));
        }
        let hidden = self
            .llm
            .run(feed)
            .map_err(|error| format!("omnivoice LLM run: {error}"))?;
        let hidden = extract_floats(&hidden[self.hidden_output], "omnivoice hidden states")?;
        let hidden = float_input(
            &self.heads,
            "hidden_states",
            vec![1, sequence as i64, 1024],
            hidden,
        )?;
        let logits = self
            .heads
            .run(vec![("hidden_states".to_string(), hidden)])
            .map_err(|error| format!("omnivoice heads run: {error}"))?;
        let logits = extract_floats(&logits[0], "omnivoice logits")?;
        // The unmasking loop slices by fixed offsets; a shape drift must fail here,
        // not panic there.
        let expected = CODEBOOKS * sequence * (CODEBOOK_SIZE + 1);
        if logits.len() != expected {
            return Err(format!(
                "omnivoice heads logits length {} != {expected}",
                logits.len()
            ));
        }
        Ok(logits.to_vec())
    }
}

fn output_index(session: &Session, name: &str) -> Result<usize, String> {
    session
        .outputs()
        .iter()
        .position(|output| output.name() == name)
        .ok_or_else(|| format!("model has no `{name}` output"))
}

fn empty_cache_shape(input: &ort::value::Outlet) -> Result<Vec<i64>, String> {
    let ValueType::Tensor { shape, .. } = input.dtype() else {
        return Err(format!("{} is not a tensor", input.name()));
    };
    let mut shape = shape.to_vec();
    for (axis, dim) in shape.iter_mut().enumerate() {
        if *dim < 0 {
            *dim = if axis == 0 {
                1
            } else if axis == 2 {
                0
            } else {
                8
            };
        }
    }
    if shape.len() >= 3 {
        shape[2] = 0;
    }
    Ok(shape)
}

fn argmax(values: &[f32]) -> usize {
    let mut best_index = 0;
    let mut best_value = f32::NEG_INFINITY;
    for (index, value) in values.iter().copied().enumerate() {
        if value > best_value {
            best_index = index;
            best_value = value;
        }
    }
    best_index
}

fn max_softmax(values: &[f32]) -> f32 {
    let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    let mut best = 0.0f32;
    for value in values {
        let probability = (*value - max).exp();
        sum += probability;
        best = best.max(probability);
    }
    if sum > 0.0 { best / sum } else { 0.0 }
}

/// Recursive char-boundary split (whitespace nearest the midpoint preferred) until
/// every piece's [`estimate_audio_frames`] fits `budget`.
fn split_for_frame_budget(text: &str, budget: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.len() < 2 || estimate_audio_frames(text) <= budget {
        return vec![text.to_string()];
    }
    let mid = chars.len() / 2;
    let cut = (1..chars.len())
        .filter(|&index| chars[index].1.is_whitespace())
        .min_by_key(|&index| index.abs_diff(mid))
        .unwrap_or(mid);
    let (head, tail) = text.split_at(chars[cut].0);
    let mut pieces = split_for_frame_budget(head, budget);
    pieces.extend(split_for_frame_budget(tail, budget));
    pieces
}

fn estimate_audio_frames(text: &str) -> usize {
    let units: f32 = text
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                0.25
            } else if character.is_ascii_punctuation() {
                0.35
            } else if character.is_ascii() {
                1.0
            } else {
                1.8
            }
        })
        .sum();
    if units == 0.0 {
        return 0;
    }
    let ratio = (units / 17.0).max(0.1);
    let boosted = if ratio < 2.0 { ratio.cbrt() } else { ratio };
    (25.0 * boosted * 1.15).ceil() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argmax_and_softmax_are_deterministic() {
        assert_eq!(argmax(&[1.0, 3.0, 3.0]), 1);
        assert!(max_softmax(&[0.0, 4.0]) > 0.98);
    }

    #[test]
    fn duration_estimate_scales_and_is_bounded_by_the_caller() {
        assert!(
            estimate_audio_frames("This is a substantially longer sentence.")
                > estimate_audio_frames("Hi.")
        );
        assert_eq!(estimate_audio_frames("").clamp(10, MAX_FRAMES), 10);
    }

    fn non_whitespace(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn cjk_text_splits_to_fit_the_frame_budget() {
        // Over-budget CJK must split (no silent clamp).
        let text = "你".repeat(300);
        assert!(estimate_audio_frames(&text) > MAX_FRAMES);
        let pieces = split_for_frame_budget(&text, MAX_FRAMES);
        assert!(pieces.len() > 1, "must split: {}", pieces.len());
        for piece in &pieces {
            assert!(estimate_audio_frames(piece) <= MAX_FRAMES);
        }
        assert_eq!(non_whitespace(&pieces.concat()), non_whitespace(&text));
    }

    #[test]
    fn cyrillic_prose_splits_at_whitespace_and_loses_nothing() {
        let text = vec!["привет мир"; 40].join(" ");
        assert!(estimate_audio_frames(&text) > MAX_FRAMES);
        let pieces = split_for_frame_budget(&text, MAX_FRAMES);
        assert!(pieces.len() > 1);
        for piece in &pieces {
            assert!(estimate_audio_frames(piece) <= MAX_FRAMES);
            assert!(!piece.starts_with(' ') && !piece.ends_with(' '));
            assert!(
                piece
                    .split_whitespace()
                    .all(|w| w == "привет" || w == "мир"),
                "no mid-word cut: {piece:?}"
            );
        }
        assert_eq!(non_whitespace(&pieces.concat()), non_whitespace(&text));
    }

    #[test]
    fn ascii_prose_passes_through_as_a_single_piece() {
        let text = "The quick brown fox jumps over the lazy dog.";
        assert_eq!(
            split_for_frame_budget(text, MAX_FRAMES),
            vec![text.to_string()]
        );
        assert!(split_for_frame_budget("   ", MAX_FRAMES).is_empty());
    }
}
