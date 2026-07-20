import XCTest
import MLX

@testable import DontSpeakMLX

final class SpeakerEncoderTests: XCTestCase {
    func testOfficialFrameGeometryAndCepstralMeanNormalization() throws {
        let audio = (0..<16_000).map { index in
            Float(sin(2 * Double.pi * 440 * Double(index) / 16_000))
        }
        let features = try WeSpeakerFeatureExtractor().extract(audio)
        XCTAssertEqual(features.shape, [98, 80])
        let values = features.asArray(Float.self)
        for mel in 0..<80 {
            let mean =
                stride(from: mel, to: values.count, by: 80)
                .reduce(Float(0)) { $0 + values[$1] } / 98
            XCTAssertEqual(mean, 0, accuracy: 1e-4)
        }
    }

    func testCheckpointKeyMapping() {
        XCTAssertEqual(SpeakerEncoder.mappedCheckpointKey("resnet.conv1.weight"), "conv1.weight")
        XCTAssertEqual(SpeakerEncoder.mappedCheckpointKey("resnet.seg_1.weight"), "fc.weight")
        XCTAssertNil(SpeakerEncoder.mappedCheckpointKey("projection.weight"))
    }

    func testSeededNetworkProducesStableNormalizedCosine() throws {
        MLXRandom.seed(42)
        let encoder = SpeakerEncoder()
        encoder.train(false)

        let low = (0..<16_000).map { index in
            Float(sin(2 * Double.pi * 220 * Double(index) / 16_000))
        }
        let high = (0..<16_000).map { index in
            Float(sin(2 * Double.pi * 880 * Double(index) / 16_000))
        }
        let lowEmbedding = try encoder.embed(low)
        let highEmbedding = try encoder.embed(high)
        let lowNorm = sqrt(lowEmbedding.reduce(Float(0)) { $0 + $1 * $1 })
        let highNorm = sqrt(highEmbedding.reduce(Float(0)) { $0 + $1 * $1 })
        let cosine = zip(lowEmbedding, highEmbedding).reduce(Float(0)) { $0 + $1.0 * $1.1 }

        XCTAssertEqual(lowEmbedding.count, 256)
        XCTAssertEqual(lowNorm, 1, accuracy: 1e-4)
        XCTAssertEqual(highNorm, 1, accuracy: 1e-4)
        XCTAssertEqual(cosine, 0.89556944, accuracy: 1e-4)
    }
}
