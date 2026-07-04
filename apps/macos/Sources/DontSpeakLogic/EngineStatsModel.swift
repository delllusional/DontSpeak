//  EngineStatsModel.swift
//
//  Pure model for the engine's `stats` push: the wire DTOs (model_status `stats`) and
//  the `EngineStats` groups the UI binds, plus the DTO→stats mapping with its per-field
//  defaulting rules. Framework-free (Codable is the Swift standard library) and split
//  into this target so the mapping is unit-testable without the Rust FFI staticlib —
//  the same seam the Windows app covers in winui.tests/HealthSnapshotTests.

/// Engine stats parsed from model_status `stats`. Grouped into cohesive nested
/// sub-structs that mirror the `stats` wire object (tts / stt / diarization /
/// lifetime), so each reader binds the one block it cares about.
public struct EngineStats: Sendable, Equatable {
    /// Text-to-speech (Kokoro): realtime factor, time-to-first-audio, and totals.
    public struct Tts: Sendable, Equatable {
        public var rtfAvg: Double = 0
        public var rtfMin: Double = 0
        public var rtfMax: Double = 0
        public var firstAvgMs: Double = 0
        public var firstMinMs: Double = 0
        public var firstMaxMs: Double = 0
        public var utterances: Int = 0
        public var audioSecs: Double = 0
        public var failures: Int = 0
        public init() {}
    }

    /// Speech-to-text (Parakeet), through the same helper.
    public struct Stt: Sendable, Equatable {
        public var rtfAvg: Double = 0
        public var rtfMin: Double = 0
        public var rtfMax: Double = 0
        public var transcriptions: Int = 0
        public var audioSecs: Double = 0
        public var failures: Int = 0
        public init() {}
    }

    /// Speaker diarization (on-demand): whether enabled + the enrolled voiceprint names
    /// it can label, the live clustering threshold (lower = more speakers split), and the
    /// resolved runtime token (tts_provider/stt_provider vocabulary) — empty when absent.
    public struct Diar: Sendable, Equatable {
        public var enabled = false
        public var speakers: [String] = []
        public var clusteringThreshold: Double = 0.7
        public var runtime = ""
        public init() {}
    }

    /// Persisted lifetime totals (seconds), summed across all sessions.
    public struct Lifetime: Sendable, Equatable {
        public var ttsSecs: Double = 0
        public var sttSecs: Double = 0
        public init() {}
    }

    public var tts = Tts()
    public var stt = Stt()
    public var diarization = Diar()
    public var lifetime = Lifetime()

    public init() {}

    /// Map the decoded `stats` DTO into `EngineStats`. A nil block (absent in the JSON,
    /// or the whole status missing) leaves every field at its struct default; a present
    /// block with a missing/null leaf falls back to the SAME per-field default the old
    /// `[String: Any]` path used (numbers → 0, flags → false, speakers → []). Note
    /// `clusteringThreshold` lands on 0 (not the 0.7 struct default) once a `diarization`
    /// block is present but omits the key — matching the old behavior exactly.
    public static func from(_ dto: StatsDTO?) -> EngineStats {
        var s = EngineStats()
        guard let dto else { return s }
        if let t = dto.tts {
            s.tts.rtfAvg = t.rtfAvg ?? 0
            s.tts.rtfMin = t.rtfMin ?? 0
            s.tts.rtfMax = t.rtfMax ?? 0
            s.tts.firstAvgMs = t.firstAvgMs ?? 0
            s.tts.firstMinMs = t.firstMinMs ?? 0
            s.tts.firstMaxMs = t.firstMaxMs ?? 0
            s.tts.utterances = t.utterances ?? 0
            s.tts.audioSecs = t.audioSecs ?? 0
            s.tts.failures = t.failures ?? 0
        }
        if let t = dto.stt {
            s.stt.rtfAvg = t.rtfAvg ?? 0
            s.stt.rtfMin = t.rtfMin ?? 0
            s.stt.rtfMax = t.rtfMax ?? 0
            s.stt.transcriptions = t.transcriptions ?? 0
            s.stt.audioSecs = t.audioSecs ?? 0
            s.stt.failures = t.failures ?? 0
        }
        if let d = dto.diarization {
            s.diarization.enabled = d.enabled ?? false
            s.diarization.speakers = d.speakers ?? []
            s.diarization.clusteringThreshold = d.clusteringThreshold ?? 0
            s.diarization.runtime = d.runtime ?? ""
        }
        if let l = dto.lifetime {
            s.lifetime.ttsSecs = l.ttsSecs ?? 0
            s.lifetime.sttSecs = l.sttSecs ?? 0
        }
        return s
    }
}

// ── Wire DTOs (model_status `stats`) ──────────────────────────────────────────────

/// TTS rolling-stat block.
public struct TtsStatsDTO: Decodable {
    public var rtfAvg: Double?
    public var rtfMin: Double?
    public var rtfMax: Double?
    public var firstAvgMs: Double?
    public var firstMinMs: Double?
    public var firstMaxMs: Double?
    public var utterances: Int?
    public var audioSecs: Double?
    public var failures: Int?

    enum CodingKeys: String, CodingKey {
        case rtfAvg = "rtf_avg"
        case rtfMin = "rtf_min"
        case rtfMax = "rtf_max"
        case firstAvgMs = "first_avg_ms"
        case firstMinMs = "first_min_ms"
        case firstMaxMs = "first_max_ms"
        case utterances
        case audioSecs = "audio_secs"
        case failures
    }
}

/// STT rolling-stat block.
public struct SttStatsDTO: Decodable {
    public var rtfAvg: Double?
    public var rtfMin: Double?
    public var rtfMax: Double?
    public var transcriptions: Int?
    public var audioSecs: Double?
    public var failures: Int?

    enum CodingKeys: String, CodingKey {
        case rtfAvg = "rtf_avg"
        case rtfMin = "rtf_min"
        case rtfMax = "rtf_max"
        case transcriptions
        case audioSecs = "audio_secs"
        case failures
    }
}

/// Lifetime totals (engine emits u64; `Double` decodes a JSON integer fine).
public struct LifetimeStatsDTO: Decodable {
    public var ttsSecs: Double?
    public var sttSecs: Double?

    enum CodingKeys: String, CodingKey {
        case ttsSecs = "tts_secs"
        case sttSecs = "stt_secs"
    }
}

/// Speaker-diarization stat block. The wire also carries `present`, `speaker_threshold`
/// and a `loaded` block — nothing in this app renders them, so they aren't decoded.
public struct DiarizationStatsDTO: Decodable {
    public var enabled: Bool?
    public var runtime: String?
    public var speakers: [String]?
    public var clusteringThreshold: Double?

    enum CodingKeys: String, CodingKey {
        case enabled
        case runtime
        case speakers
        case clusteringThreshold = "clustering_threshold"
    }
}

/// The `stats` block. Mapped into `EngineStats` by `EngineStats.from(_:)`.
public struct StatsDTO: Decodable {
    public var tts: TtsStatsDTO?
    public var stt: SttStatsDTO?
    public var lifetime: LifetimeStatsDTO?
    public var diarization: DiarizationStatsDTO?
}
