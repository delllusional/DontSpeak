# Runtime matrix

Canonical map of compute runtimes to supported targets and model families. The tables
describe static build and selection support. A supported cell does not imply that its model
assets, driver, OS permission, or execution provider are available on a particular machine;
`model_status` reports the backend that was actually realized. The client-provided
`claude_code` STT engine has no local compute runtime and is outside this matrix.

## Runtime availability

| Runtime | Config token / shipped component | Pinned version | macOS arm64 | macOS x86_64 | Windows x86_64 | Windows arm64 | Linux x86_64 | Linux arm64 |
|---|---|---|---|---|---|---|---|---|
| ONNX Runtime CPU EP | `cpu` / ONNX Runtime dylib | 1.27.1; 1.23.2 on Intel macOS | TTS + STT | TTS + STT | TTS + STT | TTS + STT | TTS + STT | TTS + STT |
| ONNX Runtime CUDA EP | `cuda` / pinned CUDA wheel set | ONNX Runtime GPU 1.26.0, CUDA runtime 12.6.77 | - | - | TTS + STT | - | TTS + STT | - |
| ONNX Runtime Core ML EP | `coreml` / target ONNX Runtime dylib | same as CPU row | TTS only | TTS only | - | - | - | - |
| MLX | `mlx` / `libdontspeak_mlx.dylib` | MLX Swift 0.31.3, MLX Audio Swift 0.1.3 | TTS + STT + diarization | - | - | - | - | - |
| FluidAudio Core ML / ANE | `fluid` / `libdontspeak_fluid.dylib` | FluidAudio 0.15.5 | Kokoro TTS + STT + diarization | - | - | - | - | - |
| OS speech | `system` / OS APIs and `libdontspeak_sys.dylib` on macOS | OS-owned | TTS + STT | TTS + STT | TTS | TTS | - | - |

The CUDA row is selectable only on x86_64 Windows and Linux. Runtime use additionally
requires the pinned wheel set and a detectable NVIDIA driver; a failed execution-provider
registration falls back to the CPU EP and status reports CPU.

The macOS app packages three peer dylibs. `libdontspeak_sys.dylib` has no package
dependencies and ships on both architectures. The MLX and FluidAudio dylibs are independent,
Apple-Silicon-only families, so either can be absent without loading symbols from the other.

## Built-in TTS models

| Model | ORT CPU | ORT CUDA | ORT Core ML | MLX | FluidAudio |
|---|---|---|---|---|---|
| Kokoro | yes | yes | yes | yes | yes |
| Chatterbox Multilingual | yes | yes | - | yes | - |
| Qwen3-TTS CustomVoice | yes | yes | - | yes | - |
| OmniVoice | yes | yes | - | yes | - |

The model table is capability-only; combine it with the platform table to determine whether a
pair is selectable on a target. FluidAudio is opt-in and is not part of the default
`mlx -> cuda -> cpu` provider ladder. OmniVoice's CUDA backbone is reported as CUDA while its
Higgs decoder remains on the CPU EP.

## STT and auxiliary graphs

| Model or graph | ORT CPU | ORT CUDA | ORT Core ML | MLX | FluidAudio | OS / native |
|---|---|---|---|---|---|---|
| Parakeet TDT 0.6b v3 (25 languages) | yes | yes | - | yes | - | - |
| Parakeet TDT 0.6b v2 (English only) | - | - | - | - | yes | - |
| `SFSpeechRecognizer` | - | - | - | - | - | macOS System STT |
| Kokoro BART G2P (English OOV) | yes | yes | yes | - | - | - |
| Kokoro eSpeak frontend (es/fr/hi/it/pt) | - | - | - | - | - | `espeakng-loader` 0.2.4 |
| SepFormer speech separation | yes | - | - | - | - | - |
| Sortformer + WeSpeaker diarization | - | - | - | yes | - | - |
| pyannote + WeSpeaker diarization | - | - | - | - | yes | - |
| VAD endpointer | - | - | - | - | - | native Rust |

Kokoro's frontend is chosen before synthesis. Its English BART graphs therefore still need
ONNX Runtime when synthesis uses MLX or FluidAudio. SepFormer deliberately creates a CPU-only
session; it is used by speaker lock alongside the Apple-Silicon-only diarization paths.

## Sources of truth

- Platform and engine gates: [`ds-config/src/enums.rs`](../rust/crates/ds-config/src/enums.rs)
- Built-in TTS capabilities: [`ds-config/src/tts_model.rs`](../rust/crates/ds-config/src/tts_model.rs)
- ONNX Runtime, CUDA, and eSpeak pins: [`ds-model/src/urls.rs`](../rust/crates/ds-model/src/urls.rs)
- macOS shim dependency pins: [`DontSpeakMLX/Package.swift`](../apps/macos/DontSpeakMLX/Package.swift)
- STT routing and auxiliary graphs: [`ds-stt/src/local.rs`](../rust/crates/ds-stt/src/local.rs),
  [`streaming.rs`](../rust/crates/ds-stt/src/streaming.rs),
  [`separate.rs`](../rust/crates/ds-stt/src/separate.rs), and
  [`diarize.rs`](../rust/crates/ds-stt/src/diarize.rs)

The pure cross-platform predicate tests in `ds-config::enums` pin the platform rows. Model
descriptor tests pin TTS coverage, and `ds-model` pin tests keep the published runtime
versions consistent with the downloadable artifacts.
