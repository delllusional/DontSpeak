//  DontSpeakCore.swift
//
//  A THIN, read-only status bridge over ds-core's global C probes. The app
//  is now an informational menu-bar + health/permissions panel only — ALL control
//  (voice, language, engine, rate, subsystem toggles) lives in DontSpeak.
//  The one action it CAN take is kicking off a model download (tap a missing /
//  failed dot), since that is a setup step, not runtime control.
//
//  The engine computes each engine's lifecycle `state` (missing / idle / warming /
//  running / failed / downloading + progress) in its model-status JSON; this just
//  parses it straight into `EngineStatus` and publishes it for SwiftUI.

import AVFoundation  // AVCaptureDevice (Microphone)
import AppKit
import ApplicationServices  // AXIsProcessTrusted
import CDontSpeak
import DontSpeakLogic
import Foundation
import Observation

/// Copy a returned `char*` into a Swift String and free it (paired alloc/free).
private func takeCString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

/// Grant state of a macOS privacy permission. Three states so the UI can show
/// "Granted" / "Not granted" / "—" (unknown / can't determine yet).
enum Grant: Sendable, Equatable {
    case granted, denied, unknown
}

/// The OS permissions DontSpeak needs, each independently queryable.
struct Perms: Sendable, Equatable {
    var accessibility = Grant.unknown  // type dictation into apps (CGEventPost); also
    // covers reading the Caps key, so no separate
    // Input Monitoring grant is tracked.
    var microphone = Grant.unknown  // record dictation (Parakeet STT)
}

/// Live activity + tray state: what the engine is doing right now and how the
/// menu-bar icon should reflect it.
struct Activity: Sendable, Equatable {
    var engineRunning = false
    var capsRunning = false
    /// The raw `caps_enabled` setting (before the OS-permission preflight). `true` +
    /// `capsRunning == false` ⇒ dictation is enabled but blocked by a missing grant.
    var capsWanted = false
    /// Live push-to-talk capture (true only while audio is being captured).
    var sttActive = false
    /// Live TTS playback (true only while audio is actually playing).
    var ttsActive = false
    /// Global mute (Caps-tap with dictation off, or the tray checkbox): playback still runs,
    /// only the audio is silenced. Marks the menu-bar icon with a diagonal slash.
    var muted = false
    /// Which states the menu-bar icon colors itself for — a SET of tokens (stt|tts);
    /// ["stt","tts"] = both (default), [] = never color.
    var trayIndicator = ["stt", "tts_animated"]
}

/// The per-engine lifecycle dots (missing / idle / warming / running / failed / downloading).
struct EngineDots: Sendable, Equatable {
    var kokoro: EngineStatus = .missing
    var parakeet: EngineStatus = .missing
    /// System STT (macOS 26 on-device SpeechAnalyzer) status — `.missing` when not
    /// available, `.running` when it's the active STT engine.
    var system: EngineStatus = .missing
    /// claude_code STT status — `.running` when Claude Code voice is on + its key is
    /// synthesizable; `.failed` (with a "run /voice" hint) when selected but not usable.
    var claudeCode: EngineStatus = .missing
    /// Speaker-diarization status — `.running` when enabled + models present.
    var diarizer: EngineStatus = .missing
    /// System TTS (macOS `say`) status — `.running` when it's the active TTS engine.
    var ttsSystem: EngineStatus = .missing
}

/// Which STT/TTS engine + provider is active, and the synthesized Claude Code key.
struct EngineSelection: Sendable, Equatable {
    /// The ACTIVE STT engine token (claude_code|built_in|system) — picks which engine
    /// the single STT status row reflects.
    var sttEngine = "built_in"
    /// The ACTUAL STT runtime for the built_in engine — "ane" (Core ML / ANE) or "cpu"
    /// (CPU); honest about the shim-absent fallback, like ttsProvider. Nil for
    /// system/claude_code. Shown in the Parakeet expander.
    var sttProvider: String? = nil
    /// The ACTIVE TTS engine token (built_in|system) — picks which engine the TTS row
    /// reflects (built_in → Kokoro, system → System `say`).
    var ttsEngine = "built_in"
    /// The ACTUAL TTS runtime for Kokoro, as a token — "ane"/"coreml"/"cuda"/"cpu"
    /// (same vocabulary as sttProvider). Nil for the system engine. Shown in the Kokoro expander.
    var ttsProvider: String? = nil
    /// The keypress label DontSpeak synthesizes into Claude Code (its bound voice key),
    /// shown in the claude_code STT expander instead of local stats. Nil unless claude_code
    /// is the active engine and usable.
    var claudeCodeKey: String? = nil
}

/// The dictation confirm-overlay state (drives `DictationPanelController`).
struct Dictation: Sendable, Equatable {
    /// Live capture in progress (partials updating in `text`).
    var recording = false
    /// Transcript finalized, awaiting the Caps confirm tap.
    var awaiting = false
    /// Transcript to show: live partial while recording, final while confirming.
    var text = ""
    /// App focused when recording started — the paste target ("→ Terminal").
    var target = ""
    /// The dictation is the local-transcript (Parakeet) path → show the overlay the
    /// moment recording starts. False for ClaudeNative (no partials → no panel).
    var local = false
    /// LIVE: is an editable text field focused to receive the paste right now? The
    /// engine samples this each tick while the panel is up; the overlay tints the glow
    /// when false ("no input to submit into"). True by default (fail-open).
    var hasTarget = true
    /// The engine's "speak now" glow decision (recording, nothing transcribed yet, not
    /// awaiting confirm) — computed once in the core (`prompt_glow`) so this overlay and
    /// the Windows one pulse identically. The no-target warning glow stays driven by
    /// `hasTarget`.
    var promptGlow = false
}

/// An immutable, Sendable health snapshot produced off the main thread and carried to
/// the main actor over an `AsyncStream`. `Equatable` is kept for tests / callers.
struct HealthSnapshot: Sendable, Equatable {
    var activity = Activity()
    var engineDots = EngineDots()
    var engineSelection = EngineSelection()
    var dictation = Dictation()
    var stats = EngineStats()
    var perms = Perms()
}

/// Read-only health bridge. `@Observable` so SwiftUI tracks the stored property groups
/// automatically — only views that READ a changed group re-render, so there's no manual
/// dedup. `@MainActor` so the mirrors are always mutated on the main thread; the blocking
/// FFI probes run OFF the main actor (a dedicated thread / detached task) and hand back a
/// `Sendable` snapshot.
@Observable @MainActor
final class Core {
    /// Which screen the single window's sidebar shows. Stored here (not local to the window)
    /// so the menu-bar items can open the window AND jump straight to a screen; the sidebar
    /// binds to it, so navigating there writes back here.
    var screen: AppScreen = .status

    /// Live activity + tray state (engine/caps liveness, record/speak, mute, tray prefs).
    var activity = Activity()
    /// Per-engine lifecycle dots (Kokoro/Parakeet/system/claude_code/diarizer/ttsSystem).
    var engineDots = EngineDots()
    /// Active STT/TTS engine + provider selection (and the synthesized claude_code key).
    var selection = EngineSelection()
    /// Dictation confirm-overlay state (drives DictationPanelController).
    var dictation = Dictation()
    var stats = EngineStats()
    /// Live OS-permission grant states. The engine can't observe System-Settings grants, so
    /// these stay POLLED (see `permsTask`) rather than pushed over the status stream.
    var perms = Perms()

    /// The app version string ("0.1.0"), resolved ONCE from the shared Rust source. It's constant
    /// for the process lifetime, so it lives here instead of an `ds_version()` FFI round-trip on
    /// every Status render. `@ObservationIgnored`: never changes, so it needn't be tracked.
    @ObservationIgnored let version: String = {
        guard let ptr = ds_version() else { return L.t("common.dash") }
        defer { ds_string_free(ptr) }
        return String(cString: ptr)
    }()

    /// Animates the menu-bar icon (crossfade on state change + breathing while active) off
    /// this Core's activity. `@ObservationIgnored`: the reference never changes; the label
    /// tracks the animator's own `image`. Set at the end of `init` (needs a ready `self`).
    @ObservationIgnored private(set) var trayAnimator: TrayAnimator!

    /// Consumes the status `AsyncStream` on the main actor (applies each snapshot + drives
    /// the dictation overlay). `@ObservationIgnored`: lifecycle handle, not view state.
    @ObservationIgnored private var statusTask: Task<Void, Never>?
    /// Polls the OS permissions every ~3 s — the one thing the push can't carry.
    @ObservationIgnored private var permsTask: Task<Void, Never>?
    /// The status continuation, finished on teardown so the consumer loop ends.
    @ObservationIgnored private var continuation: AsyncStream<HealthSnapshot>.Continuation?
    /// The dedicated PRODUCER thread: blocks in `ds_model_status_wait` and yields each
    /// FULL status snapshot the instant the engine bumps its gate (mirrors the Windows
    /// `status-push` thread). A raw `Thread`, not a `Task` — the FFI call blocks, and blocking
    /// the cooperative pool would starve it.
    /// `@ObservationIgnored` + `nonisolated(unsafe)`: written once on the main actor, read only
    /// in `deinit`, and `Thread.cancel()` is itself thread-safe — so the nonisolated deinit can
    /// touch this non-Sendable handle without a data race.
    @ObservationIgnored private nonisolated(unsafe) var pushThread: Thread?

    init() {
        // Paint immediately: the first stream value can block up to the engine's 1 s wait
        // timeout, so prime the UI synchronously from a non-blocking probe (model_status_json,
        // not the wait variant) + a one-shot permission read.
        let snap = Core.probe()
        apply(snap)
        perms = snap.perms

        // ONE status stream replaces the old poll ticker + dictation push. The engine now bumps
        // its gate on EVERY status change, so a single blocking-wait loop carries the FULL status
        // (activity/dots/selection/dictation/stats). `bufferingNewest(1)`: only the latest matters,
        // so a slow consumer can never build a backlog of stale snapshots.
        let (stream, cont) = AsyncStream<HealthSnapshot>.makeStream(bufferingPolicy: .bufferingNewest(1))
        continuation = cont
        startStatusProducer(cont)
        statusTask = Task { [weak self] in
            for await snap in stream {
                guard let self else { break }
                self.apply(snap)
            }
        }

        // OS permissions can't be pushed (the engine can't observe System-Settings grants), so
        // poll them on a cheap, separate cadence — a grant flips the row within ~3 s.
        permsTask = Task { [weak self] in
            while !Task.isCancelled {
                let p = await Task.detached { Core.probePerms() }.value
                guard let self else { return }
                // Assign only on a real change so an unchanged probe (the steady state, since
                // grants are rare) doesn't fire a redundant @Observable notification. The poll
                // is just a backstop for grants made while the Status window is open — `refresh()`
                // already re-reads perms on the common return-from-System-Settings path — so a
                // slower cadence is plenty and trims the wake-ups / detached-task spawns.
                if self.perms != p { self.perms = p }
                try? await Task.sleep(for: .seconds(3))
            }
        }

        // Now that `self` is ready, spin the menu-bar icon animator off this Core's activity.
        trayAnimator = TrayAnimator(core: self)
    }

    deinit {
        statusTask?.cancel()
        permsTask?.cancel()
        continuation?.finish()
        pushThread?.cancel()
    }

    /// Spin the dedicated PRODUCER thread: block in the engine's `WaitModelStatus`, then yield
    /// the full snapshot into the stream. The engine bumps its status sequence on every change,
    /// so this returns within a tick of any new state — a ~0-jitter push. When the engine is
    /// down the wait can't block, so yield once (so the UI reflects engine-down) then pace
    /// ourselves to avoid a hot spin until it comes back.
    private func startStatusProducer(_ cont: AsyncStream<HealthSnapshot>.Continuation) {
        let t = Thread {
            var since: UInt64 = 0  // 0 ⇒ the first call returns the current state immediately
            var delivered = false
            var lastRunning = true
            while !Thread.current.isCancelled {
                let (snap, seq) = Core.probeStatusWait(since)
                let running = snap.activity.engineRunning
                // The blocking wait returns on a ~1 s TIMEOUT with the SAME seq when nothing
                // changed; yielding that identical snapshot would re-run `apply` and churn every
                // @Observable reader (menu-bar label, any open window, the TrayAnimator chain)
                // ~1×/s forever while idle. So yield only when something actually changed:
                //   • the gate sequence advanced (a real engine-side status change), OR
                //   • `engineRunning` flipped — this is an EXTERNAL pidfile/launchd probe NOT
                //     carried in the gate seq, so a stop/crash freezes the seq and must be
                //     caught here (else the menu-bar dot stays stale "running"), OR
                //   • the very first sample.
                // The 0.4 s pace below covers the can't-block down state.
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

    /// Apply a FULL status snapshot to the observed groups (NOT perms — those are polled
    /// separately) and drive the dictation overlay. Each group is assigned only when it actually
    /// CHANGED: `@Observable`'s generated setters fire `withMutation` on every assignment with no
    /// equality short-circuit, so an unconditional reassign of all five groups would invalidate
    /// every group's observers on every push (e.g. a per-utterance `stats` update would also
    /// re-render the menu bar / tray animator, which only read `activity`). Gating on `!=` (all
    /// groups are `Equatable`) keeps invalidation granular — the same guard `permsTask` uses.
    private func apply(_ s: HealthSnapshot) {
        if activity != s.activity { activity = s.activity }
        if engineDots != s.engineDots { engineDots = s.engineDots }
        if selection != s.engineSelection { selection = s.engineSelection }
        if dictation != s.dictation { dictation = s.dictation }
        if stats != s.stats { stats = s.stats }
        DictationPanelController.shared.apply(
            recording: s.dictation.recording,
            awaiting: s.dictation.awaiting,
            text: s.dictation.text,
            target: s.dictation.target,
            local: s.dictation.local,
            hasTarget: s.dictation.hasTarget,
            promptGlow: s.dictation.promptGlow
        )
    }

    /// Force an immediate refresh (e.g. after returning from System Settings, or right after
    /// kicking off a download). Unlike the status stream this also re-reads perms, so a grant
    /// made while away reflects without waiting for the next `permsTask` tick.
    func refresh() {
        Task { [weak self] in
            let snap = await Task.detached { Core.probe() }.value
            guard let self else { return }
            self.apply(snap)
            self.perms = snap.perms
        }
    }

    /// Switch the TTS execution provider ("auto"|"cpu"|"cuda"|"coreml"|"ane"). The engine
    /// restarts the warm child on the new provider and resets its stats (only if
    /// the active provider actually changed); the next push reflects it.
    func setProvider(_ which: String) {
        Task { [weak self] in
            await Task.detached { which.withCString { _ = ds_set_provider($0) } }.value
            self?.refresh()
        }
    }

    /// Toggle global mute (the tray "Mute" checkbox). Mutes/unmutes the warm child's playback
    /// without stopping it, then `refresh()` reads the flag back so the menu-bar icon reflects
    /// the engine's state — the SAME status-driven path the `mute` MCP tool takes. The read-back
    /// is instant in practice, so no optimistic local echo is needed.
    func setMuted(_ on: Bool) {
        Task { [weak self] in
            await Task.detached { _ = ds_set_muted(on ? 1 : 0) }.value
            self?.refresh()
        }
    }

    /// Full snapshot INCLUDING the OS-permission grants. Used by `refresh()`; the poll
    /// loop calls `probeStatus()` + `probePerms()` separately for the same result.
    nonisolated static func probe() -> HealthSnapshot {
        var s = probeStatus()
        s.perms = probePerms()
        return s
    }

    /// Engine liveness + the model-status JSON, WITHOUT the permission probes. Pure
    /// w.r.t. the actor — touches no `self`, so it is safe to run detached.
    nonisolated static func probeStatus() -> HealthSnapshot {
        let running = ds_engine_running_global() != 0
        var s = decodeStatus(takeCString(ds_model_status_json()))?.0 ?? HealthSnapshot()
        s.activity.engineRunning = running
        return s
    }

    /// BLOCKING status read for the overlay PUSH (mirrors the Windows `ModelStatusWait`):
    /// wait until the engine's status sequence differs from `since` — i.e. a dictation
    /// change landed — or ~1 s elapses, then parse. Returns the snapshot plus the new
    /// sequence to pass back as `since`. Runs on the dedicated push thread because the
    /// FFI call blocks; never call it from the main actor or the cooperative pool.
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

    /// Decode the model-status JSON into a snapshot (+ its top-level `seq`), the SINGLE
    /// typed path shared by the polling `probeStatus` and the blocking `probeStatusWait`
    /// so the two can't drift. Returns nil for down/invalid JSON — the caller keeps the
    /// default snapshot (`engineRunning` is still set from the global probe). Every DTO
    /// field is Optional, so a malformed or missing field falls back to the SAME default
    /// the old `[String: Any]` path used instead of blanking the whole status. Does NOT
    /// set `engineRunning`; the caller owns that from `ds_engine_running_global()`.
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
        s.activity.trayIndicator = dto.trayIndicator ?? ["stt", "tts_animated"]
        s.stats = EngineStats.from(dto.stats)
        if let d = dto.dictation {
            s.dictation.recording = d.recording ?? false
            s.dictation.awaiting = d.awaitingConfirm ?? false
            s.dictation.text = d.text ?? ""
            s.dictation.target = d.target ?? ""
            s.dictation.local = d.localStt ?? false
            s.dictation.hasTarget = d.hasPasteTarget ?? true
            s.dictation.promptGlow = d.promptGlow ?? false
        }
        return (s, dto.seq)
    }

    /// Query the OS permissions without prompting. Cheap calls — safe to run
    /// each poll, so a grant in System Settings flips the row within a few seconds.
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

    /// Open the product homepage (dontspeak.org) in the default browser. The URL is the
    /// shared single source of truth from Rust (`ds_homepage_url`), so every
    /// platform links to the same place.
    func openHomepage() {
        guard let ptr = ds_homepage_url() else { return }
        defer { ds_string_free(ptr) }
        if let url = URL(string: String(cString: ptr)) { NSWorkspace.shared.open(url) }
    }

    /// Open System Settings → Privacy & Security → Accessibility so the user can
    /// grant DontSpeak.app — it hosts the in-process engine that posts Caps Lock
    /// keystrokes. The engine re-probes AX, so the dot flips green shortly after.
    func openAccessibilitySettings() { openPrivacyPane("Privacy_Accessibility") }
    func openMicrophoneSettings() { openPrivacyPane("Privacy_Microphone") }

    /// Open System Settings → Privacy & Security → <pane>. `anchor` is the pane key,
    /// e.g. "Privacy_Accessibility" / "Privacy_Microphone".
    /// The engine re-probes, so the row flips shortly after.
    func openPrivacyPane(_ anchor: String) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Open System Settings → Accessibility → Spoken Content, where macOS manages the System
    /// TTS (`say`) voices ("Manage Voices…"). Routes to the SHARED cross-platform seam
    /// (`ds_open_voice_settings`) so macOS, Windows, and Linux all open their system-voice
    /// page from ONE Rust implementation (per-OS deep links live in ds-tts).
    func openSpokenContentSettings() {
        Task.detached { _ = ds_open_voice_settings() }
    }
}

// MARK: - model_status DTO (type-safe decode)
//
// The Swift HAND-MIRROR of the `model_status` wire schema. The single source of truth is the
// Rust crate `ds-status` (rust/crates/ds-status/src/lib.rs): the engine serializes
// a `ds_status::ModelStatus` to JSON over the C ABI and this decodes it back. These DTOs
// mirror that schema field-for-field and MUST be kept in lockstep with it — see STATUS-SCHEMA.md.
//
// EVERY field is Optional so a malformed/missing value degrades to its per-field default (see
// `decodeStatus` / `EngineStats.from`) instead of failing the whole decode and blanking the
// status. snake_case wire keys are mapped explicitly via `CodingKeys` (more robust than
// `.convertFromSnakeCase` for keys like `tts`, `stt_active`, `first_avg_ms`). Keys the app does
// NOT consume are omitted on purpose (`caps_events`, `build_id`, and the `running` engine
// booleans kokoro/parakeet/system/… — the engine dots already carry those); `Decodable` ignores
// unknown keys, so omitting them is safe.

/// One engine's lifecycle block: `{present, removable, state, progress, error}`.
struct EngineObjDTO: Decodable {
    var present: Bool?
    var removable: Bool?
    var state: String?
    /// Overall download fraction 0…1 — byte-weighted across the whole model set (a single global
    /// percent, not per-file). Only meaningful while `state == "downloading"`.
    var progress: Double?
    var error: String?
}

extension Optional where Wrapped == EngineObjDTO {
    /// Map this engine block to an `EngineStatus`, replicating the old `engineStatus(_:)`
    /// logic exactly: a missing block (engine down) or missing `state` → `.missing`.
    var engineStatus: EngineStatus {
        guard let obj = self, let state = obj.state else { return .missing }
        switch state {
        case "downloading": return .downloading(obj.progress ?? 0)
        case "warming": return .warming
        case "running": return .running
        case "failed": return .failed(obj.error ?? L.t("status.engine.reason.default"))
        case "idle": return .idle
        default: return .missing
        }
    }
}

/// The live `running` flags block.
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

/// The dictation-overlay block.
struct DictationDTO: Decodable {
    var recording: Bool?
    var awaitingConfirm: Bool?
    var text: String?
    var target: String?
    var localStt: Bool?
    var hasPasteTarget: Bool?
    var promptGlow: Bool?

    enum CodingKeys: String, CodingKey {
        case recording
        case awaitingConfirm = "awaiting_confirm"
        case text
        case target
        case localStt = "local_stt"
        case hasPasteTarget = "has_paste_target"
        case promptGlow = "prompt_glow"
    }
}

/// The top-level model_status payload.
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
    var buildId: String?
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
        case buildId = "build_id"
        case seq
    }
}
