// Phase 0 smoke test: prove FluidAudio's ANE Kokoro works on this Mac.
// Text -> KokoroAneManager.synthesize -> 24 kHz mono WAV. Reports init/synth
// timing + RTFx so we can confirm it's worth wiring into DontSpeak.
import FluidAudio
import Foundation

let args = Array(CommandLine.arguments.dropFirst())
let text =
    args.first
    ?? "Hello from the Apple Neural Engine. This is Kokoro running on Core ML, inside DontSpeak."
let outPath = args.count > 1 ? args[1] : "smoke.wav"

func audioSeconds(_ wav: Data) -> Double {
    // 24 kHz mono 16-bit PCM WAV: (bytes - 44-byte header) / 2 / 24000
    guard wav.count > 44 else { return 0 }
    return Double(wav.count - 44) / 2.0 / 24_000.0
}

do {
    let manager = KokoroAneManager()  // .english variant, default voice af_heart, ANE routing

    let t0 = Date()
    try await manager.initialize()
    let tInit = Date().timeIntervalSince(t0)
    print(String(format: "initialize : %.2fs", tInit))

    let t1 = Date()
    let wav = try await manager.synthesize(text: text)
    let tSyn = Date().timeIntervalSince(t1)

    try wav.write(to: URL(fileURLWithPath: outPath))
    let secs = audioSeconds(wav)
    let rtfx = tSyn > 0 ? secs / tSyn : 0
    print(String(format: "synthesize : %.2fs for %.2fs audio  => RTFx %.1fx", tSyn, secs, rtfx))
    print("wrote      : \(outPath) (\(wav.count) bytes)")
} catch {
    FileHandle.standardError.write(Data("SMOKE ERROR: \(error)\n".utf8))
    exit(1)
}
