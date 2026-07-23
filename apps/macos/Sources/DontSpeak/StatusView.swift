// Status pane: health + permissions only. Each expandable row owns its `@State expanded`
// so toggles/pushes invalidate just that subtree; collapsed rows skip FFI formatters.

import AppKit
import CDontSpeak
import DontSpeakLogic
import SwiftUI

/// Engine lifecycle for the status dot (color + shape for color-blind readability).
enum EngineStatus: Equatable, Sendable {
    case missing
    case idle
    /// Overall byte-weighted download fraction 0…1.
    case downloading(Double)
    case warming
    case running
    case failed(String)
    /// Enabled but a required OS grant is missing.
    case blocked

    /// Shared `ds_engine_state_word`; shown via `troubleNote` when not ready.
    var word: String {
        func w(_ state: String, _ progress: Double = 0, _ why: String = "") -> String {
            state.withCString { sp in
                why.withCString { wp in
                    guard let ptr = ds_engine_state_word(sp, progress, wp) else { return state }
                    defer { ds_string_free(ptr) }
                    return String(cString: ptr)
                }
            }
        }
        switch self {
        case .missing: return w("missing")
        case .idle: return w("idle")
        case .downloading(let p): return w("downloading", p, "")
        case .warming: return w("warming")
        case .running: return w("running")
        case .failed(let why): return w("failed", 0, why)
        case .blocked: return w("blocked")
        }
    }

    /// Expanded note when not ready; nil when running/idle (show stats).
    /// Empty `word` is the note-vs-stats gate — same as Windows/Linux.
    var troubleNote: String? {
        let w = word
        return w.isEmpty ? nil : w
    }
}

/// Right-aligned color+shape status indicator.
struct StatusDot: View {
    let status: EngineStatus
    init(_ status: EngineStatus) { self.status = status }

    private let size: CGFloat = 10

    // Not-ready detail is the expanded troubleNote line (no tooltip).
    var body: some View {
        Group {
            switch status {
            case .missing:
                Circle().strokeBorder(Color.secondary.opacity(0.5), lineWidth: 1.5)
            case .idle:
                Circle().fill(Color.secondary.opacity(0.45))
            case .downloading(let p):
                ZStack {
                    Circle().strokeBorder(Color.smWarning.opacity(0.25), lineWidth: 2)
                    Circle()
                        .trim(from: 0, to: max(0.02, min(1, p)))
                        .stroke(Color.smWarning, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                }
            case .warming, .blocked:
                Circle().fill(Color.smWarning)
            case .running:
                Circle().fill(Color.green)
            case .failed:
                ZStack {
                    Circle().fill(Color.red)
                    Text("!").font(.system(size: 7, weight: .bold)).foregroundStyle(.white)
                }
            }
        }
        .frame(width: size, height: size)
    }
}

/// Expandable-row trailing: status dot ↔ chevron crossfade (rides caller's animation).
struct ExpandDot<Dot: View>: View {
    let expanded: Bool
    let dot: Dot
    init(expanded: Bool, @ViewBuilder dot: () -> Dot) {
        self.expanded = expanded
        self.dot = dot()
    }

    var body: some View {
        ZStack {
            dot
                .opacity(expanded ? 0 : 1)
            Image(systemName: "chevron.up")
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(.secondary)
                .opacity(expanded ? 1 : 0)
        }
    }
}

struct StatusView: View {
    @Environment(Core.self) private var core

    /// `ds_diarization_ui_enabled` (not a host-local const).
    private let diarizationUIEnabled: Bool = ds_diarization_ui_enabled() != 0

    var body: some View {
        ScrollView {
            VStack(spacing: 12) {
                // Headline row → lifetime totals. Integrations wired by engine reconcile, not here.
                Platter {
                    DontSpeakRow()
                }

                // Role (TTS/STT) + secondary backend; lifecycle dot = downloaded+running.
                Platter {
                    ttsEngineRow
                    PlatterDivider()
                    sttEngineRow
                    // On-demand diarization; visibility: shared validation gate (#77).
                    if diarizationUIEnabled {
                        PlatterDivider()
                        EngineStatRow(
                            role: L.t("status.engine.role_diar"),
                            detail: L.t("status.engine.sortformer"),
                            status: core.diarization.status
                        ) { DiarStatsContent() }
                    }
                }

                // Caps loop + nested AX/Mic grants (folded into header dot).
                Platter {
                    CapsLockRow()
                }
            }
            .windowContentInset()
        }
        .scrollIndicators(.hidden)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var ttsEngineRow: some View {
        switch core.tts.engine {
        case "system":
            EngineStatRow(
                role: L.t("status.engine.role_tts"), detail: L.t("status.engine.system"),
                status: core.tts.status
            ) { TtsStatsContent() }
        case "built_in":
            if let model = core.tts.model {
                EngineStatRow(
                    role: L.t("status.engine.role_tts"), detail: ttsModelName(model),
                    status: core.tts.status
                ) { TtsStatsContent() }
            } else {
                OffEngineRow(role: L.t("status.engine.role_tts"))
            }
        default:
            OffEngineRow(role: L.t("status.engine.role_tts"))
        }
    }

    private func ttsModelName(_ model: TtsModel) -> String {
        switch model {
        case .kokoro: L.t("status.engine.kokoro")
        case .chatterbox: L.t("status.engine.chatterbox")
        case .qwen: L.t("status.engine.qwen")
        case .omnivoice: L.t("status.engine.omnivoice")
        }
    }

    @ViewBuilder
    private var sttEngineRow: some View {
        switch core.stt.engine {
        case "claude_code":
            EngineStatRow(
                role: L.t("status.engine.role_stt"), detail: L.t("status.engine.claude_code"),
                status: core.stt.status
            ) { SttStatsContent() }
        case "system":
            EngineStatRow(
                role: L.t("status.engine.role_stt"), detail: L.t("status.engine.system"),
                status: core.stt.status
            ) { SttStatsContent() }
        case "built_in":
            EngineStatRow(
                role: L.t("status.engine.role_stt"), detail: L.t("status.engine.parakeet"),
                status: core.stt.status
            ) { SttStatsContent() }
        default:
            OffEngineRow(role: L.t("status.engine.role_stt"))
        }
    }
}

// MARK: - Rows (each owns its own `expanded` state)

/// Expandable engine row. Auto-download on first use — no Download/Retry button.
/// Stats built only when open AND ready.
private struct EngineStatRow<Stats: View>: View {
    let role: String
    let detail: String
    let status: EngineStatus
    @ViewBuilder var stats: () -> Stats
    @State private var expanded = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                HStack(spacing: 6) {
                    Text(role).glassRowTitle()
                    Text(detail).glassRowDetail()
                }
                Spacer()
                ExpandDot(expanded: expanded) { StatusDot(status) }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { expandToggle($expanded) }
            if expanded {
                PlatterDivider()
                statusDetailBlock {
                    if let note = status.troubleNote {
                        Text(note).glassCaption()
                    } else {
                        stats()
                    }
                }
            }
        }
    }
}

/// Off engine: role + gray idle dot, not expandable.
private struct OffEngineRow: View {
    let role: String
    var body: some View {
        HStack(spacing: 8) {
            Text(role).glassRowTitle()
            Spacer()
            StatusDot(.idle)
        }
        .frame(maxWidth: .infinity)
        .platterRow()
    }
}

/// App name + version (links home); expands to lifetime totals.
private struct DontSpeakRow: View {
    @Environment(Core.self) private var core
    @State private var expanded = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                HStack(spacing: 6) {
                    Text(L.t("common.app_name")).glassRowTitle()
                    // Version has its own tap (homepage); rest of row expands. UpdateBadge = same target.
                    if core.updateAvailable, let latest = core.latestVersion {
                        UpdateBadge(current: core.version, latest: latest)
                            .contentShape(Rectangle())
                            .linkCursor()
                            .onTapGesture { core.openHomepage() }
                    } else {
                        Text(core.version).glassRowDetail()
                            .contentShape(Rectangle())
                            .linkCursor()
                            .onTapGesture { core.openHomepage() }
                    }
                }
                Spacer()
                ExpandDot(expanded: expanded) { StatusDot(core.activity.engineRunning ? .running : .idle) }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { expandToggle($expanded) }
            if expanded {
                PlatterDivider()
                statusDetailBlock { LifetimeContent() }
            }
        }
    }
}

/// "current → new" pill; brand purple (neutral notice, not warning orange).
private struct UpdateBadge: View {
    let current: String
    let latest: String

    var body: some View {
        HStack(spacing: 4) {
            Text(current)
            Text(L.t("common.update_arrow"))
            Text(latest)
        }
        .glassRowDetail()
        .padding(.horizontal, 7)
        .padding(.vertical, 2)
        .background(Capsule().fill(Color.smSeedPurple.opacity(0.18)))
        .accessibilityLabel(L.t("status.update_available"))
    }
}

/// Caps capture loop (subsystem, not a permission). Nested grants fold into header via capsCombined.
private struct CapsLockRow: View {
    @Environment(Core.self) private var core
    @State private var expanded = false

    /// Mic row only for engines we capture — see `dontSpeakUsesMicrophone`.
    private var showsMicrophone: Bool {
        dontSpeakUsesMicrophone(sttEngine: core.stt.engine)
    }

    /// Nested grants for header: orange only on DENIED (not .unknown). Mic only if showsMicrophone.
    /// Input Monitoring omitted — Accessibility subsumes it.
    private var permsRollup: Grant {
        var grants = [core.perms.accessibility]
        if showsMicrophone { grants.append(core.perms.microphone) }
        return grants.contains(.denied) ? .denied : .granted
    }

    /// Caps loop state folded with nested grants (denied → orange on collapsed header).
    private var capsCombined: EngineStatus {
        if permsRollup == .denied { return .blocked }
        if core.activity.capsActive { return .running }
        return core.activity.capsEnabled ? .blocked : .idle
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text(L.t("status.caps_lock")).glassRowTitle()
                Spacer()
                ExpandDot(expanded: expanded) { StatusDot(capsCombined) }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { expandToggle($expanded) }
            if expanded {
                PlatterDivider()
                statusDetailBlock {
                    glassHint("status.caps_hint")
                }
                PlatterDivider()
                PermRow(
                    name: L.t("status.permission.accessibility"), grant: core.perms.accessibility,
                    purpose: L.t("status.permission.accessibility_purpose"), pane: "Privacy_Accessibility")
                if showsMicrophone {
                    PlatterDivider()
                    PermRow(
                        name: L.t("status.permission.microphone"), grant: core.perms.microphone,
                        purpose: L.t("status.permission.microphone_purpose"), pane: "Privacy_Microphone")
                }
            }
        }
    }
}

/// Permission row: purpose, Settings button, grant dot.
private struct PermRow: View {
    @Environment(Core.self) private var core
    let name: String
    let grant: Grant
    let purpose: String
    let pane: String

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(name).glassRowTitle()
                Text(purpose).glassCaption()
            }
            Spacer()
            Button {
                core.openPrivacyPane(pane)
            } label: {
                Image(systemName: "arrow.up.forward.app")
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help(L.t("status.permission.open_settings_help"))
            .linkCursor()
            grantDot(grant)
        }
        .frame(maxWidth: .infinity)
        .platterRow()
    }
}

// MARK: - Expanded stat content

/// Lifetime TTS/STT seconds (all sessions), live off status push.
private struct LifetimeContent: View {
    @Environment(Core.self) private var core
    var body: some View {
        LabeledContent {
            Text(durationText(core.stats.lifetime.ttsSecs)).monospacedDigit()
        } label: {
            lifetimeLabel(L.t("status.engine.role_tts"))
        }
        LabeledContent {
            Text(durationText(core.stats.lifetime.sttSecs)).monospacedDigit()
        } label: {
            lifetimeLabel(L.t("status.engine.role_stt"))
        }
    }
}

/// Active TTS stats: System `say` → Spoken Content link; Kokoro → live RTF stats.
private struct TtsStatsContent: View {
    @Environment(Core.self) private var core
    var body: some View {
        if core.tts.engine == "system" {
            // Whole row opens Spoken Content (no local RTF for `say`).
            LabeledContent {
                Image(systemName: "arrow.up.forward.app").foregroundStyle(.secondary)
            } label: {
                Text(L.t("status.tts_system_settings"))
            }
            .contentShape(Rectangle())
            .onTapGesture { core.openSpokenContentSettings() }
            .linkCursor()
        } else {
            if let prov = core.tts.provider {
                LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(prov))
            }
            let s = core.stats.tts
            if s.utterances == 0 {
                glassHint("status.no_data")
            } else {
                statRangeRow(
                    L.t("status.stats.realtime"), s.rtfMin, s.rtfAvg, s.rtfMax, 2, "status.stats.unit.times")
                statRangeRow(
                    L.t("status.stats.first_audio"), s.firstMinMs / 1000, s.firstAvgMs / 1000,
                    s.firstMaxMs / 1000, 1,
                    "status.stats.unit.seconds")
                statCountRow(L.t("status.stats.spoken"), s.utterances, s.audioSecs)
                LabeledContent(L.t("status.stats.queue"), value: "\(s.queued)")
                if s.failures > 0 {
                    LabeledContent(L.t("status.stats.failures"), value: "\(s.failures)").foregroundStyle(.red)
                }
            }
        }
    }
}

/// Active STT stats; empty-state hint is engine-specific.
private struct SttStatsContent: View {
    @Environment(Core.self) private var core
    var body: some View {
        if core.stt.engine == "built_in", let prov = core.stt.provider {
            LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(prov))
        }
        // Claude Code: show synthesized key, not local RTF.
        let s = core.stats.stt
        if core.stt.engine == "claude_code" {
            if let k = core.stt.delegationKey, !k.isEmpty {
                glassHint("status.stt_claude_code", ["key": k])
            } else {
                glassHint("status.stt_claude_code_off")
            }
        } else if s.transcriptions == 0 {
            glassHint("status.no_data")
        } else {
            statRangeRow(
                L.t("status.stats.realtime"), s.rtfMin, s.rtfAvg, s.rtfMax, 2, "status.stats.unit.times")
            statCountRow(L.t("status.stats.transcribed"), s.transcriptions, s.audioSecs)
            if s.failures > 0 {
                LabeledContent(L.t("status.stats.failures"), value: "\(s.failures)").foregroundStyle(.red)
            }
        }
    }
}

/// Diarization: enroll prompt until speakers exist; then runtime + names + threshold.
private struct DiarStatsContent: View {
    @Environment(Core.self) private var core
    var body: some View {
        let s = core.diarization
        if !s.enabled {
            glassHint("status.diarization_disabled")
        } else if s.speakers.isEmpty {
            glassHint("status.diarization_no_speakers")
        } else {
            if let prov = s.provider {
                LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(prov))
            }
            LabeledContent(
                L.t("status.diarization_enrolled"),
                value: s.speakers.joined(separator: ", "))
            LabeledContent(
                L.t("status.diarization_sensitivity"),
                value: String(format: "%.2f", s.activityThreshold))
        }
    }
}

// MARK: - Shared row chrome + formatters

/// Expanded-row detail with platter insets + spread LabeledContent.
@MainActor @ViewBuilder
private func statusDetailBlock<C: View>(@ViewBuilder _ content: () -> C) -> some View {
    VStack(alignment: .leading, spacing: 8) { content() }
        .labeledContentStyle(.spread)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14)
        .padding(.vertical, 9)
}

@MainActor
private func glassHint(_ key: String) -> some View {
    Text(L.t(key)).glassCaption()
}

@MainActor
private func glassHint(_ key: String, _ params: [String: String]) -> some View {
    Text(L.t(key, params)).glassCaption()
}

/// Grant as a StatusDot-column dot.
@MainActor @ViewBuilder
private func grantDot(_ grant: Grant) -> some View {
    Group {
        switch grant {
        case .granted: Circle().fill(Color.green)
        case .denied: Circle().fill(Color.orange)
        case .unknown: Circle().strokeBorder(Color.secondary.opacity(0.5), lineWidth: 1.5)
        }
    }
    .frame(width: 10, height: 10)
    .help(
        grant == .granted
            ? L.t("status.grant.granted")
            : (grant == .denied ? L.t("status.grant.denied") : L.t("status.grant.unknown")))
}

@MainActor
private func lifetimeLabel(_ name: String) -> some View {
    HStack(spacing: 6) {
        Text(name).glassRowTitle()
        Text(L.t("status.stats.lifetime_all_time")).glassRowDetail()
    }
}

/// Live duration via shared `ds_duration_live`.
private func durationText(_ secs: Double) -> String {
    smTake(ds_duration_live(secs))
}

/// Via shared `ds_stats_count`.
@MainActor @ViewBuilder
private func statCountRow(_ label: String, _ count: Int, _ secs: Double) -> some View {
    LabeledContent {
        Text(smTake(ds_stats_count(UInt64(count), secs))).monospacedDigit()
    } label: {
        Text(label).glassRowTitle()
    }
}

/// Via shared `ds_stats_range`.
@MainActor @ViewBuilder
private func statRangeRow(
    _ title: String, _ lo: Double, _ avg: Double, _ hi: Double,
    _ precision: UInt32, _ unitKey: String
) -> some View {
    LabeledContent {
        Text(smTake(ds_stats_range(lo, avg, hi, precision, unitKey))).monospacedDigit()
    } label: {
        Text(title).glassRowTitle()
    }
}

/// Runtime token → label via shared `ds_runtime_label`.
private func runtimeLabel(_ provider: String) -> String {
    smTake(ds_runtime_label(provider))
}

/// Owned `char*` → String + free.
private func smTake(_ ptr: UnsafeMutablePointer<CChar>?) -> String {
    guard let ptr else { return "" }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

// MARK: - Cursor modifiers

/// Link cursor on hover. Pre-15: continuous NSCursor (Form/List resets on mouse-moved).
private struct LinkCursorOnHover: ViewModifier {
    func body(content: Content) -> some View {
        if #available(macOS 15.0, *) {
            content.pointerStyle(.link)
        } else {
            content.onContinuousHover { phase in
                switch phase {
                case .active: NSCursor.pointingHand.set()
                case .ended: NSCursor.arrow.set()
                }
            }
        }
    }
}

private extension View {
    func linkCursor() -> some View { modifier(LinkCursorOnHover()) }
}
