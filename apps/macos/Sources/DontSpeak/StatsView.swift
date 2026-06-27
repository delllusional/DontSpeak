//  StatsView.swift
//
//  Live engine stats for power users: the TTS realtime factor (synth time vs
//  audio produced — the speech analog of tokens/sec), time-to-first-audio, how
//  much has been spoken, failures, and which models are resident. All measured by
//  the engine and read from the same model_status push (no extra round-trip).

import SwiftUI

/// Engine stats parsed from model_status `stats`. Grouped into cohesive nested
/// sub-structs that mirror the `stats` wire object (tts / stt / diarization /
/// lifetime / loaded), so each reader binds the one block it cares about.
struct EngineStats: Sendable, Equatable {
    /// Text-to-speech (Kokoro): realtime factor, time-to-first-audio, and totals.
    struct Tts: Sendable, Equatable {
        var rtfAvg: Double = 0
        var rtfMin: Double = 0
        var rtfMax: Double = 0
        var firstAvgMs: Double = 0
        var firstMinMs: Double = 0
        var firstMaxMs: Double = 0
        var utterances: Int = 0
        var audioSecs: Double = 0
        var failures: Int = 0
    }

    /// Speech-to-text (Parakeet), through the same helper.
    struct Stt: Sendable, Equatable {
        var rtfAvg: Double = 0
        var rtfMin: Double = 0
        var rtfMax: Double = 0
        var transcriptions: Int = 0
        var audioSecs: Double = 0
    }

    /// Speaker diarization (on-demand): whether enabled + the enrolled voiceprint names
    /// it can label, the live clustering threshold (lower = more speakers split), and the
    /// resolved runtime token (tts_provider/stt_provider vocabulary) — empty when absent.
    struct Diar: Sendable, Equatable {
        var enabled = false
        var speakers: [String] = []
        var clusteringThreshold: Double = 0.7
        var runtime = ""
    }

    /// Persisted lifetime totals (seconds), summed across all sessions.
    struct Lifetime: Sendable, Equatable {
        var ttsSecs: Double = 0
        var sttSecs: Double = 0
    }

    /// Which models are currently resident in the warm helper.
    struct Loaded: Sendable, Equatable {
        var tts = false
        var stt = false
    }

    var tts = Tts()
    var stt = Stt()
    var diarization = Diar()
    var lifetime = Lifetime()
    var loaded = Loaded()

    /// Map the decoded `stats` DTO into `EngineStats`. A nil block (absent in the JSON,
    /// or the whole status missing) leaves every field at its struct default; a present
    /// block with a missing/null leaf falls back to the SAME per-field default the old
    /// `[String: Any]` path used (numbers → 0, flags → false, speakers → []). Note
    /// `clusteringThreshold` lands on 0 (not the 0.7 struct default) once a `diarization`
    /// block is present but omits the key — matching the old behavior exactly.
    static func from(_ dto: StatsDTO?) -> EngineStats {
        var s = EngineStats()
        guard let dto else { return s }
        if let t = dto.tts {
            s.tts.rtfAvg = t.rtfAvg ?? 0; s.tts.rtfMin = t.rtfMin ?? 0; s.tts.rtfMax = t.rtfMax ?? 0
            s.tts.firstAvgMs = t.firstAvgMs ?? 0; s.tts.firstMinMs = t.firstMinMs ?? 0; s.tts.firstMaxMs = t.firstMaxMs ?? 0
            s.tts.utterances = t.utterances ?? 0
            s.tts.audioSecs = t.audioSecs ?? 0
            s.tts.failures = t.failures ?? 0
        }
        if let t = dto.stt {
            s.stt.rtfAvg = t.rtfAvg ?? 0; s.stt.rtfMin = t.rtfMin ?? 0; s.stt.rtfMax = t.rtfMax ?? 0
            s.stt.transcriptions = t.transcriptions ?? 0
            s.stt.audioSecs = t.audioSecs ?? 0
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
        if let l = dto.loaded {
            s.loaded.tts = l.tts ?? false
            s.loaded.stt = l.stt ?? false
        }
        return s
    }
}

/// A min–avg–max metric on ONE clean row: the title on the left; on the right the
/// AVERAGE (with its unit) followed by the min–max range as a lighter secondary
/// qualifier — no units on the range, since it shares the average's. `fmt` formats
/// each number; `unit` (e.g. "×", " s") is appended to the average only. Styled to
/// match the panel's other rows (primary value + subheadline secondary qualifier).
struct StatRange: View {
    let title: String
    let lo: Double
    let avg: Double
    let hi: Double
    var unit: String = ""
    let fmt: (Double) -> String

    var body: some View {
        LabeledContent {
            HStack(spacing: 6) {
                Text(fmt(avg) + unit)
                Text("\(fmt(lo))–\(fmt(hi))")
                    .glassRowDetail()
            }
            .monospacedDigit()
        } label: {
            Text(title).glassRowTitle()
        }
    }
}

// The engine-stats UI lives in the Status window (StatusView): per-engine stats
// expand under the TTS/STT rows. This file just holds the shared types above
// (EngineStats + StatRange) that StatusView renders.
