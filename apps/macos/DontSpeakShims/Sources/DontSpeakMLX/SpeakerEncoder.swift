import Accelerate
import Foundation
@preconcurrency import MLX
import MLXNN

final class WeSpeakerFeatureExtractor: @unchecked Sendable {
    private static let frameLength = 400
    private let hopLength = 160
    private let melCount = 80
    private let paddedFft = 512
    private let fftSetup: FFTSetup
    private let window: [Float]
    private let melFilterbank: [Float]

    init() {
        guard let setup = vDSP_create_fftsetup(9, FFTRadix(kFFTRadix2)) else {
            fatalError("Failed to initialize the WeSpeaker FFT")
        }
        fftSetup = setup
        window = (0..<Self.frameLength).map { index in
            0.54 - 0.46 * cos(2 * Float.pi * Float(index) / Float(Self.frameLength - 1))
        }
        melFilterbank = Self.makeMelFilterbank(melCount: 80, fftSize: 512, sampleRate: 16_000)
    }

    deinit {
        vDSP_destroy_fftsetup(fftSetup)
    }

    private static func makeMelFilterbank(
        melCount: Int, fftSize: Int, sampleRate: Int
    ) -> [Float] {
        func hzToMel(_ hz: Float) -> Float { 1127 * log(1 + hz / 700) }
        func melToHz(_ mel: Float) -> Float { 700 * (exp(mel / 1127) - 1) }

        let binCount = fftSize / 2 + 1
        let minMel = hzToMel(20)
        let maxMel = hzToMel(Float(sampleRate) / 2)
        let points = (0..<(melCount + 2)).map { index in
            melToHz(minMel + Float(index) * (maxMel - minMel) / Float(melCount + 1))
        }
        var filters = [Float](repeating: 0, count: melCount * binCount)
        for mel in 0..<melCount {
            let lower = points[mel]
            let center = points[mel + 1]
            let upper = points[mel + 2]
            for bin in 0..<binCount {
                let frequency = Float(bin * sampleRate) / Float(fftSize)
                let value: Float
                if frequency > lower, frequency < center {
                    value = (frequency - lower) / (center - lower)
                } else if frequency >= center, frequency < upper {
                    value = (upper - frequency) / (upper - center)
                } else {
                    value = 0
                }
                filters[mel * binCount + bin] = value
            }
        }
        return filters
    }

    func extract(_ audio: [Float]) throws -> MLXArray {
        guard audio.count >= Self.frameLength else { throw MlxShimError.badAudio }
        let frameCount = 1 + (audio.count - Self.frameLength) / hopLength
        let halfFft = paddedFft / 2
        let binCount = halfFft + 1
        var features = [Float](repeating: 0, count: frameCount * melCount)
        var frame = [Float](repeating: 0, count: paddedFft)
        var real = [Float](repeating: 0, count: halfFft)
        var imaginary = [Float](repeating: 0, count: halfFft)
        var power = [Float](repeating: 0, count: binCount)

        for frameIndex in 0..<frameCount {
            let start = frameIndex * hopLength
            let mean =
                audio[start..<(start + Self.frameLength)].reduce(0, +)
                / Float(Self.frameLength)
            frame[0] = (audio[start] - mean) * (1 - 0.97) * window[0]
            for index in 1..<Self.frameLength {
                let current = audio[start + index] - mean
                let previous = audio[start + index - 1] - mean
                frame[index] = (current - 0.97 * previous) * window[index]
            }
            for index in Self.frameLength..<paddedFft { frame[index] = 0 }
            for index in 0..<halfFft {
                real[index] = frame[2 * index]
                imaginary[index] = frame[2 * index + 1]
            }
            real.withUnsafeMutableBufferPointer { realBuffer in
                imaginary.withUnsafeMutableBufferPointer { imaginaryBuffer in
                    var split = DSPSplitComplex(
                        realp: realBuffer.baseAddress!, imagp: imaginaryBuffer.baseAddress!)
                    vDSP_fft_zrip(
                        fftSetup, &split, 1, 9, FFTDirection(kFFTDirection_Forward))
                }
            }
            power[0] = real[0] * real[0]
            power[halfFft] = imaginary[0] * imaginary[0]
            for bin in 1..<halfFft {
                power[bin] = real[bin] * real[bin] + imaginary[bin] * imaginary[bin]
            }
            for mel in 0..<melCount {
                var energy: Float = 0
                let filterOffset = mel * binCount
                for bin in 0..<binCount {
                    energy += power[bin] * melFilterbank[filterOffset + bin]
                }
                features[frameIndex * melCount + mel] = log(max(energy, Float.ulpOfOne))
            }
        }

        for mel in 0..<melCount {
            var mean: Float = 0
            for frameIndex in 0..<frameCount {
                mean += features[frameIndex * melCount + mel]
            }
            mean /= Float(frameCount)
            for frameIndex in 0..<frameCount {
                features[frameIndex * melCount + mel] -= mean
            }
        }
        return MLXArray(features, [frameCount, melCount])
    }
}

private func batchNormNHWC(_ norm: BatchNorm, _ x: MLXArray) -> MLXArray {
    let shape = x.shape
    let channels = shape.last ?? 0
    return norm(x.reshaped([-1, channels])).reshaped(shape)
}

final class SpeakerBasicBlock: Module {
    @ModuleInfo var conv1: Conv2d
    @ModuleInfo var bn1: BatchNorm
    @ModuleInfo var conv2: Conv2d
    @ModuleInfo var bn2: BatchNorm
    @ModuleInfo var shortcut: [Module]

    private let hasShortcut: Bool

    init(inputChannels: Int, outputChannels: Int, stride: Int) {
        hasShortcut = stride != 1 || inputChannels != outputChannels
        _conv1.wrappedValue = Conv2d(
            inputChannels: inputChannels,
            outputChannels: outputChannels,
            kernelSize: .init((3, 3)),
            stride: .init((stride, stride)),
            padding: .init((1, 1)),
            bias: false)
        _bn1.wrappedValue = BatchNorm(featureCount: outputChannels)
        _conv2.wrappedValue = Conv2d(
            inputChannels: outputChannels,
            outputChannels: outputChannels,
            kernelSize: .init((3, 3)),
            padding: .init((1, 1)),
            bias: false)
        _bn2.wrappedValue = BatchNorm(featureCount: outputChannels)
        if hasShortcut {
            _shortcut.wrappedValue = [
                Conv2d(
                    inputChannels: inputChannels,
                    outputChannels: outputChannels,
                    kernelSize: .init((1, 1)),
                    stride: .init((stride, stride)),
                    bias: false),
                BatchNorm(featureCount: outputChannels),
            ]
        } else {
            _shortcut.wrappedValue = []
        }
    }

    func callAsFunction(_ x: MLXArray) -> MLXArray {
        var out = relu(batchNormNHWC(bn1, conv1(x)))
        out = batchNormNHWC(bn2, conv2(out))
        var identity = x
        if hasShortcut,
            let conv = shortcut.first as? Conv2d,
            let norm = shortcut.dropFirst().first as? BatchNorm
        {
            identity = batchNormNHWC(norm, conv(identity))
        }
        return relu(out + identity)
    }
}

/// MLX port of the MIT-licensed WeSpeaker ResNet34 checkpoint used for identity matching.
final class SpeakerEncoder: Module, @unchecked Sendable {
    @ModuleInfo var conv1: Conv2d
    @ModuleInfo var bn1: BatchNorm
    @ModuleInfo var layer1: [SpeakerBasicBlock]
    @ModuleInfo var layer2: [SpeakerBasicBlock]
    @ModuleInfo var layer3: [SpeakerBasicBlock]
    @ModuleInfo var layer4: [SpeakerBasicBlock]
    @ModuleInfo var fc: Linear
    private let featureExtractor = WeSpeakerFeatureExtractor()

    override init() {
        _conv1.wrappedValue = Conv2d(
            inputChannels: 1, outputChannels: 32,
            kernelSize: .init((3, 3)), padding: .init((1, 1)), bias: false)
        _bn1.wrappedValue = BatchNorm(featureCount: 32)
        _layer1.wrappedValue = Self.makeLayer(
            inputChannels: 32, outputChannels: 32, count: 3, stride: 1)
        _layer2.wrappedValue = Self.makeLayer(
            inputChannels: 32, outputChannels: 64, count: 4, stride: 2)
        _layer3.wrappedValue = Self.makeLayer(
            inputChannels: 64, outputChannels: 128, count: 6, stride: 2)
        _layer4.wrappedValue = Self.makeLayer(
            inputChannels: 128, outputChannels: 256, count: 3, stride: 2)
        _fc.wrappedValue = Linear(5120, 256)
    }

    private static func makeLayer(
        inputChannels: Int, outputChannels: Int, count: Int, stride: Int
    ) -> [SpeakerBasicBlock] {
        var blocks = [
            SpeakerBasicBlock(
                inputChannels: inputChannels, outputChannels: outputChannels, stride: stride)
        ]
        for _ in 1..<count {
            blocks.append(
                SpeakerBasicBlock(
                    inputChannels: outputChannels, outputChannels: outputChannels, stride: 1))
        }
        return blocks
    }

    static func load(from directory: URL) throws -> SpeakerEncoder {
        let model = SpeakerEncoder()
        let raw = try MLX.loadArrays(url: directory.appendingPathComponent("weights.npz"))
        var weights = [String: MLXArray]()
        for (key, value) in raw {
            if let mapped = mappedCheckpointKey(key) {
                weights[mapped] = value
            }
        }
        try model.update(parameters: ModuleParameters.unflattened(weights), verify: .all)
        model.train(false)
        eval(model.parameters())
        return model
    }

    static func mappedCheckpointKey(_ key: String) -> String? {
        guard key.hasPrefix("resnet.") else { return nil }
        return String(key.dropFirst("resnet.".count))
            .replacingOccurrences(of: "seg_1.", with: "fc.")
    }

    func embed(_ samples: [Float]) throws -> [Float] {
        let features = try featureExtractor.extract(samples)
        var x = features.expandedDimensions(axis: -1).expandedDimensions(axis: 0)
        x = x.transposed(0, 2, 1, 3)
        x = relu(batchNormNHWC(bn1, conv1(x)))
        for block in layer1 { x = block(x) }
        for block in layer2 { x = block(x) }
        for block in layer3 { x = block(x) }
        for block in layer4 { x = block(x) }

        let mean = x.mean(axis: 2).transposed(0, 2, 1)
        let standardDeviation = MLX.sqrt(x.variance(axis: 2) + 1e-7).transposed(0, 2, 1)
        let meanFlat = mean.reshaped([mean.dim(0), -1])
        let standardDeviationFlat = standardDeviation.reshaped([standardDeviation.dim(0), -1])
        let stats = MLX.concatenated([meanFlat, standardDeviationFlat], axis: 1)
        var embedding = fc(stats)[0]
        embedding /= MLX.sqrt((embedding * embedding).sum() + 1e-12)
        eval(embedding)
        return embedding.asArray(Float.self)
    }
}
