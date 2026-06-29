# Streaming STT — validated reference

`reference_prototype.py` is a **working** Python reference (onnxruntime + kaldi-native-fbank) for
the cache-aware streaming NeMo FastConformer transducer. The Rust runner in `ds-stt` is a port of
this; keep them in sync. Run it against a downloaded model dir to regenerate ground truth.

## Model
`sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-80ms-int8`
(HF: `csukuangfj/...`). Files: `encoder.int8.onnx`, `decoder.int8.onnx`, `joiner.int8.onnx`,
`tokens.txt`. fp32 variant: drop `.int8`. 480ms / 1040ms variants exist (different latency).

## Verified ONNX contract (read dims from encoder metadata at runtime)
- **Encoder** in: `audio_signal f32 [1,80,T]`, `length i64 [1]`, `cache_last_channel f32 [1,17,70,512]`,
  `cache_last_time f32 [1,17,512,8]`, `cache_last_channel_len i64 [1]`.
  out: `outputs f32 [1,512,T']`, `encoded_lengths i64`, `cache_last_channel_next`,
  `cache_last_time_next`, `cache_last_channel_next_len`. (Thread the 3 next-caches back in.)
- **Decoder** (LSTM) in: `targets i32 [1,1]`, `target_length i32 [1]`, `states.1 f32 [1,1,640]`,
  `onnx::LSTM_3 f32 [1,1,640]`. out: `outputs f32 [1,640,1]`, lengths, 2 next states.
- **Joiner** in: `encoder_outputs f32 [1,512,1]`, `decoder_outputs f32 [1,640,1]`.
  out: `outputs f32 [1,1,1,1025]`.
- Encoder metadata: `window_size=65`, `chunk_shift=56`, `subsampling_factor=8`, `vocab_size=1024`
  (blank id = 1024, tokens.txt has 1025 lines), `pred_rnn_layers=1`, `pred_hidden=640`,
  `normalize_type=""`, cache dims `cache_last_channel_dim{1,2,3}` / `cache_last_time_dim{1,2,3}`.

## Verified feature config (THE crux — empirically nailed against the oracle)
Kaldi log-mel fbank via kaldi-native-fbank:
- waveform in **[-1, 1] (NO ×32768 scaling)** ← the key gotcha; scaling by 32768 yields all-blanks
- `samp_freq=16000`, `dither=0`, `snip_edges=false`, `mel_opts.num_bins=80`
- **raw log-mel, NO CMVN / per-feature normalization** (matches `normalize_type=""`)
- features laid out channels-first `[1, 80, T]` (mel = channel dim)

## Decode (greedy transducer)
- init decoder once with `targets=[[blank]]` → seed `decoder_out` + LSTM state.
- per encoder output column: run joiner, `argmax` over 1025; if non-blank → emit token, re-run
  decoder with it to update state, up to `max_symbols_per_frame=10`; if blank → next column.
- text = concat tokens, replace `▁` with space, trim.

## Streaming loop
Feature frames windowed `window_size=65`, advance `chunk_shift=56` (9-frame overlap); caches
zero-init (`cache_last_channel_len=0`) and threaded output→input each step. Batch is always 1.

## Oracle
`test_wavs/0.wav` (LibriSpeech 1089) → exactly:
`after early nightfall the yellow lamps would light up here and there the squalid quarter of the bros...`
Confirmed identical from (a) sherpa-onnx itself and (b) `reference_prototype.py` (scale=1.0, RAW).

## ort (Rust) gotchas
int64 for `length`/`cache_last_channel_len`; int32 for decoder `targets`/`target_length` and the
emitted token. Channels-first (don't transpose to time-major). Read cache dims from metadata, never
hardcode (80ms/1040ms differ). int8 models = dynamic-quant (CPU EP fine; for GPU prefer fp32).
