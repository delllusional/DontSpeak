//! Qwen3-TTS CustomVoice int4 ONNX inference.

use std::collections::HashMap;
use std::path::Path;

use half::f16;
use ort::session::{Session, SessionInputValue};
use ort::value::{Tensor, TensorElementType, ValueType};
use serde::Deserialize;
use tokenizers::AddedToken;
use tokenizers::models::bpe::BPE;
use tokenizers::pre_tokenizers::byte_level::ByteLevel;
use tokenizers::pre_tokenizers::sequence::Sequence;
use tokenizers::pre_tokenizers::split::{Split, SplitPattern};
use tokenizers::pre_tokenizers::PreTokenizerWrapper;
use tokenizers::SplitDelimiterBehavior;

/// Qwen2 pre-tokenizer regex (`transformers.models.qwen2.tokenization_qwen2.PRETOKENIZE_REGEX`).
/// Unlike GPT-2's ByteLevel default, an optional leading non-letter/non-digit stays with the
/// following letters (`-seven`, `'S`, `.com`, `(hello`…), so BPE sees the in-distribution spans.
const QWEN2_PRETOKENIZE_REGEX: &str = r"(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+";

const GROUPS: usize = 16;
const DECODER_FRAMES: usize = 25;
const MAX_NEW_TOKENS: usize = 2048;

#[derive(Deserialize)]
struct Config {
    tts_bos_token_id: i64,
    tts_eos_token_id: i64,
    tts_pad_token_id: i64,
    talker_config: TalkerConfig,
}

#[derive(Deserialize)]
struct TalkerConfig {
    codec_eos_token_id: i64,
    codec_pad_id: i64,
    codec_bos_id: i64,
    codec_think_id: i64,
    codec_nothink_id: i64,
    codec_think_bos_id: i64,
    codec_think_eos_id: i64,
    codec_language_id: HashMap<String, i64>,
    spk_id: HashMap<String, i64>,
    vocab_size: usize,
}

#[derive(Deserialize)]
struct TokenizerConfig {
    added_tokens_decoder: HashMap<String, AddedToken>,
}

struct TensorData {
    shape: Vec<i64>,
    data: Vec<f32>,
}

struct TalkerOutput {
    logits: TensorData,
    hidden: TensorData,
    cache: Vec<TensorData>,
}

impl TensorData {
    fn tensor(&self) -> Result<Tensor<f32>, String> {
        if self.data.is_empty() {
            Tensor::new(&ort::memory::Allocator::default(), self.shape.clone())
                .map_err(|error| format!("qwen empty tensor: {error}"))
        } else {
            Tensor::from_array((self.shape.clone(), self.data.clone()))
                .map_err(|error| format!("qwen tensor: {error}"))
        }
    }

    /// f16 form of [`tensor`](Self::tensor). The int4 talker's FP16 sub-graphs take their
    /// embeddings/hidden/KV as float16; the host loop stays f32 and narrows at the boundary.
    fn tensor_f16(&self) -> Result<Tensor<f16>, String> {
        if self.data.is_empty() {
            Tensor::new(&ort::memory::Allocator::default(), self.shape.clone())
                .map_err(|error| format!("qwen empty f16 tensor: {error}"))
        } else {
            let data: Vec<f16> = self.data.iter().map(|&x| f16::from_f32(x)).collect();
            Tensor::from_array((self.shape.clone(), data))
                .map_err(|error| format!("qwen f16 tensor: {error}"))
        }
    }
}

/// Build a float model input from f32 host data, narrowed to float16 when the graph
/// declares that input as f16 (per-input, since a graph can mix dtypes).
fn float_feed(
    session: &Session,
    name: &str,
    data: &TensorData,
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
        Ok(data.tensor_f16()?.into())
    } else {
        Ok(data.tensor()?.into())
    }
}

pub struct QwenSynth {
    text_embed: Session,
    codec_embed: Session,
    talker: Session,
    code_predictor: Session,
    residual_embed: Session,
    decoder: Session,
    tokenizer: tokenizers::Tokenizer,
    config: Config,
    past_names: Vec<String>,
    past_shapes: Vec<Vec<i64>>,
    present_indices: Vec<usize>,
    logits_index: usize,
    hidden_index: usize,
    provider: ds_config::RealizedProvider,
}

impl QwenSynth {
    pub fn load() -> Result<Self, String> {
        crate::ort_session::load_with_fallback("qwen", Self::load_with_provider)
    }

    pub fn load_with_provider(preference: &str) -> Result<Self, String> {
        let dir = ds_model::tts_model_dir(ds_config::TtsModel::Qwen)
            .ok_or("cannot resolve model_dir()")?;
        let mut sessions =
            crate::ort_session::OrtSessions::from_preference(ds_config::TtsModel::Qwen, preference);
        let text_embed = session(&mut sessions, &dir, "text_embed.onnx")?;
        let codec_embed = session(&mut sessions, &dir, "codec_embed.onnx")?;
        let talker = session(&mut sessions, &dir, "talker_cache.onnx")?;
        let code_predictor = session(&mut sessions, &dir, "code_predictor.onnx")?;
        let residual_embed = session(&mut sessions, &dir, "residual_embed.onnx")?;
        let decoder = session(&mut sessions, &dir, "tok_decoder.onnx")?;
        let provider = sessions.provider();
        let tokenizer = load_tokenizer(&dir)?;
        let config = serde_json::from_slice(
            &std::fs::read(dir.join("config.json"))
                .map_err(|error| format!("qwen config read: {error}"))?,
        )
        .map_err(|error| format!("qwen config parse: {error}"))?;

        let past_names: Vec<String> = talker
            .inputs()
            .iter()
            .filter(|input| input.name().contains("past"))
            .map(|input| input.name().to_string())
            .collect();
        if past_names.is_empty() {
            return Err("qwen talker has no KV-cache inputs".to_string());
        }
        let past_shapes = past_names
            .iter()
            .map(|name| empty_cache_shape(&talker, name))
            .collect::<Result<Vec<_>, _>>()?;
        let output_names: Vec<String> = talker
            .outputs()
            .iter()
            .map(|output| output.name().to_string())
            .collect();
        let logits_index = named_output(&output_names, &["logits", "logits_Q4"])?;
        let hidden_index = named_output(&output_names, &["hidden", "hidden_states"])?;
        let present_indices =
            present_output_indices(&past_names, &output_names, &[logits_index, hidden_index])?;
        Ok(Self {
            text_embed,
            codec_embed,
            talker,
            code_predictor,
            residual_embed,
            decoder,
            tokenizer,
            config,
            past_names,
            past_shapes,
            present_indices,
            logits_index,
            hidden_index,
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
        if cancelled() {
            return Ok(Vec::new());
        }
        // Declared param, default 1.05 (the reference generation_config value this port
        // hardcoded before the parameter surface).
        let repetition_penalty = params.float(ds_config::TtsModel::Qwen, "repetition_penalty");
        let input_ids = self.tokenize(&format!(
            "<|im_start|>assistant\n{text}<|im_end|>\n<|im_start|>assistant\n"
        ))?;
        if input_ids.len() < 9 {
            return Err("qwen text tokenized too short for the assistant template".to_string());
        }
        if cancelled() {
            return Ok(Vec::new());
        }
        let special = self.embed_text(&[
            self.config.tts_bos_token_id,
            self.config.tts_eos_token_id,
            self.config.tts_pad_token_id,
        ])?;
        let hidden = hidden_width(&special)?;
        let bos = row(&special, 0, hidden)?;
        let eos = row(&special, 1, hidden)?;
        let pad = row(&special, 2, hidden)?;

        let talker = &self.config.talker_config;
        let language = ds_config::TtsModel::Qwen
            .descriptor()
            .runtime_language(language);
        let codec_prefill = match talker.codec_language_id.get(language) {
            Some(language_id) => vec![
                talker.codec_think_id,
                talker.codec_think_bos_id,
                *language_id,
                talker.codec_think_eos_id,
            ],
            None => vec![
                talker.codec_nothink_id,
                talker.codec_think_bos_id,
                talker.codec_think_eos_id,
            ],
        };
        let codec_pad_id = talker.codec_pad_id;
        let codec_bos_id = talker.codec_bos_id;
        let speaker = *talker
            .spk_id
            .get(&voice.to_ascii_lowercase())
            .ok_or_else(|| format!("unknown qwen voice '{voice}'"))?;
        let mut codec_input = self.embed_codec(&codec_prefill)?;
        let speaker_embed = self.embed_codec(&[speaker])?;
        append_rows(&mut codec_input, &speaker_embed, hidden)?;
        let codec_tail = self.embed_codec(&[codec_pad_id, codec_bos_id])?;
        append_rows(&mut codec_input, &codec_tail, hidden)?;

        let role = self.embed_text(&input_ids[..3])?;
        let codec_rows = rows(&codec_input, hidden)?;
        let mut prefill = role;
        for index in 0..codec_rows - 1 {
            let text_embed = if index + 1 == codec_rows - 1 {
                &bos
            } else {
                &pad
            };
            let codec_embed = row(&codec_input, index, hidden)?;
            append_row(&mut prefill, add(text_embed, codec_embed), hidden)?;
        }

        let body = self.embed_text(&input_ids[3..input_ids.len() - 5])?;
        let body_rows = rows(&body, hidden)?;
        let codec_pad = self.embed_codec(&[codec_pad_id])?;
        for index in 0..body_rows {
            append_row(
                &mut prefill,
                add(row(&body, index, hidden)?, row(&codec_pad, 0, hidden)?),
                hidden,
            )?;
        }
        append_row(&mut prefill, add(eos, row(&codec_pad, 0, hidden)?), hidden)?;
        let codec_bos = self.embed_codec(&[codec_bos_id])?;
        append_row(&mut prefill, add(pad, row(&codec_bos, 0, hidden)?), hidden)?;

        if cancelled() {
            return Ok(Vec::new());
        }
        let codes = self.generate(prefill, pad, hidden, repetition_penalty, cancelled)?;
        if codes.is_empty() || cancelled() {
            return Ok(Vec::new());
        }
        self.decode(&codes, cancelled)
    }

    fn tokenize(&self, text: &str) -> Result<Vec<i64>, String> {
        self.tokenizer
            .encode(text, false)
            .map(|encoded| encoded.get_ids().iter().map(|id| i64::from(*id)).collect())
            .map_err(|error| format!("qwen tokenize: {error}"))
    }

    fn embed_text(&mut self, ids: &[i64]) -> Result<TensorData, String> {
        run_ids(&mut self.text_embed, "text_ids", ids, "qwen text embed")
    }

    fn embed_codec(&mut self, ids: &[i64]) -> Result<TensorData, String> {
        run_ids(&mut self.codec_embed, "codec_ids", ids, "qwen codec embed")
    }

    fn generate(
        &mut self,
        prefill: TensorData,
        trailing: &[f32],
        hidden_width: usize,
        repetition_penalty: f32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<i64>, String> {
        let mut cache = self
            .past_shapes
            .iter()
            .map(|shape| TensorData {
                shape: shape.clone(),
                data: Vec::new(),
            })
            .collect::<Vec<_>>();
        let total = rows(&prefill, hidden_width)?;
        let output = self.run_talker(&prefill, total, 0, &cache)?;
        cache = output.cache;
        let mut logits = output.logits;
        let mut hidden = output.hidden;
        let mut codes = Vec::new();
        let mut previous = Vec::new();
        let mut stopped = false;

        for total in (total..).take(MAX_NEW_TOKENS) {
            if cancelled() {
                return Ok(Vec::new());
            }
            let vocab = self.config.talker_config.vocab_size;
            let last_logits = last_row(&logits, vocab)?;
            let first = select_first_code(
                last_logits,
                vocab,
                self.config.talker_config.codec_eos_token_id,
                &previous,
                repetition_penalty,
            );
            if first == self.config.talker_config.codec_eos_token_id {
                stopped = true;
                break;
            }
            previous.push(first);
            let talker_hidden = last_row(&hidden, hidden_width)?.to_vec();
            let mut frame = [0i64; GROUPS];
            frame[0] = first;
            for group in 1..GROUPS {
                if cancelled() {
                    return Ok(Vec::new());
                }
                let hidden_data = TensorData {
                    shape: vec![1, hidden_width as i64],
                    data: talker_hidden.clone(),
                };
                let ids_tensor = Tensor::from_array((vec![1, GROUPS as i64], frame.to_vec()))
                    .map_err(|error| format!("qwen predictor IDs tensor: {error}"))?;
                let predicted = self
                    .code_predictor
                    .run(vec![
                        (
                            "talker_hidden".to_string(),
                            float_feed(&self.code_predictor, "talker_hidden", &hidden_data)?,
                        ),
                        ("codec_ids".to_string(), SessionInputValue::from(ids_tensor)),
                    ])
                    .map_err(|error| format!("qwen code predictor run: {error}"))?;
                let predicted = extract_f32(&predicted[0], "qwen group logits")?;
                let group_vocab = predicted.data.len() / (GROUPS - 1);
                let start = (group - 1) * group_vocab;
                frame[group] = argmax(&predicted.data[start..start + group_vocab]) as i64;
            }
            codes.extend_from_slice(&frame);

            let ids = Tensor::from_array((vec![1, GROUPS as i64], frame.to_vec()))
                .map_err(|error| format!("qwen residual IDs tensor: {error}"))?;
            let mut next = {
                let embedded = self
                    .residual_embed
                    .run(ort::inputs! { "codec_ids" => ids })
                    .map_err(|error| format!("qwen residual embed run: {error}"))?;
                extract_f32(&embedded[0], "qwen residual embed")?
            };
            if next.data.len() != hidden_width {
                return Err(format!(
                    "qwen residual width {} does not match {hidden_width}",
                    next.data.len()
                ));
            }
            for (value, pad) in next.data.iter_mut().zip(trailing) {
                *value += pad;
            }
            next.shape = vec![1, 1, hidden_width as i64];
            let output = self.run_talker(&next, total + 1, total, &cache)?;
            cache = output.cache;
            logits = output.logits;
            hidden = output.hidden;
        }
        if !stopped {
            log::warn!(
                target: "tts",
                "qwen exhausted MAX_NEW_TOKENS ({MAX_NEW_TOKENS}) without EOS; audio tail may be truncated"
            );
        }
        Ok(codes)
    }

    fn run_talker(
        &mut self,
        embeds: &TensorData,
        attention_length: usize,
        position_start: usize,
        cache: &[TensorData],
    ) -> Result<TalkerOutput, String> {
        let current = embeds.shape[1] as usize;
        let mut transposed = vec![0i64; 3 * current];
        for axis in 0..3 {
            for offset in 0..current {
                transposed[axis * current + offset] = (position_start + offset) as i64;
            }
        }
        let positions = Tensor::from_array((vec![3, 1, current as i64], transposed))
            .map_err(|error| format!("qwen positions tensor: {error}"))?;
        let attention = Tensor::from_array((
            vec![1, attention_length as i64],
            vec![1i64; attention_length],
        ))
        .map_err(|error| format!("qwen attention tensor: {error}"))?;
        let mut feed: Vec<(String, SessionInputValue)> = vec![
            (
                "inputs_embeds".into(),
                float_feed(&self.talker, "inputs_embeds", embeds)?,
            ),
            ("position_ids".into(), positions.into()),
            ("attention_mask".into(), attention.into()),
        ];
        for ((name, _), value) in self.past_names.iter().zip(&self.past_shapes).zip(cache) {
            feed.push((name.clone(), float_feed(&self.talker, name, value)?));
        }
        let outputs = self
            .talker
            .run(feed)
            .map_err(|error| format!("qwen talker run: {error}"))?;
        Ok(TalkerOutput {
            logits: extract_f32(&outputs[self.logits_index], "qwen logits")?,
            hidden: extract_f32(&outputs[self.hidden_index], "qwen hidden")?,
            cache: extract_cache(&outputs, &self.present_indices)?,
        })
    }

    fn decode(&mut self, codes: &[i64], cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>, String> {
        let frames = codes.len() / GROUPS;
        let mut waveform = Vec::new();
        for start in (0..frames).step_by(DECODER_FRAMES) {
            if cancelled() {
                return Ok(Vec::new());
            }
            let count = (frames - start).min(DECODER_FRAMES);
            let mut chunk = Vec::with_capacity(DECODER_FRAMES * GROUPS);
            for frame in 0..DECODER_FRAMES {
                let source = start + frame % count;
                chunk.extend_from_slice(&codes[source * GROUPS..(source + 1) * GROUPS]);
            }
            let input = Tensor::from_array((vec![1, DECODER_FRAMES as i64, GROUPS as i64], chunk))
                .map_err(|error| format!("qwen decoder tensor: {error}"))?;
            let output = self
                .decoder
                .run(ort::inputs! { "audio_codes" => input })
                .map_err(|error| format!("qwen decoder run: {error}"))?;
            let decoded = extract_f32(&output[0], "qwen waveform")?;
            let keep = decoded.data.len() * count / DECODER_FRAMES;
            waveform.extend_from_slice(&decoded.data[..keep]);
        }
        Ok(crate::trim::trim_silence(&waveform))
    }
}

fn session(
    sessions: &mut crate::ort_session::OrtSessions,
    dir: &Path,
    file: &str,
) -> Result<Session, String> {
    sessions.load_file(&dir.join(file))
}

fn load_tokenizer(dir: &Path) -> Result<tokenizers::Tokenizer, String> {
    let vocab = dir.join("vocab.json");
    let merges = dir.join("merges.txt");
    let model = BPE::from_file(
        vocab.to_string_lossy().as_ref(),
        merges.to_string_lossy().as_ref(),
    )
    .build()
    .map_err(|error| format!("qwen BPE load: {error}"))?;
    let mut tokenizer = tokenizers::Tokenizer::new(model);
    // Qwen2: Split(regex, Isolated) then ByteLevel(use_regex=false). GPT-2 ByteLevel(use_regex=true)
    // alone yields OOD ids on hyphenated numbers, contractions, domains, and open-paren words.
    let byte_level = ByteLevel::new(false, false, false);
    let split = Split::new(
        SplitPattern::Regex(QWEN2_PRETOKENIZE_REGEX.to_owned()),
        SplitDelimiterBehavior::Isolated,
        false,
    )
    .map_err(|error| format!("qwen pre-tokenizer: {error}"))?;
    tokenizer.with_pre_tokenizer(Some(Sequence::new(vec![
        PreTokenizerWrapper::from(split),
        PreTokenizerWrapper::from(byte_level),
    ])));
    tokenizer.with_decoder(Some(byte_level));
    let config: TokenizerConfig = serde_json::from_slice(
        &std::fs::read(dir.join("tokenizer_config.json"))
            .map_err(|error| format!("qwen tokenizer config read: {error}"))?,
    )
    .map_err(|error| format!("qwen tokenizer config parse: {error}"))?;
    let mut tokens: Vec<(u32, AddedToken)> = config
        .added_tokens_decoder
        .into_iter()
        .map(|(id, token)| {
            id.parse::<u32>()
                .map(|id| (id, token))
                .map_err(|error| format!("qwen added-token ID: {error}"))
        })
        .collect::<Result<_, _>>()?;
    tokens.sort_by_key(|(id, _)| *id);
    tokenizer
        .add_tokens(tokens.into_iter().map(|(_, token)| token))
        .map_err(|error| format!("qwen added tokens: {error}"))?;
    Ok(tokenizer)
}

fn run_ids(
    session: &mut Session,
    name: &str,
    ids: &[i64],
    label: &str,
) -> Result<TensorData, String> {
    let tensor = Tensor::from_array((vec![1, ids.len() as i64], ids.to_vec()))
        .map_err(|error| format!("{label} tensor: {error}"))?;
    let outputs = session
        .run(vec![(name.to_string(), SessionInputValue::from(tensor))])
        .map_err(|error| format!("{label} run: {error}"))?;
    extract_f32(&outputs[0], label)
}

/// Extract a float output as f32, accepting either f32 or f16 — the FP16 talker sub-graphs
/// emit float16 while other graphs emit float32.
fn extract_f32(value: &ort::value::DynValue, label: &str) -> Result<TensorData, String> {
    if let Ok((shape, data)) = value.try_extract_tensor::<f32>() {
        return Ok(TensorData {
            shape: shape.to_vec(),
            data: data.to_vec(),
        });
    }
    let (shape, data) = value
        .try_extract_tensor::<f16>()
        .map_err(|error| format!("{label}: {error}"))?;
    Ok(TensorData {
        shape: shape.to_vec(),
        data: data.iter().map(|h| h.to_f32()).collect(),
    })
}

fn extract_cache(
    outputs: &ort::session::SessionOutputs<'_>,
    indices: &[usize],
) -> Result<Vec<TensorData>, String> {
    indices
        .iter()
        .map(|index| extract_f32(&outputs[*index], "qwen present cache"))
        .collect()
}

fn empty_cache_shape(session: &Session, name: &str) -> Result<Vec<i64>, String> {
    let input = session
        .inputs()
        .iter()
        .find(|input| input.name() == name)
        .ok_or_else(|| format!("qwen talker has no `{name}` input"))?;
    let ValueType::Tensor { shape, .. } = input.dtype() else {
        return Err(format!("qwen `{name}` cache is not a tensor"));
    };
    let mut shape = shape.to_vec();
    if shape.len() < 2 {
        return Err(format!(
            "qwen `{name}` cache has rank {}; need at least 2",
            shape.len()
        ));
    }
    for dimension in &mut shape {
        if *dimension < 0 {
            *dimension = 1;
        }
    }
    let sequence = shape.len() - 2;
    shape[sequence] = 0;
    Ok(shape)
}

fn present_output_index(name: &str, outputs: &[String]) -> Option<usize> {
    let suffix = name
        .strip_prefix("past_key_values.")
        .or_else(|| name.strip_prefix("past."))
        .unwrap_or(name);
    outputs.iter().position(|output| {
        output == &format!("present.{suffix}") || output == &format!("present_key_values.{suffix}")
    })
}

fn present_output_indices(
    past_names: &[String],
    outputs: &[String],
    reserved: &[usize],
) -> Result<Vec<usize>, String> {
    let named: Vec<Option<usize>> = past_names
        .iter()
        .map(|name| present_output_index(name, outputs))
        .collect();
    if named.iter().all(Option::is_some) {
        let indices: Vec<usize> = named.into_iter().flatten().collect();
        let mut unique = indices.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() == indices.len() && !indices.iter().any(|index| reserved.contains(index)) {
            return Ok(indices);
        }
        return Err("qwen talker present outputs overlap or repeat".to_string());
    }
    if named.iter().any(Option::is_some) {
        return Err("qwen talker only partially names its present outputs".to_string());
    }
    let positional: Vec<usize> = (0..outputs.len())
        .filter(|index| !reserved.contains(index))
        .collect();
    if positional.len() != past_names.len() {
        return Err(format!(
            "qwen talker has {} cache inputs but {} unclassified outputs; outputs: {outputs:?}",
            past_names.len(),
            positional.len()
        ));
    }
    Ok(positional)
}

fn named_output(outputs: &[String], candidates: &[&str]) -> Result<usize, String> {
    outputs
        .iter()
        .position(|output| candidates.contains(&output.as_str()))
        .ok_or_else(|| {
            format!(
                "qwen talker has no {} output; outputs: {outputs:?}",
                candidates.join("|")
            )
        })
}

fn hidden_width(tensor: &TensorData) -> Result<usize, String> {
    tensor
        .shape
        .last()
        .copied()
        .filter(|width| *width > 0)
        .map(|width| width as usize)
        .ok_or_else(|| "qwen embedding has no hidden width".to_string())
}

fn rows(tensor: &TensorData, width: usize) -> Result<usize, String> {
    if width == 0 || !tensor.data.len().is_multiple_of(width) {
        return Err("qwen embedding has inconsistent shape".to_string());
    }
    Ok(tensor.data.len() / width)
}

fn row(tensor: &TensorData, index: usize, width: usize) -> Result<&[f32], String> {
    tensor
        .data
        .get(index * width..(index + 1) * width)
        .ok_or_else(|| format!("qwen embedding has no row {index}"))
}

fn last_row(tensor: &TensorData, width: usize) -> Result<&[f32], String> {
    let rows = rows(tensor, width)?;
    if rows == 0 {
        return Err("qwen tensor has no final row".to_string());
    }
    Ok(&tensor.data[(rows - 1) * width..])
}

fn add(left: &[f32], right: &[f32]) -> Vec<f32> {
    left.iter()
        .zip(right)
        .map(|(left, right)| left + right)
        .collect()
}

fn append_rows(target: &mut TensorData, source: &TensorData, width: usize) -> Result<(), String> {
    rows(source, width)?;
    target.data.extend_from_slice(&source.data);
    target.shape = vec![1, rows(target, width)? as i64, width as i64];
    Ok(())
}

fn append_row(target: &mut TensorData, source: Vec<f32>, width: usize) -> Result<(), String> {
    if source.len() != width {
        return Err("qwen embedding row width mismatch".to_string());
    }
    target.data.extend(source);
    target.shape = vec![1, rows(target, width)? as i64, width as i64];
    Ok(())
}

fn select_first_code(
    logits: &[f32],
    vocab: usize,
    eos: i64,
    previous: &[i64],
    repetition_penalty: f32,
) -> i64 {
    let mut scores = logits.to_vec();
    for (id, score) in scores
        .iter_mut()
        .enumerate()
        .take(vocab)
        .skip(vocab.saturating_sub(1024))
    {
        if id as i64 != eos {
            *score = f32::NEG_INFINITY;
        }
    }
    apply_repetition_penalty(&mut scores, previous, repetition_penalty);
    argmax(&scores) as i64
}

fn apply_repetition_penalty(scores: &mut [f32], previous: &[i64], penalty: f32) {
    let mut seen = previous.to_vec();
    seen.sort_unstable();
    seen.dedup();
    for id in seen {
        if let Some(score) = scores.get_mut(id as usize) {
            *score = if *score < 0.0 {
                *score * penalty
            } else {
                *score / penalty
            };
        }
    }
}

fn argmax(values: &[f32]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            left.partial_cmp(right)
                .unwrap_or(std::cmp::Ordering::Less)
                .then_with(|| right_index.cmp(left_index))
        })
        .map_or(0, |(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tokenizers::tokenizer::{OffsetReferential, OffsetType, PreTokenizedString};
    use tokenizers::PreTokenizer;

    #[test]
    fn repetition_penalty_changes_each_seen_token_once() {
        let mut scores = vec![4.0, -2.0, 3.0];
        apply_repetition_penalty(&mut scores, &[0, 0, 1], 2.0);
        assert_eq!(scores, vec![2.0, -4.0, 3.0]);
    }

    /// Qwen2 keeps an optional leading non-letter with the following letters; GPT-2's regex
    /// peels it off. These are the spans that went OOD on the real model (issue #163).
    #[test]
    fn qwen2_pretokenizer_regex_matches_upstream_splits() {
        let split = Split::new(
            SplitPattern::Regex(QWEN2_PRETOKENIZE_REGEX.to_owned()),
            SplitDelimiterBehavior::Isolated,
            false,
        )
        .unwrap();
        let cases = [
            ("fifty-seven", &["fifty", "-seven"][..]),
            ("IT'S", &["IT", "'S"][..]),
            ("example.com", &["example", ".com"][..]),
            ("(hello", &["(hello"][..]),
        ];
        for (text, expected) in cases {
            let mut pretok = PreTokenizedString::from(text);
            split.pre_tokenize(&mut pretok).unwrap();
            let parts: Vec<String> = pretok
                .get_splits(OffsetReferential::Original, OffsetType::Byte)
                .into_iter()
                .map(|(s, _, _)| s.to_string())
                .collect();
            assert_eq!(parts, expected, "split of {text:?}");
        }
    }

    /// Hermetic id parity: tiny BPE where merges only form multi-char tokens inside a single
    /// pretoken. With the Qwen2 pre-tokenizer, the divergent cases encode to the composite
    /// ids; GPT-2-style ByteLevel(use_regex=true) would leave them unmerged.
    #[test]
    fn qwen_tokenizer_encodes_hyphen_contraction_domain_paren_as_upstream() {
        let dir = tempfile::tempdir().unwrap();
        // Character seeds + composites that BPE only reaches when the pretoken is joined.
        let mut vocab = BTreeMap::new();
        // Every single-char token that appears in merges.txt must be present or BPE::from_file fails.
        for (i, ch) in [
            'f', 'i', 't', 'y', '-', 's', 'e', 'v', 'n', 'I', 'T', '\'', 'S', 'x', 'a', 'm', 'p',
            'l', 'c', 'o', '.', '(', 'h',
        ]
        .into_iter()
        .enumerate()
        {
            vocab.insert(ch.to_string(), i as u32);
        }
        let composites: &[(&str, u32)] = &[
            ("fi", 100),
            ("fif", 101),
            ("fift", 102),
            ("fifty", 103),
            ("se", 104),
            ("sev", 105),
            ("seve", 106),
            ("seven", 107),
            ("-seven", 108),
            ("IT", 109),
            ("'S", 110),
            ("ex", 111),
            ("exa", 112),
            ("exam", 113),
            ("examp", 114),
            ("exampl", 115),
            ("example", 116),
            (".c", 117),
            (".co", 118),
            (".com", 119),
            ("(h", 120),
            ("(he", 121),
            ("(hel", 122),
            ("(hell", 123),
            ("(hello", 124),
            ("he", 125),
            ("hel", 126),
            ("hell", 127),
            ("hello", 128),
        ];
        for &(tok, id) in composites {
            vocab.insert(tok.to_string(), id);
        }
        std::fs::write(
            dir.path().join("vocab.json"),
            serde_json::to_string(&vocab).unwrap(),
        )
        .unwrap();
        // Ranked merges that build the composites left-to-right from single chars.
        let merges = "\
#version: 0.2
f i
fi f
fif t
fift y
s e
se v
sev e
seve n
- seven
I T
' S
e x
ex a
exa m
exam p
examp l
exampl e
. c
.c o
.co m
( h
(h e
(he l
(hel l
(hell o
h e
he l
hel l
hell o
";
        std::fs::write(dir.path().join("merges.txt"), merges).unwrap();
        std::fs::write(
            dir.path().join("tokenizer_config.json"),
            r#"{"added_tokens_decoder":{}}"#,
        )
        .unwrap();

        let tokenizer = load_tokenizer(dir.path()).unwrap();
        let cases: &[(&str, &[(u32, &str)])] = &[
            ("fifty-seven", &[(103, "fifty"), (108, "-seven")]),
            ("IT'S", &[(109, "IT"), (110, "'S")]),
            ("example.com", &[(116, "example"), (119, ".com")]),
            ("(hello", &[(124, "(hello")]),
        ];
        for &(text, expected) in cases {
            let encoding = tokenizer.encode(text, false).unwrap();
            let ids = encoding.get_ids();
            let tokens = encoding.get_tokens();
            let got: Vec<(u32, &str)> = ids
                .iter()
                .zip(tokens.iter())
                .map(|(&id, tok)| (id, tok.as_str()))
                .collect();
            assert_eq!(got, expected, "encode of {text:?}");
        }
    }

    #[test]
    fn first_code_suppresses_reserved_tail_except_eos() {
        let mut logits = vec![0.0; 3072];
        logits[100] = 5.0;
        logits[2200] = 9.0;
        logits[2150] = 7.0;
        assert_eq!(select_first_code(&logits, 3072, 2150, &[], 1.05), 2150);
    }

    #[test]
    fn generic_export_outputs_pair_cache_by_declared_order() {
        let past = vec!["past_kv".to_string(), "past_kv_0_1".to_string()];
        let outputs = vec![
            "logits_Q4".to_string(),
            "hidden_states".to_string(),
            "present".to_string(),
            "cat_62".to_string(),
        ];
        let logits = named_output(&outputs, &["logits", "logits_Q4"]).unwrap();
        let hidden = named_output(&outputs, &["hidden", "hidden_states"]).unwrap();
        assert_eq!(
            present_output_indices(&past, &outputs, &[logits, hidden]).unwrap(),
            vec![2, 3]
        );
    }

    #[test]
    fn semantic_export_outputs_pair_cache_by_name() {
        let past = vec![
            "past_key_values.0.key".to_string(),
            "past_key_values.0.value".to_string(),
        ];
        let outputs = vec![
            "present.0.value".to_string(),
            "logits".to_string(),
            "present.0.key".to_string(),
            "hidden".to_string(),
        ];
        assert_eq!(
            present_output_indices(&past, &outputs, &[1, 3]).unwrap(),
            vec![2, 0]
        );
    }

    #[test]
    fn ambiguous_cache_outputs_fail_closed() {
        let past = vec!["past_kv".to_string(), "past_kv_0_1".to_string()];
        let outputs = vec![
            "logits".to_string(),
            "hidden".to_string(),
            "present".to_string(),
        ];
        assert!(present_output_indices(&past, &outputs, &[0, 1]).is_err());
    }
}
