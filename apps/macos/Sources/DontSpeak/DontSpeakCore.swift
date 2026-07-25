// Read-only status bridge over ds-core. Control lives elsewhere; model_status JSON → EngineStatus.

import AVFoundation  // AVCaptureDevice
import AppKit
import ApplicationServices  // AXIsProcessTrusted
import CDontSpeak
import DontSpeakLogic
import Foundation
import Observation

/// Owned char* → String + free.
private func takeCString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

enum Grant: Sendable, Equatable {
    case granted, denied, unknown
}

struct Perms: Sendable, Equatable {
    // Accessibility also covers Caps key reads — no separate Input Monitoring grant.
    var accessibility = Grant.unknown
    var microphone = Grant.unknown
}

struct Activity: Sendable, Equatable {
    var engineRunning = false
    var capsActive = false
    /// Raw `caps` (pre-permission). `true` + `capsActive == false` ⇒ blocked by missing grant.
    var capsEnabled = false
    var recording = false
    var speaking = false
    /// Wired client of the in-flight TTS utterance (`claude`/…); nil when unattributed.
    var speakingSource: String? = nil
    /// Playback continues but silenced; menu-bar slash.
    var muted = false
    /// Color/animate tokens; [] = never color. Default: static mic + breathing voice.
    var trayIndicator = ["stt", "tts_animated"]
}

struct TtsEngine: Sendable, Equatable {
    var engine = "off"
    var model: TtsModel? = nil
    var language: String? = nil
    var provider: String? = nil
    var status: EngineStatus = .missing
}

struct SttEngine: Sendable, Equatable {
    var engine = "off"
    var provider: String? = nil
    var status: EngineStatus = .missing
    /// Bound Claude Code voice key label; nil unless claude_code active + usable.
    var delegationKey: String? = nil
}

struct Diarization: Sendable, Equatable {
    var status: EngineStatus = .missing
    var enabled = false
    var provider: String? = nil
    var speakers: [String] = []
    var activityThreshold = 0.5
}

/// Dictation overlay state for DictationPanelController.
struct Dictation: Sendable, Equatable {
    var text = ""
    /// Editable field focused for paste? False → warning glow. Default true (fail-open).
    var hasTarget = true
    /// Canonical token (ds-status dictation_state): hidden|recording|awaiting_confirm|refused.
    var state = "hidden"
    var externalUiActive = false
}

struct HealthSnapshot: Sendable, Equatable {
    var activity = Activity()
    var tts = TtsEngine()
    var stt = SttEngine()
    var diarization = Diarization()
    var dictation = Dictation()
    var stats = EngineStats()
    var perms = Perms()
    /// Config `agents` gate; nil when the snapshot is undecodable / engine down
    /// (host keeps last known instead of hiding the tab on a blip).
    var agentsEnabled: Bool?
}

/// Read-only health bridge. Main-actor mutations; blocking FFI off-thread → Sendable snapshot.
@Observable @MainActor
final class Core {
    /// Config `agents` gate; hides the Agents tab and its sidebar entry when off.
    var agentsEnabled: Bool
    /// Survives tray hide/reopen. Starts on Agents when the gate is on.
    var screen: AppScreen

    var activity = Activity()
    /// Agents heard this launch. The Agents tab keeps a card for them even when the
    /// provider publishes no quota; view state alone wouldn't survive a tab switch.
    var spokenAgents: Set<String> = []
    var tts = TtsEngine()
    var stt = SttEngine()
    var diarization = Diarization()
    var dictation = Dictation()
    var stats = EngineStats()
    /// OS grants polled separately — engine can't observe System Settings.
    var perms = Perms()

    @ObservationIgnored private var micAccessRequested = false

    /// Process-constant (avoid per-render FFI).
    @ObservationIgnored let version: String = {
        guard let ptr = ds_version() else { return L.t("common.dash") }
        defer { ds_string_free(ptr) }
        return String(cString: ptr)
    }()

    /// Once per launch; fail-quiet until resolved.
    var updateAvailable = false
    var latestVersion: String?

    @ObservationIgnored private(set) var trayAnimator: TrayAnimator!

    @ObservationIgnored private var statusTask: Task<Void, Never>?
    /// ~3s poll — grants aren't on the status push.
    @ObservationIgnored private var permsTask: Task<Void, Never>?
    @ObservationIgnored private var continuation: AsyncStream<HealthSnapshot>.Continuation?
    /// Blocks in ds_model_status_wait (raw Thread — never Task/cooperative pool).
    /// nonisolated(unsafe): main-actor write once; deinit cancel is thread-safe.
    @ObservationIgnored private nonisolated(unsafe) var pushThread: Thread?

    init() {
        let agents = ds_agents_ui_enabled() != 0
        agentsEnabled = agents
        screen = agents ? .agents : .status

        // Prime from non-blocking probe; first stream value can wait ~1s.
        let snap = Core.probe()
        apply(snap)
        perms = snap.perms

        // bufferingNewest(1): no stale backlog.
        let (stream, cont) = AsyncStream<HealthSnapshot>.makeStream(bufferingPolicy: .bufferingNewest(1))
        continuation = cont
        startStatusProducer(cont)
        statusTask = Task { [weak self] in
            for await snap in stream {
                guard let self else { break }
                self.apply(snap)
            }
        }

        // Grants not pushable — poll ~3s. refresh() re-reads on return from Settings.
        permsTask = Task { [weak self] in
            while !Task.isCancelled {
                let p = await Task.detached { Core.probePerms() }.value
                guard let self else { return }
                // != guard: @Observable fires on every assign without equality short-circuit.
                if self.perms != p { self.perms = p }
                try? await Task.sleep(for: .seconds(3))
            }
        }

        trayAnimator = TrayAnimator(core: self)
        // Blocking network GET — off main.
        checkForUpdateOnce()
    }

    /// `ds_tray_icon_kind`.
    nonisolated static func trayIconKind(
        sttActive: Bool, ttsActive: Bool, trayIndicator: [String]
    ) -> String {
        let jsonData = (try? JSONSerialization.data(withJSONObject: trayIndicator)) ?? Data("[]".utf8)
        let json = String(data: jsonData, encoding: .utf8) ?? "[]"
        return json.withCString { jp in
            guard let ptr = ds_tray_icon_kind(sttActive ? 1 : 0, ttsActive ? 1 : 0, jp) else {
                return "idle"
            }
            defer { ds_string_free(ptr) }
            return String(cString: ptr)
        }
    }

    /// Failures → no pill (FFI returns `{}`).
    private func checkForUpdateOnce() {
        Task { [weak self] in
            let result = await Task.detached { () -> (Bool, String?) in
                guard let ptr = ds_update_check_json() else { return (false, nil) }
                defer { ds_string_free(ptr) }
                let json = String(cString: ptr)
                guard let data = json.data(using: .utf8),
                    let obj = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                else { return (false, nil) }
                let available = (obj["update_available"] as? Bool) ?? false
                return (available, available ? obj["latest_version"] as? String : nil)
            }.value
            guard let self else { return }
            self.updateAvailable = result.0
            self.latestVersion = result.1
        }
    }

    deinit {
        statusTask?.cancel()
        permsTask?.cancel()
        continuation?.finish()
        pushThread?.cancel()
    }

    /// Block in WaitModelStatus → yield. Engine down: yield once then 0.4s pace.
    private func startStatusProducer(_ cont: AsyncStream<HealthSnapshot>.Continuation) {
        let t = Thread {
            var since: UInt64 = 0  // 0 ⇒ immediate current state
            var delivered = false
            var lastRunning = true
            while !Thread.current.isCancelled {
                let (snap, seq) = Core.probeStatusWait(since)
                let running = snap.activity.engineRunning
                // Idle timeout keeps seq unchanged — statusShouldYield dedups (incl. engineRunning
                // flip, external to gate seq). See StatusYield.swift.
                if statusShouldYield(
                    delivered: delivered,
                    seq: seq,
                    since: since,
                    running: running,
                    lastRunning: lastRunning
                ) {
                    cont.yield(snap)
                    delivered = true
                    lastRunning = running
                }
                since = seq
                if !running {
                    Thread.sleep(forTimeInterval: 0.4)
                }
            }
            cont.finish()
        }
        t.name = "status-push"
        pushThread = t
        t.start()
    }

    /// Assign only when != so @Observable invalidation stays granular. Perms polled separately.
    private func apply(_ s: HealthSnapshot) {
        if activity != s.activity { activity = s.activity }
        if let speaker = s.activity.speakingSource, !spokenAgents.contains(speaker) {
            spokenAgents.insert(speaker)
        }
        if tts != s.tts { tts = s.tts }
        if stt != s.stt { stt = s.stt }
        if diarization != s.diarization { diarization = s.diarization }
        if dictation != s.dictation { dictation = s.dictation }
        if stats != s.stats { stats = s.stats }
        // nil = undecodable snapshot / engine down: keep last known instead of hiding on a blip.
        if let a = s.agentsEnabled, a != agentsEnabled {
            agentsEnabled = a
            // Same main-actor update: sidebar row disappears and detail leaves Agents together.
            if !a && screen == .agents { screen = .status }
        }
        maybeRequestMicAccess()
        DictationPanelController.shared.apply(
            state: s.dictation.externalUiActive ? "hidden" : s.dictation.state,
            text: s.dictation.text,
            hasTarget: s.dictation.hasTarget
        )
    }

    /// Prompt mic TCC once for capture engines (see `dontSpeakUsesMicrophone`). Fresh install has no
    /// Privacy→Microphone row otherwise; dialog only while .notDetermined.
    private func maybeRequestMicAccess() {
        guard !micAccessRequested,
            dontSpeakUsesMicrophone(sttEngine: stt.engine)
        else { return }
        guard AVCaptureDevice.authorizationStatus(for: .audio) == .notDetermined else {
            // Latch: skip TCC re-read on every status push after decided.
            micAccessRequested = true
            return
        }
        micAccessRequested = true
        AVCaptureDevice.requestAccess(for: .audio) { _ in }
    }

    /// Includes perms (return from System Settings / after download).
    func refresh() {
        Task { [weak self] in
            let snap = await Task.detached { Core.probe() }.value
            guard let self else { return }
            self.apply(snap)
            self.perms = snap.perms
        }
    }

    func setMuted(_ on: Bool) {
        Task { [weak self] in
            await Task.detached { _ = ds_set_muted(on ? 1 : 0) }.value
            self?.refresh()
        }
    }

    nonisolated static func probe() -> HealthSnapshot {
        var s = probeStatus()
        s.perms = probePerms()
        return s
    }

    /// Status without perms; pure w.r.t. actor (safe detached).
    nonisolated static func probeStatus() -> HealthSnapshot {
        let running = ds_engine_running_global() != 0
        var s = decodeStatus(takeCString(ds_model_status_json()))?.0 ?? HealthSnapshot()
        s.activity.engineRunning = running
        return s
    }

    /// BLOCKING WaitModelStatus (~1s). Push thread only — never main/cooperative pool.
    nonisolated static func probeStatusWait(_ since: UInt64) -> (HealthSnapshot, UInt64) {
        let running = ds_engine_running_global() != 0
        var s = HealthSnapshot()
        var seq = since
        if let (snap, decodedSeq) = decodeStatus(takeCString(ds_model_status_wait(since, 1000))) {
            s = snap
            seq = decodedSeq ?? since
        }
        s.activity.engineRunning = running
        return (s, seq)
    }

    /// Optional fields → per-field defaults. Caller sets engineRunning from global pidfile probe.
    private nonisolated static func decodeStatus(_ json: String?) -> (HealthSnapshot, UInt64?)? {
        guard let json,
            let dto = try? JSONDecoder().decode(ModelStatusDTO.self, from: Data(json.utf8))
        else { return nil }
        var s = HealthSnapshot()
        s.tts.engine = dto.tts.engine
        s.tts.model = dto.tts.model
        s.tts.language = dto.tts.language
        s.tts.provider = dto.tts.provider
        s.tts.status = dto.tts.status.engineStatus
        s.stt.engine = dto.stt.engine
        s.stt.provider = dto.stt.provider
        s.stt.status = dto.stt.status.engineStatus
        s.stt.delegationKey = dto.stt.voiceKey
        s.diarization.status = dto.diarization.status.engineStatus
        s.diarization.enabled = dto.diarization.enabled
        s.diarization.provider = dto.diarization.provider
        s.diarization.speakers = dto.diarization.speakers
        s.diarization.activityThreshold = dto.diarization.activityThreshold
        s.activity.capsActive = dto.activity.capsActive
        s.activity.capsEnabled = dto.activity.caps
        s.activity.recording = dto.activity.recording
        s.activity.speaking = dto.activity.speaking
        s.activity.speakingSource = dto.activity.speaking ? dto.activity.speaker : nil
        s.activity.muted = dto.activity.muted
        s.activity.trayIndicator = dto.tray
        s.stats = EngineStats.from(dto.stats)
        s.dictation.state = dto.dictation.state
        s.dictation.text = dto.dictation.text
        s.dictation.hasTarget = dto.dictation.canPaste
        s.dictation.externalUiActive = dto.dictation.externalUiActive ?? false
        s.agentsEnabled = dto.agents
        return (s, dto.seq)
    }

    nonisolated static func probePerms() -> Perms {
        var p = Perms()
        p.accessibility = AXIsProcessTrusted() ? .granted : .denied
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: p.microphone = .granted
        case .notDetermined: p.microphone = .unknown
        default: p.microphone = .denied
        }
        return p
    }

    func openHomepage() {
        guard let ptr = ds_homepage_url() else { return }
        defer { ds_string_free(ptr) }
        if let url = URL(string: String(cString: ptr)) { NSWorkspace.shared.open(url) }
    }

    func openPrivacyPane(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    func openSpokenContentSettings() {
        Task.detached { _ = ds_open_voice_settings() }
    }
}

// MARK: - model_status DTO → EngineStatus

extension EngineStatusDTO {
    var engineStatus: EngineStatus {
        switch state {
        case "downloading": return .downloading(progress)
        case "warming": return .warming
        case "blocked": return .blocked
        case "running": return .running
        case "failed": return .failed(error ?? L.t("status.engine.reason.default"))
        case "idle": return .idle
        default: return .missing
        }
    }
}

extension Optional where Wrapped == EngineStatusDTO {
    /// Missing block means the selected engine is off.
    var engineStatus: EngineStatus { self?.engineStatus ?? .missing }
}
