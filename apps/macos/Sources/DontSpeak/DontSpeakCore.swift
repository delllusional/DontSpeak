// Read-only status bridge over ds-core C probes. Runtime control lives elsewhere;
// model lifecycle state arrives as model_status JSON → EngineStatus for SwiftUI.

import AVFoundation  // AVCaptureDevice (Microphone)
import AppKit
import ApplicationServices  // AXIsProcessTrusted
import CDontSpeak
import DontSpeakLogic
import Foundation
import Observation

/// Owned `char*` → String + free.
private func takeCString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

/// Privacy grant for UI: granted / denied / unknown.
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
    var capsRunning = false
    /// Raw `caps_enabled` (pre-permission). `true` + `capsRunning == false` ⇒ blocked by missing grant.
    var capsWanted = false
    var sttActive = false
    var ttsActive = false
    /// Playback continues but silenced; menu-bar slash.
    var muted = false
    /// Color/animate tokens; [] = never color. Default: static mic + breathing voice.
    var trayIndicator = ["stt", "tts_animated"]
}

struct EngineDots: Sendable, Equatable {
    var kokoro: EngineStatus = .missing
    var parakeet: EngineStatus = .missing
    var system: EngineStatus = .missing
    /// claude_code: `.failed` + "run /voice" hint when selected but not usable.
    var claudeCode: EngineStatus = .missing
    var diarizer: EngineStatus = .missing
    var ttsSystem: EngineStatus = .missing
}

struct EngineSelection: Sendable, Equatable {
    /// Active STT token (claude_code|built_in|system).
    var sttEngine = "built_in"
    /// built_in runtime "ane"|"cpu" (shim fallback); nil for system/claude_code.
    var sttProvider: String? = nil
    /// Active TTS token (built_in|system).
    var ttsEngine = "built_in"
    /// Kokoro runtime token; nil for system TTS.
    var ttsProvider: String? = nil
    /// Bound Claude Code voice key label; nil unless claude_code active + usable.
    var claudeCodeKey: String? = nil
}

/// Dictation overlay state for `DictationPanelController`.
struct Dictation: Sendable, Equatable {
    var recording = false
    var awaiting = false
    var text = ""
    /// Focused app at record start (paste target).
    var target = ""
    /// Local Parakeet path → show panel at start. False for ClaudeNative (no panel).
    var local = false
    /// Editable field focused for paste? False → warning glow. Default true (fail-open).
    var hasTarget = true
    /// Engine `prompt_glow` — shared with Windows so both pulse identically.
    var promptGlow = false
    /// Start refused (missing model / warming): same warning wash as no-target; fail-quiet default.
    var refused = false
    /// Canonical token (ds-status dictation_state): hidden|recording|awaiting_confirm|refused.
    /// Empty (older engine) ⇒ panel falls back to legacy booleans.
    var state = ""
}

/// Off-main-thread health snapshot for the main actor over AsyncStream.
struct HealthSnapshot: Sendable, Equatable {
    var activity = Activity()
    var engineDots = EngineDots()
    var engineSelection = EngineSelection()
    var dictation = Dictation()
    var stats = EngineStats()
    var perms = Perms()
}

/// Read-only health bridge. Main-actor mutations; blocking FFI off-thread → Sendable snapshot.
@Observable @MainActor
final class Core {
    /// Sidebar selection — also set by tray menu before openWindow.
    var screen: AppScreen = .status

    var activity = Activity()
    var engineDots = EngineDots()
    var selection = EngineSelection()
    var dictation = Dictation()
    var stats = EngineStats()
    /// OS grants polled separately — engine can't observe System Settings.
    var perms = Perms()

    /// Session guard for one-shot mic TCC prompt.
    @ObservationIgnored private var micAccessRequested = false

    /// Process-constant version from Rust (avoid per-render FFI).
    @ObservationIgnored let version: String = {
        guard let ptr = ds_version() else { return L.t("common.dash") }
        defer { ds_string_free(ptr) }
        return String(cString: ptr)
    }()

    /// Set once per launch by `checkForUpdateOnce()`; fail-quiet until resolved.
    var updateAvailable = false
    var latestVersion: String?

    /// Set end of init (needs ready self). Label tracks animator's image.
    @ObservationIgnored private(set) var trayAnimator: TrayAnimator!

    @ObservationIgnored private var statusTask: Task<Void, Never>?
    /// ~3 s poll — grants aren't on the status push.
    @ObservationIgnored private var permsTask: Task<Void, Never>?
    @ObservationIgnored private var continuation: AsyncStream<HealthSnapshot>.Continuation?
    /// Blocks in `ds_model_status_wait` (raw Thread — never Task/cooperative pool).
    /// nonisolated(unsafe): main-actor write once; deinit cancel is thread-safe.
    @ObservationIgnored private nonisolated(unsafe) var pushThread: Thread?

    init() {
        // Prime UI from non-blocking probe; first stream value can wait up to ~1 s.
        let snap = Core.probe()
        apply(snap)
        perms = snap.perms

        // Single blocking-wait stream for full status. bufferingNewest(1): no stale backlog.
        let (stream, cont) = AsyncStream<HealthSnapshot>.makeStream(bufferingPolicy: .bufferingNewest(1))
        continuation = cont
        startStatusProducer(cont)
        statusTask = Task { [weak self] in
            for await snap in stream {
                guard let self else { break }
                self.apply(snap)
            }
        }

        // Grants not pushable — poll ~3 s. refresh() also re-reads on return from Settings.
        permsTask = Task { [weak self] in
            while !Task.isCancelled {
                let p = await Task.detached { Core.probePerms() }.value
                guard let self else { return }
                // != guard: @Observable fires on every assign with no equality short-circuit.
                if self.perms != p { self.perms = p }
                try? await Task.sleep(for: .seconds(3))
            }
        }

        trayAnimator = TrayAnimator(core: self)
        // Blocking network GET — off main thread.
        checkForUpdateOnce()
    }

    /// Active TTS model_status object key (`kokoro`|`tts_system`|empty) — `ds_active_tts_slot`.
    nonisolated static func activeTtsSlot(_ ttsEngine: String) -> String {
        ttsEngine.withCString { p in
            guard let ptr = ds_active_tts_slot(p) else { return "" }
            defer { ds_string_free(ptr) }
            return String(cString: ptr)
        }
    }

    /// Active STT model_status object key — `ds_active_stt_slot`.
    nonisolated static func activeSttSlot(_ sttEngine: String) -> String {
        sttEngine.withCString { p in
            guard let ptr = ds_active_stt_slot(p) else { return "" }
            defer { ds_string_free(ptr) }
            return String(cString: ptr)
        }
    }

    /// Tray kind token from shared Rust (`idle`|`recording`|`speaking`).
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

    /// One-shot update check. Failures → no pill (FFI returns `{}`).
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

    /// Producer thread: block in WaitModelStatus → yield. Engine down: yield once then 0.4 s pace.
    private func startStatusProducer(_ cont: AsyncStream<HealthSnapshot>.Continuation) {
        let t = Thread {
            var since: UInt64 = 0  // 0 ⇒ immediate current state
            var delivered = false
            var lastRunning = true
            while !Thread.current.isCancelled {
                let (snap, seq) = Core.probeStatusWait(since)
                let running = snap.activity.engineRunning
                // Idle timeout keeps seq unchanged — statusShouldYield dedups (incl. engineRunning
                // flip, which is external to the gate seq). See StatusYield.swift.
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

    /// Apply snapshot (not perms). Assign only when `!=` so @Observable invalidation stays granular.
    private func apply(_ s: HealthSnapshot) {
        if activity != s.activity { activity = s.activity }
        if engineDots != s.engineDots { engineDots = s.engineDots }
        if selection != s.engineSelection { selection = s.engineSelection }
        if dictation != s.dictation { dictation = s.dictation }
        if stats != s.stats { stats = s.stats }
        maybeRequestMicAccess()
        DictationPanelController.shared.apply(
            state: s.dictation.state,
            recording: s.dictation.recording,
            awaiting: s.dictation.awaiting,
            text: s.dictation.text,
            target: s.dictation.target,
            local: s.dictation.local,
            hasTarget: s.dictation.hasTarget,
            promptGlow: s.dictation.promptGlow,
            refused: s.dictation.refused
        )
    }

    /// Prompt mic TCC once for engines we capture (not off/claude_code). Without this, a fresh
    /// install has no Privacy→Microphone row and the prompt ambushes the first Caps tap.
    /// Dialog only while `.notDetermined`; re-runs if engine switches still undetermined.
    private func maybeRequestMicAccess() {
        guard !micAccessRequested,
            dontSpeakUsesMicrophone(sttEngine: selection.sttEngine)
        else { return }
        guard AVCaptureDevice.authorizationStatus(for: .audio) == .notDetermined else {
            // Latch so we don't re-read TCC on every status push after decided.
            micAccessRequested = true
            return
        }
        micAccessRequested = true
        AVCaptureDevice.requestAccess(for: .audio) { _ in }
    }

    /// Immediate refresh including perms (return from System Settings / after download).
    func refresh() {
        Task { [weak self] in
            let snap = await Task.detached { Core.probe() }.value
            guard let self else { return }
            self.apply(snap)
            self.perms = snap.perms
        }
    }

    /// Switch TTS provider; engine restarts warm child if the active provider changes.
    func setProvider(_ which: String) {
        Task { [weak self] in
            await Task.detached { which.withCString { _ = ds_set_provider($0) } }.value
            self?.refresh()
        }
    }

    /// Mute toggle; refresh reads back engine state (same path as MCP `mute`).
    func setMuted(_ on: Bool) {
        Task { [weak self] in
            await Task.detached { _ = ds_set_muted(on ? 1 : 0) }.value
            self?.refresh()
        }
    }

    /// Full snapshot including perms.
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

    /// Blocking WaitModelStatus (~1 s timeout). Push thread only — never main/cooperative pool.
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

    /// Single typed decode for poll + wait paths. Optional fields → per-field defaults.
    /// Does not set `engineRunning` (caller uses `ds_engine_running_global()`).
    private nonisolated static func decodeStatus(_ json: String?) -> (HealthSnapshot, UInt64?)? {
        guard let json,
            let dto = try? JSONDecoder().decode(ModelStatusDTO.self, from: Data(json.utf8))
        else { return nil }
        var s = HealthSnapshot()
        s.engineDots.kokoro = dto.kokoro.engineStatus
        s.engineDots.parakeet = dto.parakeet.engineStatus
        s.engineDots.system = dto.system.engineStatus
        s.engineDots.claudeCode = dto.claudeCode.engineStatus
        s.engineDots.diarizer = dto.diarization.engineStatus
        s.engineDots.ttsSystem = dto.ttsSystem.engineStatus
        s.engineSelection.sttEngine = dto.sttEngine ?? "built_in"
        s.engineSelection.sttProvider = dto.sttProvider
        s.engineSelection.ttsProvider = dto.ttsProvider
        s.engineSelection.ttsEngine = dto.ttsEngine ?? "built_in"
        s.engineSelection.claudeCodeKey = dto.claudeCodeKey
        if let r = dto.running {
            s.activity.capsRunning = r.caps ?? false
            s.activity.capsWanted = r.capsWanted ?? false
            s.activity.sttActive = r.sttActive ?? false
            s.activity.ttsActive = r.ttsActive ?? false
            s.activity.muted = r.muted ?? false
        }
        s.activity.trayIndicator = dto.trayIndicator ?? Activity().trayIndicator
        s.stats = EngineStats.from(dto.stats)
        if let d = dto.dictation {
            s.dictation.recording = d.recording ?? false
            s.dictation.awaiting = d.awaitingConfirm ?? false
            s.dictation.text = d.text ?? ""
            s.dictation.target = d.target ?? ""
            s.dictation.local = d.localStt ?? false
            s.dictation.hasTarget = d.hasPasteTarget ?? true
            s.dictation.promptGlow = d.promptGlow ?? false
            s.dictation.refused = d.refused ?? false
            s.dictation.state = d.state ?? ""
        }
        return (s, dto.seq)
    }

    /// Non-prompting permission probe.
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

    /// Homepage from shared Rust `ds_homepage_url`.
    func openHomepage() {
        guard let ptr = ds_homepage_url() else { return }
        defer { ds_string_free(ptr) }
        if let url = URL(string: String(cString: ptr)) { NSWorkspace.shared.open(url) }
    }

    func openAccessibilitySettings() { openPrivacyPane("Privacy_Accessibility") }
    func openMicrophoneSettings() { openPrivacyPane("Privacy_Microphone") }

    /// System Settings Privacy pane by anchor key.
    func openPrivacyPane(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    /// System voice settings via shared `ds_open_voice_settings`.
    func openSpokenContentSettings() {
        Task.detached { _ = ds_open_voice_settings() }
    }
}

// MARK: - model_status DTO
// Hand-mirror of `ds-status` wire schema — keep lockstep (ARCHITECTURE.md § FFI).
// All fields Optional → per-field defaults. Unused wire keys omitted (Decodable ignores them).

struct EngineObjDTO: Decodable {
    var state: String?
    /// Byte-weighted overall download fraction 0…1 while downloading.
    var progress: Double?
    var error: String?
}

extension Optional where Wrapped == EngineObjDTO {
    /// Missing block/state → `.missing`.
    var engineStatus: EngineStatus {
        guard let obj = self, let state = obj.state else { return .missing }
        switch state {
        case "downloading": return .downloading(obj.progress ?? 0)
        case "warming": return .warming
        case "blocked": return .blocked
        case "running": return .running
        case "failed": return .failed(obj.error ?? L.t("status.engine.reason.default"))
        case "idle": return .idle
        default: return .missing
        }
    }
}

struct RunningDTO: Decodable {
    var caps: Bool?
    var capsWanted: Bool?
    var sttActive: Bool?
    var ttsActive: Bool?
    var muted: Bool?

    enum CodingKeys: String, CodingKey {
        case caps
        case capsWanted = "caps_wanted"
        case sttActive = "stt_active"
        case ttsActive = "tts_active"
        case muted
    }
}

struct DictationDTO: Decodable {
    var recording: Bool?
    var awaitingConfirm: Bool?
    var text: String?
    var target: String?
    var localStt: Bool?
    var hasPasteTarget: Bool?
    var promptGlow: Bool?
    var refused: Bool?
    /// Nil (older engine) ⇒ legacy boolean derivation, not straight to hidden.
    var state: String?

    enum CodingKeys: String, CodingKey {
        case recording
        case awaitingConfirm = "awaiting_confirm"
        case text
        case target
        case localStt = "local_stt"
        case hasPasteTarget = "has_paste_target"
        case promptGlow = "prompt_glow"
        case refused
        case state
    }
}

struct ModelStatusDTO: Decodable {
    var kokoro: EngineObjDTO?
    var parakeet: EngineObjDTO?
    var diarization: EngineObjDTO?
    var system: EngineObjDTO?
    var claudeCode: EngineObjDTO?
    var ttsSystem: EngineObjDTO?
    var sttEngine: String?
    var sttProvider: String?
    var ttsEngine: String?
    var ttsProvider: String?
    var claudeCodeKey: String?
    var running: RunningDTO?
    var dictation: DictationDTO?
    var trayIndicator: [String]?
    var stats: StatsDTO?
    var seq: UInt64?

    enum CodingKeys: String, CodingKey {
        case kokoro
        case parakeet
        case diarization
        case system
        case claudeCode = "claude_code"
        case ttsSystem = "tts_system"
        case sttEngine = "stt_engine"
        case sttProvider = "stt_provider"
        case ttsEngine = "tts_engine"
        case ttsProvider = "tts_provider"
        case claudeCodeKey = "claude_code_key"
        case running
        case dictation
        case trayIndicator = "tray_indicator"
        case stats
        case seq
    }
}
