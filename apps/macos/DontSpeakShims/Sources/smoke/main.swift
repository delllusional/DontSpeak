// Smoke: local MLX Audio Kokoro model → 24 kHz WAV.
import AVFoundation
import Foundation
@preconcurrency import MLX
import MLXAudioTTS

let args = Array(CommandLine.arguments.dropFirst())
guard let modelPath = args.first else {
    FileHandle.standardError.write(Data("usage: smoke MODEL_DIR [IPA] [OUT.wav]\n".utf8))
    exit(2)
}
let phonemes = args.count > 1 ? args[1] : "həlˈoʊ fɹəm ɛm ɛl ɛks ˈɔːdiˌoʊ"
let outPath = args.count > 2 ? args[2] : "smoke.wav"

do {
    let t0 = Date()
    let model = try await KokoroModel.fromModelDirectory(
        URL(fileURLWithPath: modelPath), textProcessor: nil)
    print(String(format: "initialize : %.2fs", Date().timeIntervalSince(t0)))

    let t1 = Date()
    let audio = try await model.generate(
        text: phonemes, voice: "af_heart", refAudio: nil, refText: nil,
        language: nil, generationParameters: model.defaultGenerationParameters)
    eval(audio)
    let samples = audio.asArray(Float.self)
    let elapsed = Date().timeIntervalSince(t1)

    guard let format = AVAudioFormat(
        commonFormat: .pcmFormatFloat32, sampleRate: 24_000, channels: 1,
        interleaved: false),
        let buffer = AVAudioPCMBuffer(
            pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)),
        let channel = buffer.floatChannelData?.pointee
    else {
        throw NSError(domain: "smoke", code: 1)
    }
    buffer.frameLength = AVAudioFrameCount(samples.count)
    samples.withUnsafeBufferPointer { source in
        channel.update(from: source.baseAddress!, count: samples.count)
    }
    let output = URL(fileURLWithPath: outPath)
    let file = try AVAudioFile(forWriting: output, settings: format.settings)
    try file.write(from: buffer)

    let seconds = Double(samples.count) / 24_000
    let rtfx = elapsed > 0 ? seconds / elapsed : 0
    print(String(format: "synthesize : %.2fs for %.2fs audio => RTFx %.1fx", elapsed, seconds, rtfx))
    print("wrote      : \(outPath) (\(samples.count) samples)")
} catch {
    FileHandle.standardError.write(Data("SMOKE ERROR: \(error)\n".utf8))
    exit(1)
}
