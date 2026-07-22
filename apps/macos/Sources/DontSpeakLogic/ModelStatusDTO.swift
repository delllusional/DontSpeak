// Hand mirror of ds-status model_status JSON. No codegen — contract test keeps lockstep.

import Foundation

/// Built-in TTS model wire token (`model_status.tts.model`).
public enum TtsModel: String, Decodable, Sendable, Equatable {
    case kokoro, chatterbox, qwen, omnivoice
}

public struct ActivityDTO: Decodable, Sendable, Equatable {
    public var caps: Bool
    public var capsActive: Bool
    public var recording: Bool
    public var speaking: Bool
    public var speaker: String?
    public var voice: String?
    public var language: String?
    public var warning: String?
    public var muted: Bool

    enum CodingKeys: String, CodingKey {
        case caps
        case capsActive = "caps_active"
        case recording
        case speaking
        case speaker
        case voice
        case language
        case warning
        case muted
    }
}

public struct DictationDTO: Decodable, Sendable, Equatable {
    public var state: String
    public var text: String
    public var canPaste: Bool

    enum CodingKeys: String, CodingKey {
        case state
        case text
        case canPaste = "can_paste"
    }
}

public struct UtteranceStatusDTO: Decodable, Sendable, Equatable {
    public var voice: String
    public var language: String
    public var warning: String?
}

public struct DownloadStatusDTO: Decodable, Sendable, Equatable {
    public var target: String
    public var doneBytes: UInt64
    public var totalBytes: UInt64
    public var startBytes: UInt64
    public var elapsedSeconds: UInt64

    enum CodingKeys: String, CodingKey {
        case target
        case doneBytes = "done_bytes"
        case totalBytes = "total_bytes"
        case startBytes = "start_bytes"
        case elapsedSeconds = "elapsed_seconds"
    }
}

public struct TtsStatusDTO: Decodable, Sendable, Equatable {
    public var engine: String
    public var model: TtsModel?
    public var language: String?
    public var provider: String?
    public var status: EngineStatusDTO?
    public var lastUtterance: UtteranceStatusDTO?

    enum CodingKeys: String, CodingKey {
        case engine, model, language, provider, status
        case lastUtterance = "last_utterance"
    }
}

public struct SttStatusDTO: Decodable, Sendable, Equatable {
    public var engine: String
    public var provider: String?
    public var status: EngineStatusDTO?
    public var voiceKey: String?

    enum CodingKeys: String, CodingKey {
        case engine, provider, status
        case voiceKey = "voice_key"
    }
}

/// Full `model_status` payload — consumer projection of the canonical `ds-status` contract.
public struct ModelStatusDTO: Decodable, Sendable, Equatable {
    public var seq: UInt64
    public var activity: ActivityDTO
    public var tts: TtsStatusDTO
    public var stt: SttStatusDTO
    public var diarization: DiarizationStatusDTO
    public var dictation: DictationDTO
    public var stats: StatsDTO
    public var tray: [String]
    public var downloads: [DownloadStatusDTO]
    public var agents: Bool

    enum CodingKeys: String, CodingKey {
        case seq
        case activity, tts, stt
        case diarization
        case dictation
        case stats
        case tray
        case downloads
        case agents
    }
}
