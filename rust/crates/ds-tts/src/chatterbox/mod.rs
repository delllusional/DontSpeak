//! Chatterbox Multilingual ONNX backend.
//!
//! Four ONNX graphs from the shared TTS asset registry on the same dynamic ORT as Kokoro:
//! transient `speech_encoder` → cached voice conditioning, then AR loop
//! `embed_tokens` → `language_model` (greedy + rep penalty, KV cache) →
//! `conditional_decoder` → 24 kHz PCM. 1:1 MIT reference pipeline.
//!
//! Plain-text frontend ([`crate::chatterbox::frontend`]); shared markdown→prose only.
//!
//! `rate` is a no-op for this backend.
//!
//! No Perth watermark (not in ONNX graphs; MIT does not require it).

pub mod frontend;
pub mod synth;
pub mod tokenizer;
