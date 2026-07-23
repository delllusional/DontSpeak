/// Engine stats from model_status `stats`, grouped to mirror the wire object.
/// Same seam as Windows HealthSnapshotTests.
public struct EngineStats: Sendable, Equatable {
    public struct Tts: Sendable, Equatable {
        public var rtfMin: Double = 0
        public var rtfAvg: Double = 0
        public var rtfMax: Double = 0
        public var firstMinMs: Double = 0
        public var firstAvgMs: Double = 0
        public var firstMaxMs: Double = 0
        public var utterances: Int = 0
        public var audioSecs: Double = 0
        public var failures: Int = 0
        /// Utterances left to say (waiting + in-flight); live, unlike its cumulative siblings.
        public var queued: UInt64 = 0
        public init() {}
    }

    public struct Stt: Sendable, Equatable {
        public var rtfMin: Double = 0
        public var rtfAvg: Double = 0
        public var rtfMax: Double = 0
        public var transcriptions: Int = 0
        public var audioSecs: Double = 0
        public var failures: Int = 0
        public init() {}
    }

    public struct Lifetime: Sendable, Equatable {
        public var ttsSecs: Double = 0
        public var sttSecs: Double = 0
        public init() {}
    }

    public var tts = Tts()
    public var stt = Stt()
    public var lifetime = Lifetime()

    public init() {}

    public static func from(_ dto: StatsDTO) -> EngineStats {
        var s = EngineStats()
        s.tts.rtfMin = dto.tts.rtfMin
        s.tts.rtfAvg = dto.tts.rtfAvg
        s.tts.rtfMax = dto.tts.rtfMax
        s.tts.firstMinMs = dto.tts.firstMinMs
        s.tts.firstAvgMs = dto.tts.firstAvgMs
        s.tts.firstMaxMs = dto.tts.firstMaxMs
        s.tts.utterances = dto.tts.utterances
        s.tts.audioSecs = dto.tts.audioSecs
        s.tts.failures = dto.tts.failures
        s.tts.queued = dto.tts.queued
        s.stt.rtfMin = dto.stt.rtfMin
        s.stt.rtfAvg = dto.stt.rtfAvg
        s.stt.rtfMax = dto.stt.rtfMax
        s.stt.transcriptions = dto.stt.transcriptions
        s.stt.audioSecs = dto.stt.audioSecs
        s.stt.failures = dto.stt.failures
        s.lifetime.ttsSecs = dto.lifetime.ttsSecs
        s.lifetime.sttSecs = dto.lifetime.sttSecs
        return s
    }
}

// MARK: - Wire DTOs

public struct EngineStatusDTO: Decodable, Sendable, Equatable {
    public var state: String
    public var progress: Double
    public var error: String?
}

/// `model_status.diarization`: lifecycle and configuration kept together on the wire.
public struct DiarizationStatusDTO: Decodable, Sendable, Equatable {
    public var status: EngineStatusDTO
    public var enabled: Bool
    /// `nil` until a diarization backend is realized.
    public var provider: String?
    public var speakers: [String]
    public var activityThreshold: Double

    enum CodingKeys: String, CodingKey {
        case status, enabled, provider, speakers
        case activityThreshold = "activity_threshold"
    }
}

// `model_status.stats`

public struct TtsStatsDTO: Decodable, Sendable, Equatable {
    public var rtfMin: Double
    public var rtfAvg: Double
    public var rtfMax: Double
    public var firstMinMs: Double
    public var firstAvgMs: Double
    public var firstMaxMs: Double
    public var utterances: Int
    public var audioSecs: Double
    public var failures: Int
    public var queued: UInt64

    enum CodingKeys: String, CodingKey {
        case rtfMin = "rtf_min"
        case rtfAvg = "rtf_avg"
        case rtfMax = "rtf_max"
        case firstMinMs = "ttfa_min_ms"
        case firstAvgMs = "ttfa_avg_ms"
        case firstMaxMs = "ttfa_max_ms"
        case utterances
        case audioSecs = "audio_secs"
        case failures
        case queued
    }
}

public struct SttStatsDTO: Decodable, Sendable, Equatable {
    public var rtfMin: Double
    public var rtfAvg: Double
    public var rtfMax: Double
    public var transcriptions: Int
    public var audioSecs: Double
    public var failures: Int

    enum CodingKeys: String, CodingKey {
        case rtfMin = "rtf_min"
        case rtfAvg = "rtf_avg"
        case rtfMax = "rtf_max"
        case transcriptions
        case audioSecs = "audio_secs"
        case failures
    }
}

/// Lifetime totals (engine u64; Double decodes JSON integers).
public struct LifetimeStatsDTO: Decodable, Sendable, Equatable {
    public var ttsSecs: Double
    public var sttSecs: Double

    enum CodingKeys: String, CodingKey {
        case ttsSecs = "tts_secs"
        case sttSecs = "stt_secs"
    }
}

public struct StatsDTO: Decodable, Sendable, Equatable {
    public var tts: TtsStatsDTO
    public var stt: SttStatsDTO
    public var lifetime: LifetimeStatsDTO
}
