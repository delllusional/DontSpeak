//! CFG iterative unmasking for OmniVoice's bidirectional ONNX export.
//! Ported from `_generate_iterative` (k2-fsa/OmniVoice 468e927); formulas cite upstream.

use half::f16;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};

/// Extract f32 or f16 model output as f32.
fn extract_floats(value: &ort::value::DynValue, label: &str) -> Result<Vec<f32>, String> {
    if let Ok((_, data)) = value.try_extract_tensor::<f32>() {
        return Ok(data.to_vec());
    }
    let (_, data) = value
        .try_extract_tensor::<f16>()
        .map_err(|error| format!("{label}: {error}"))?;
    Ok(data.iter().map(|half| half.to_f32()).collect())
}

/// Match the graph's declared f32/f16 input type.
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
    if wants_f16 {
        let data: Vec<f16> = data.iter().map(|&x| f16::from_f32(x)).collect();
        Tensor::from_array((shape, data))
            .map(Into::into)
            .map_err(|error| format!("{name} f16 tensor: {error}"))
    } else {
        Tensor::from_array((shape, data))
            .map(Into::into)
            .map_err(|error| format!("{name} tensor: {error}"))
    }
}

const CODEBOOKS: usize = 8;
const CODEBOOK_SIZE: usize = 1024;
const MASK_TOKEN: i64 = 1024;
// Head logits append MASK after code entries.
const VOCAB: usize = CODEBOOK_SIZE + 1;
// Cap frames so TTFA stays bounded (`2 * steps` LLM forwards grow with seq length).
const MAX_FRAMES: usize = 200;
// Upstream guidance_scale (omnivoice.py:178): `w` in `c + w*(c - u)`.
const GUIDANCE_SCALE: f32 = 2.0;
// Upstream t_shift (omnivoice.py:179).
const T_SHIFT: f64 = 0.1;
// Upstream layer_penalty_factor (omnivoice.py:180, 1407).
const LAYER_PENALTY: f32 = 5.0;
// Upstream position_temperature (omnivoice.py:181, 1409, 1632). Noise on Gumbel, not scores.
const GUMBEL_SCALE: f32 = 5.0;

/// Upstream voice-design presets; test pins registry order. `default` omits ORT instruct.
pub const OMNIVOICE_PRESETS: &[(&str, &str)] = &[
    ("default", ""),
    ("young_woman", "female, young adult, moderate pitch"),
    ("young_man", "male, young adult, moderate pitch"),
    ("mature_woman", "female, middle-aged, moderate pitch"),
    ("mature_man", "male, middle-aged, low pitch"),
    (
        "british_woman",
        "female, middle-aged, moderate pitch, british accent",
    ),
    (
        "british_man",
        "male, middle-aged, moderate pitch, british accent",
    ),
    ("bright_woman", "female, young adult, high pitch"),
    ("deep_man", "male, middle-aged, very low pitch"),
    ("whisper", "female, young adult, whisper"),
];

fn preset_instruct(voice: &str) -> Option<&'static str> {
    OMNIVOICE_PRESETS
        .iter()
        .find(|(id, _)| *id == voice)
        .map(|(_, instruct)| *instruct)
}

/// MLX voice arg: preset → instruct (default → nil in shim); raw instructs pass through.
#[cfg(any(test, target_os = "macos"))]
pub(crate) fn mlx_voice_arg(voice: &str) -> &str {
    match preset_instruct(voice) {
        Some("") => "default",
        Some(instruct) => instruct,
        None if voice.is_empty() => "default",
        None => voice,
    }
}

pub struct OmniVoiceSynth {
    embeddings: Session,
    llm: Session,
    heads: Session,
    decoder: Session,
    tokenizer: tokenizers::Tokenizer,
    hidden_output: usize,
    /// Realized backbone EP (Higgs decoder may still run CPU).
    provider: ds_config::RealizedProvider,
}

/// #165: FP16 higgs_decoder NaNs on CUDA EP → decode on CPU; other EPs in place.
fn decoder_provider(backbone: ds_config::RealizedProvider) -> ds_config::RealizedProvider {
    if backbone == ds_config::RealizedProvider::Cuda {
        ds_config::RealizedProvider::Cpu
    } else {
        backbone
    }
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
        let llm = sessions.load_file(&dir.join("llm_backbone_fp32.onnx"))?;
        let inputs: Vec<(&str, ValueType)> = llm
            .inputs()
            .iter()
            .map(|input| (input.name(), input.dtype().clone()))
            .collect();
        let outputs: Vec<&str> = llm.outputs().iter().map(|output| output.name()).collect();
        check_llm_contract(&inputs, &outputs)?;
        let heads = sessions.load_file(&dir.join("audio_heads_decoder.onnx"))?;
        // Pin decoder EP so fallback cannot drift it.
        let provider = sessions.provider();
        let mut decoder_sessions = crate::ort_session::OrtSessions::from_realized(
            ds_config::TtsModel::OmniVoice,
            decoder_provider(provider),
        );
        let decoder = decoder_sessions.load_file(&dir.join("higgs_decoder.onnx"))?;
        let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|error| format!("omnivoice tokenizer load: {error}"))?;
        let hidden_output = output_index(&llm, "hidden_states")?;
        Ok(Self {
            embeddings,
            llm,
            heads,
            decoder,
            tokenizer,
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
        voice: &str,
        language: &str,
        params: &ds_config::ResolvedTtsParams,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        let model = ds_config::TtsModel::OmniVoice;
        let language = model.descriptor().runtime_language(language).to_string();
        let steps = params.int(model, "steps") as usize;
        // Unknown non-empty → raw instruct; empty omits the block.
        let instruct = preset_instruct(voice).unwrap_or(voice);
        // Seed from language+voice only so speaker holds across split pieces;
        // non-negative config seed overrides.
        let seed = u64::try_from(params.int(model, "seed"))
            .unwrap_or_else(|_| stable_seed(&language, instruct));
        let mut waveform = Vec::new();
        for piece in split_for_frame_budget(text, MAX_FRAMES) {
            if cancelled() {
                return Ok(Vec::new());
            }
            waveform.extend(
                self.synthesize_piece(&piece, &language, instruct, steps, seed, cancelled)?,
            );
        }
        Ok(waveform)
    }

    fn synthesize_piece(
        &mut self,
        text: &str,
        language: &str,
        instruct: &str,
        steps: usize,
        seed: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<f32>, String> {
        let estimate = estimate_audio_frames(text);
        if estimate > MAX_FRAMES {
            log::warn!(
                target: "tts",
                "omnivoice frame estimate {estimate} exceeds {MAX_FRAMES} after splitting; audio will truncate"
            );
        }
        let frames = estimate.clamp(10, MAX_FRAMES);
        let tokenizer = &self.tokenizer;
        let prompt = build_prompt(
            |segment| {
                tokenizer
                    .encode(segment, true)
                    .map(|encoded| encoded.get_ids().to_vec())
                    .map_err(|error| format!("omnivoice tokenize: {error}"))
            },
            language,
            instruct,
            text,
            frames,
        )?;
        let seq = prompt.seq();
        let cond_len = prompt.cond_len;
        let total = CODEBOOKS * frames;

        let mut cond_ids = prompt.ids.clone();
        // Uncond pass: target region only, audio_mask true (omnivoice.py:1342-1344).
        let uncond_mask = vec![true; frames];
        let mut remaining_mask = vec![true; total];
        let mut remaining = total;

        for (step, count) in schedule_counts(total, steps).into_iter().enumerate() {
            if cancelled() {
                return Ok(Vec::new());
            }
            if count == 0 || remaining == 0 {
                continue;
            }
            let cond_logits = self.run_pass(&cond_ids, &prompt.audio_mask, seq)?;
            if cancelled() {
                return Ok(Vec::new());
            }
            let mut uncond_ids = vec![0i64; total];
            for codebook in 0..CODEBOOKS {
                let start = codebook * seq + cond_len;
                uncond_ids[codebook * frames..(codebook + 1) * frames]
                    .copy_from_slice(&cond_ids[start..start + frames]);
            }
            let uncond_logits = self.run_pass(&uncond_ids, &uncond_mask, frames)?;

            // Cond/uncond have different seq lengths:
            //   cond (c,f)   = ((c*cond_seq) + cond_len + f) * VOCAB
            //   uncond (c,f) = ((c*frames) + f) * VOCAB
            let mut guided = vec![f32::NEG_INFINITY; total * VOCAB];
            for cell in 0..total {
                if !remaining_mask[cell] {
                    continue;
                }
                let codebook = cell / frames;
                let frame = cell % frames;
                let cond_offset = ((codebook * seq) + cond_len + frame) * VOCAB;
                let uncond_offset = ((codebook * frames) + frame) * VOCAB;
                let row = guided_logits(
                    &cond_logits[cond_offset..cond_offset + VOCAB],
                    &uncond_logits[uncond_offset..uncond_offset + VOCAB],
                );
                guided[cell * VOCAB..(cell + 1) * VOCAB].copy_from_slice(&row);
            }
            for (cell, token) in select_cells(
                &guided,
                &remaining_mask,
                count,
                seed.wrapping_add(step as u64),
            ) {
                let codebook = cell / frames;
                let frame = cell % frames;
                cond_ids[codebook * seq + cond_len + frame] = token;
                remaining_mask[cell] = false;
                remaining -= 1;
            }
        }

        // MASK_TOKEN is outside decoder codebook range.
        let mut codes = Vec::with_capacity(total);
        for codebook in 0..CODEBOOKS {
            let start = codebook * seq + cond_len;
            codes.extend_from_slice(&cond_ids[start..start + frames]);
        }
        if remaining != 0 || codes.contains(&MASK_TOKEN) {
            return Err("omnivoice left masked audio cells".to_string());
        }
        let diversity = codebook_diversity(&codes, frames);
        log::debug!(
            target: "tts",
            "omnivoice unique codes per codebook over {frames} frames: {diversity:?}"
        );
        if is_total_collapse(&diversity) {
            return Err(format!(
                "omnivoice decode collapsed: every codebook emitted a single code over {frames} frames"
            ));
        }
        if cancelled() {
            return Ok(Vec::new());
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

    fn run_pass(
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
        // Bidirectional export: rank-4 bool mask, true means attend.
        let attention = Tensor::from_array((
            vec![1, 1, sequence as i64, sequence as i64],
            vec![true; sequence * sequence],
        ))
        .map_err(|error| format!("omnivoice attention mask: {error}"))?;
        let feed: Vec<(String, SessionInputValue)> = vec![
            ("inputs_embeds".into(), embeds),
            ("attention_mask".into(), attention.into()),
        ];
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
        // Fail shape drift here before fixed-offset slicing in the unmasking loop.
        let expected = CODEBOOKS * sequence * VOCAB;
        if logits.len() != expected {
            return Err(format!(
                "omnivoice heads logits length {} != {expected}",
                logits.len()
            ));
        }
        Ok(logits)
    }
}

/// Fail closed unless the export has embeddings, a rank-4 bool mask, no KV cache,
/// and hidden-state output; other shapes can run but produce invalid audio.
fn check_llm_contract(inputs: &[(&str, ValueType)], outputs: &[&str]) -> Result<(), String> {
    let expected = "the pinned llm_backbone_fp32.onnx export declares \
                    inputs_embeds float[1,L,1024] + attention_mask bool[1,1,L,L] only";
    let mut saw_embeds = false;
    let mut saw_mask = false;
    for (name, dtype) in inputs {
        if name.starts_with("past") {
            return Err(format!(
                "omnivoice LLM has KV-cache input `{name}`; {expected} — re-download the model"
            ));
        }
        let ValueType::Tensor { ty, shape, .. } = dtype else {
            return Err(format!("omnivoice LLM input `{name}` is not a tensor"));
        };
        match *name {
            "inputs_embeds" => {
                let is_float =
                    matches!(ty, TensorElementType::Float32 | TensorElementType::Float16);
                if !is_float || shape.len() != 3 {
                    return Err(format!(
                        "omnivoice LLM inputs_embeds is {ty:?} rank-{}; {expected}",
                        shape.len()
                    ));
                }
                saw_embeds = true;
            }
            "attention_mask" => {
                if *ty != TensorElementType::Bool || shape.len() != 4 {
                    return Err(format!(
                        "omnivoice LLM attention_mask is {ty:?} rank-{}; {expected}",
                        shape.len()
                    ));
                }
                saw_mask = true;
            }
            other => {
                return Err(format!(
                    "omnivoice LLM has unexpected input `{other}`; {expected}"
                ));
            }
        }
    }
    if !saw_embeds || !saw_mask {
        return Err(format!(
            "omnivoice LLM is missing a required input; {expected}"
        ));
    }
    if !outputs.contains(&"hidden_states") {
        return Err(format!(
            "omnivoice LLM has no hidden_states output; {expected}"
        ));
    }
    Ok(())
}

/// Prompt tokens broadcast across codebooks, followed by masked target frames.
struct Prompt {
    ids: Vec<i64>,
    audio_mask: Vec<bool>,
    cond_len: usize,
    frames: usize,
}

impl Prompt {
    fn seq(&self) -> usize {
        self.cond_len + self.frames
    }
}

/// Mirror `_prepare_inference_inputs` (omnivoice.py:1220-1258): tokenize style and
/// text separately, then concatenate. Empty instruct omits its block.
fn build_prompt(
    encode: impl Fn(&str) -> Result<Vec<u32>, String>,
    language: &str,
    instruct: &str,
    text: &str,
    frames: usize,
) -> Result<Prompt, String> {
    let mut style = format!("<|lang_start|>{language}<|lang_end|>");
    if !instruct.is_empty() {
        style.push_str(&format!("<|instruct_start|>{instruct}<|instruct_end|>"));
    }
    let mut prompt: Vec<i64> = encode(&style)?.into_iter().map(i64::from).collect();
    let wrapped = format!("<|text_start|>{text}<|text_end|>");
    prompt.extend(encode(&wrapped)?.into_iter().map(i64::from));
    if prompt.is_empty() {
        return Err("omnivoice tokenizer produced no tokens".to_string());
    }
    let cond_len = prompt.len();
    let seq = cond_len + frames;
    let mut ids = vec![MASK_TOKEN; CODEBOOKS * seq];
    for codebook in 0..CODEBOOKS {
        ids[codebook * seq..codebook * seq + cond_len].copy_from_slice(&prompt);
    }
    let mut audio_mask = vec![false; seq];
    audio_mask[cond_len..].fill(true);
    Ok(Prompt {
        ids,
        audio_mask,
        cond_len,
        frames,
    })
}

/// Shifted upstream schedule (omnivoice.py:1356-1378, 1639-1648); the final step
/// consumes the remainder.
fn schedule_counts(total: usize, steps: usize) -> Vec<usize> {
    let shifted = |index: usize| -> f64 {
        let t = index as f64 / steps as f64;
        T_SHIFT * t / (1.0 + (T_SHIFT - 1.0) * t)
    };
    let mut remaining = total;
    let mut counts = Vec::with_capacity(steps);
    for step in 0..steps {
        let count = if step == steps - 1 {
            remaining
        } else {
            let fraction = shifted(step + 1) - shifted(step);
            (((total as f64) * fraction).ceil() as usize).min(remaining)
        };
        counts.push(count);
        remaining -= count;
    }
    counts
}

fn log_softmax(row: &[f32]) -> Vec<f32> {
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let log_sum = max
        + row
            .iter()
            .map(|value| (value - max).exp())
            .sum::<f32>()
            .ln();
    row.iter().map(|value| value - log_sum).collect()
}

/// Upstream classifier-free guidance (omnivoice.py:1430-1440), followed by masking
/// the audio-mask token.
fn guided_logits(cond: &[f32], uncond: &[f32]) -> Vec<f32> {
    let c_lp = log_softmax(cond);
    let u_lp = log_softmax(uncond);
    let combined: Vec<f32> = c_lp
        .iter()
        .zip(&u_lp)
        .map(|(c, u)| c + GUIDANCE_SCALE * (c - u))
        .collect();
    let mut guided = log_softmax(&combined);
    guided[MASK_TOKEN as usize] = f32::NEG_INFINITY;
    guided
}

/// Gumbel(0, 1), clamped to the open unit interval (omnivoice.py:1632-1636).
fn gumbel(rng: &mut fastrand::Rng) -> f32 {
    let u = rng.f64().clamp(1e-10, 1.0 - 1e-10);
    (-(-u.ln()).ln()) as f32
}

/// Rank masked cells by upstream confidence, codebook penalty, and Gumbel noise
/// (omnivoice.py:1407, 1448-1450). MASK is pinned to negative infinity.
fn select_cells(
    guided: &[f32],
    remaining_mask: &[bool],
    count: usize,
    seed: u64,
) -> Vec<(usize, i64)> {
    let cells = remaining_mask.len();
    debug_assert_eq!(guided.len(), cells * VOCAB);
    let frames = cells / CODEBOOKS;
    let mut rng = fastrand::Rng::with_seed(seed);
    let mut scored: Vec<(f32, usize, i64)> = Vec::new();
    for (cell, remaining) in remaining_mask.iter().copied().enumerate() {
        // Draw for all cells so the deterministic noise stream does not shift.
        let noise = gumbel(&mut rng);
        if !remaining {
            continue;
        }
        let row = &guided[cell * VOCAB..(cell + 1) * VOCAB];
        let token = argmax(row);
        let confidence = row[token];
        let codebook = cell / frames.max(1);
        let score = confidence - LAYER_PENALTY * codebook as f32 + GUMBEL_SCALE * noise;
        scored.push((score, cell, token as i64));
    }
    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    scored.truncate(count.min(scored.len()));
    scored
        .into_iter()
        .map(|(_, cell, token)| (cell, token))
        .collect()
}

/// Unique code counts for collapse telemetry.
fn codebook_diversity(codes: &[i64], frames: usize) -> Vec<usize> {
    codes
        .chunks(frames.max(1))
        .map(|row| {
            let mut sorted = row.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            sorted.len()
        })
        .collect()
}

/// Only total single-code collapse is fatal; short utterances may have low diversity.
fn is_total_collapse(diversity: &[usize]) -> bool {
    !diversity.is_empty() && diversity.iter().all(|&unique| unique <= 1)
}

/// FNV-1a with NUL-separated fields.
fn stable_seed(language: &str, instruct: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for chunk in [language.as_bytes(), &[0][..], instruct.as_bytes()] {
        for &byte in chunk {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

fn output_index(session: &Session, name: &str) -> Result<usize, String> {
    session
        .outputs()
        .iter()
        .position(|output| output.name() == name)
        .ok_or_else(|| format!("model has no `{name}` output"))
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

/// Recursively split on the nearest midpoint whitespace until each piece fits.
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
    use ort::value::{Shape, SymbolicDimensions};

    /// Published attribute vocabulary from upstream `docs/voice-design.md`.
    fn instruct_is_legal(instruct: &str) -> Result<(), String> {
        const LEGAL: &[&str] = &[
            "male",
            "female",
            "child",
            "teenager",
            "young adult",
            "middle-aged",
            "elderly",
            "very low pitch",
            "low pitch",
            "moderate pitch",
            "high pitch",
            "very high pitch",
            "whisper",
            "american accent",
            "british accent",
            "australian accent",
            "canadian accent",
            "indian accent",
            "chinese accent",
            "korean accent",
            "japanese accent",
            "portuguese accent",
            "russian accent",
        ];
        for item in instruct.split(", ") {
            if !LEGAL.contains(&item) {
                return Err(format!("illegal instruct item `{item}`"));
            }
        }
        Ok(())
    }

    #[test]
    fn every_preset_instruct_is_legal_vocabulary() {
        for (id, instruct) in OMNIVOICE_PRESETS {
            if instruct.is_empty() {
                assert_eq!(*id, "default", "only default may omit the instruct");
                continue;
            }
            instruct_is_legal(instruct).unwrap_or_else(|error| panic!("{id}: {error}"));
        }
        let error = instruct_is_legal("warm, clear female voice").unwrap_err();
        assert!(error.contains("`warm`"), "{error}");
    }

    #[test]
    fn preset_ids_match_the_registry_voices_exactly() {
        let preset_ids: Vec<&str> = OMNIVOICE_PRESETS.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            preset_ids,
            ds_config::TtsModel::OmniVoice.descriptor().voices
        );
        let descriptor = ds_config::TtsModel::OmniVoice.descriptor();
        assert_eq!(descriptor.default_voices, ["young_woman"]);
        assert!(descriptor.voices.contains(&descriptor.warmup_voice));
    }

    #[test]
    fn voice_resolution_maps_presets_and_passes_raw_instructs() {
        assert_eq!(
            preset_instruct("young_woman"),
            Some("female, young adult, moderate pitch")
        );
        assert_eq!(preset_instruct("default"), Some(""));
        assert_eq!(preset_instruct("no_such_voice"), None);
        assert_eq!(mlx_voice_arg("default"), "default");
        assert_eq!(mlx_voice_arg(""), "default");
        assert_eq!(mlx_voice_arg("whisper"), "female, young adult, whisper");
        assert_eq!(mlx_voice_arg("male, elderly"), "male, elderly");
    }

    #[test]
    fn cuda_backbone_uses_cpu_decoder() {
        use ds_config::RealizedProvider::*;
        assert_eq!(decoder_provider(Cuda), Cpu);
        for passthrough in [Cpu, CoreMl, Mlx, System] {
            assert_eq!(decoder_provider(passthrough), passthrough);
        }
    }

    #[test]
    fn argmax_is_deterministic_and_breaks_ties_low() {
        assert_eq!(argmax(&[1.0, 3.0, 3.0]), 1);
        assert_eq!(argmax(&[f32::NEG_INFINITY, -1.0]), 1);
    }

    #[test]
    fn declared_params_resolve_to_the_decode_defaults() {
        let model = ds_config::TtsModel::OmniVoice;
        let resolved = model.descriptor().resolve_params(&Default::default());
        assert_eq!(resolved.int(model, "steps"), 16);
        assert_eq!(u64::try_from(resolved.int(model, "seed")).ok(), None);
        let mut stored = ds_config::TtsParamMap::new();
        stored.insert("seed".into(), ds_config::TtsParamValue::Int(42));
        let resolved = model.descriptor().resolve_params(&stored);
        assert_eq!(u64::try_from(resolved.int(model, "seed")).ok(), Some(42));
    }

    #[test]
    fn schedule_counts_sum_exactly_and_never_go_negative() {
        for (total, steps) in [(4800, 32), (80, 32), (7, 3), (1, 32), (576, 16)] {
            let counts = schedule_counts(total, steps);
            assert_eq!(counts.len(), steps);
            assert_eq!(counts.iter().sum::<usize>(), total, "{total}/{steps}");
            let mut remaining = total;
            for count in counts {
                assert!(count <= remaining, "{total}/{steps} exceeded remaining");
                remaining -= count;
            }
            assert_eq!(remaining, 0);
        }
    }

    #[test]
    fn schedule_counts_single_step_takes_everything() {
        assert_eq!(schedule_counts(4800, 1), vec![4800]);
        assert_eq!(schedule_counts(0, 1), vec![0]);
    }

    fn test_row(rest: impl Fn(usize) -> f32) -> Vec<f32> {
        (0..VOCAB)
            .map(|index| {
                if index == MASK_TOKEN as usize {
                    -1e4
                } else {
                    rest(index)
                }
            })
            .collect()
    }

    #[test]
    fn guided_logits_reduce_to_cond_log_softmax_when_passes_agree() {
        let row = test_row(|index| (index % 13) as f32 * 0.25 - 1.0);
        let guided = guided_logits(&row, &row);
        let plain = log_softmax(&row);
        for index in 0..VOCAB {
            if index == MASK_TOKEN as usize {
                continue;
            }
            assert!(
                (guided[index] - plain[index]).abs() < 1e-4,
                "index {index}: {} vs {}",
                guided[index],
                plain[index]
            );
        }
    }

    #[test]
    fn guided_logits_rows_normalize_and_pin_mask() {
        let cond = test_row(|index| if index == 3 { 4.0 } else { 0.0 });
        let uncond = test_row(|index| if index == 7 { 2.0 } else { 0.5 });
        let guided = guided_logits(&cond, &uncond);
        assert_eq!(guided[MASK_TOKEN as usize], f32::NEG_INFINITY);
        let sum: f32 = guided
            .iter()
            .filter(|value| value.is_finite())
            .map(|value| value.exp())
            .sum();
        assert!((sum - 1.0).abs() < 1e-3, "probabilities sum to {sum}");
    }

    #[test]
    fn guided_logits_stay_finite_at_extreme_magnitudes() {
        let cond = test_row(|index| if index == 0 { 1e4 } else { -1e4 });
        let uncond = test_row(|index| if index == 1 { 1e4 } else { -1e4 });
        let guided = guided_logits(&cond, &uncond);
        for (index, value) in guided.iter().enumerate() {
            if index == MASK_TOKEN as usize {
                continue;
            }
            assert!(!value.is_nan(), "NaN at {index}");
        }
        assert_eq!(argmax(&guided), 0);
    }

    fn uniform_guided(cells: usize, peaks: &[usize]) -> Vec<f32> {
        let mut guided = vec![-20.0f32; cells * VOCAB];
        for (cell, &peak) in peaks.iter().enumerate() {
            guided[cell * VOCAB + peak] = -0.5;
            guided[cell * VOCAB + MASK_TOKEN as usize] = f32::NEG_INFINITY;
        }
        guided
    }

    #[test]
    fn select_cells_never_pick_the_mask_token() {
        let cells = CODEBOOKS;
        let guided = uniform_guided(cells, &[5; CODEBOOKS]);
        let picked = select_cells(&guided, &[true; CODEBOOKS], cells, 42);
        assert_eq!(picked.len(), cells);
        for (_, token) in picked {
            assert_ne!(token, MASK_TOKEN);
            assert_eq!(token, 5);
        }
    }

    #[test]
    fn layer_penalty_orders_codebook_zero_ahead_at_equal_confidence() {
        let guided = uniform_guided(CODEBOOKS, &[9; CODEBOOKS]);
        let picked = select_cells(&guided, &[true; CODEBOOKS], 1, 7);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].0, 0, "codebook 0 must unmask first");
    }

    #[test]
    fn select_cells_reproduce_under_a_fixed_seed() {
        let cells = CODEBOOKS * 4;
        let peaks: Vec<usize> = (0..cells).map(|cell| cell % CODEBOOK_SIZE).collect();
        let guided = uniform_guided(cells, &peaks);
        let mask = vec![true; cells];
        let first = select_cells(&guided, &mask, 5, 1234);
        let second = select_cells(&guided, &mask, 5, 1234);
        assert_eq!(first, second);
    }

    #[test]
    fn select_cells_clamp_to_the_remaining_cells() {
        let cells = CODEBOOKS;
        let guided = uniform_guided(cells, &[1; CODEBOOKS]);
        let mut mask = vec![false; cells];
        mask[2] = true;
        mask[5] = true;
        let picked = select_cells(&guided, &mask, 100, 9);
        assert_eq!(picked.len(), 2);
        let picked_cells: Vec<usize> = picked.iter().map(|(cell, _)| *cell).collect();
        assert!(picked_cells.contains(&2) && picked_cells.contains(&5));
    }

    // Distinct sentinels expose segment order and boundaries.
    fn fake_encode(segment: &str) -> Result<Vec<u32>, String> {
        let sentinel = if segment.starts_with("<|lang_start|>") {
            900_000
        } else {
            800_000
        };
        let mut ids = vec![sentinel];
        ids.extend(segment.bytes().map(u32::from));
        Ok(ids)
    }

    #[test]
    fn prompt_orders_style_before_text_and_broadcasts_all_codebooks() {
        let prompt = build_prompt(fake_encode, "en", "female, young adult", "Hi.", 12).unwrap();
        let seq = prompt.seq();
        assert_eq!(seq, prompt.cond_len + 12);
        assert_eq!(prompt.ids.len(), CODEBOOKS * seq);
        let row0 = &prompt.ids[..seq];
        assert_eq!(row0[0], 900_000);
        let text_sentinel = row0.iter().position(|&id| id == 800_000).unwrap();
        assert!(text_sentinel > 0 && text_sentinel < prompt.cond_len);
        for codebook in 1..CODEBOOKS {
            assert_eq!(&prompt.ids[codebook * seq..(codebook + 1) * seq], row0);
        }
        assert!(row0[prompt.cond_len..].iter().all(|&id| id == MASK_TOKEN));
    }

    #[test]
    fn prompt_audio_mask_covers_exactly_the_target_region() {
        let prompt = build_prompt(fake_encode, "en", "", "Hello there.", 30).unwrap();
        assert_eq!(prompt.audio_mask.len(), prompt.seq());
        assert!(prompt.audio_mask[..prompt.cond_len].iter().all(|&m| !m));
        assert!(prompt.audio_mask[prompt.cond_len..].iter().all(|&m| m));
        assert_eq!(prompt.frames, 30);
    }

    #[test]
    fn empty_instruct_omits_the_instruct_block() {
        let with = build_prompt(fake_encode, "en", "female voice", "Hi.", 10).unwrap();
        let without = build_prompt(fake_encode, "en", "", "Hi.", 10).unwrap();
        assert!(without.cond_len < with.cond_len);
        let style_len_without = without.cond_len - "<|text_start|>Hi.<|text_end|>".len() - 1;
        assert_eq!(
            style_len_without,
            "<|lang_start|>en<|lang_end|>".len() + 1,
            "style segment must be the bare lang block"
        );
    }

    fn tensor(ty: TensorElementType, dims: &[i64]) -> ValueType {
        ValueType::Tensor {
            ty,
            shape: Shape::new(dims.iter().copied()),
            dimension_symbols: SymbolicDimensions::empty(dims.len()),
        }
    }

    fn published_inputs() -> Vec<(&'static str, ValueType)> {
        vec![
            (
                "inputs_embeds",
                tensor(TensorElementType::Float32, &[-1, -1, 1024]),
            ),
            (
                "attention_mask",
                tensor(TensorElementType::Bool, &[-1, 1, -1, -1]),
            ),
        ]
    }

    #[test]
    fn published_export_shape_passes_the_contract() {
        assert_eq!(
            check_llm_contract(&published_inputs(), &["hidden_states"]),
            Ok(())
        );
    }

    #[test]
    fn a_causal_lm_style_int64_mask_fails_naming_the_expected_export() {
        let inputs = vec![
            (
                "inputs_embeds",
                tensor(TensorElementType::Float32, &[-1, -1, 1024]),
            ),
            (
                "attention_mask",
                tensor(TensorElementType::Int64, &[-1, -1]),
            ),
        ];
        let error = check_llm_contract(&inputs, &["hidden_states"]).unwrap_err();
        assert!(error.contains("llm_backbone_fp32"), "{error}");
        assert!(error.contains("attention_mask"), "{error}");
    }

    #[test]
    fn kv_cache_inputs_fail_the_contract() {
        let mut inputs = published_inputs();
        inputs.push((
            "past_key_values.0.key",
            tensor(TensorElementType::Float32, &[-1, 8, -1, 128]),
        ));
        let error = check_llm_contract(&inputs, &["hidden_states"]).unwrap_err();
        assert!(error.contains("KV-cache"), "{error}");
    }

    #[test]
    fn a_missing_hidden_states_output_fails_the_contract() {
        let error = check_llm_contract(&published_inputs(), &["logits"]).unwrap_err();
        assert!(error.contains("hidden_states"), "{error}");
    }

    #[test]
    fn diversity_counts_unique_codes_per_codebook() {
        let codes = [1, 2, 2, 3, 7, 7, 7, 7];
        assert_eq!(codebook_diversity(&codes, 4), vec![3, 1]);
    }

    #[test]
    fn only_total_collapse_is_fatal() {
        assert!(is_total_collapse(&[1, 1, 1, 1, 1, 1, 1, 1]));
        assert!(!is_total_collapse(&[1, 1, 1, 1, 1, 1, 1, 2]));
        assert!(!is_total_collapse(&[55, 60, 48, 62, 51, 58, 49, 63]));
        assert!(!is_total_collapse(&[]));
    }

    #[test]
    fn stable_seed_separates_fields_and_is_deterministic() {
        assert_eq!(stable_seed("en", "female"), stable_seed("en", "female"));
        assert_ne!(stable_seed("en", "female"), stable_seed("en", "male"));
        assert_ne!(stable_seed("ab", "c"), stable_seed("a", "bc"));
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
