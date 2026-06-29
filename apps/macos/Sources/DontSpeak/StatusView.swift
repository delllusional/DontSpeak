//  StatusView.swift
//
//  The single window: a read-only health + permissions panel. All controls
//  (engine/voice/language/rate/toggles/downloads) live in DontSpeak; this
//  screen only shows state and helps grant the OS permissions MCP can't.

import SwiftUI
import AppKit
import CDontSpeak

/// Lifecycle state of an engine/model, shown as one right-aligned dot. Color AND
/// shape both carry the meaning (readable when color-blind):
///   • missing      — not downloaded yet                → gray, hollow ring
///   • idle         — downloaded but not running (off)   → gray, filled
///   • downloading  — fetching the model (0…1 progress)  → orange, progress ring
///   • warming      — downloaded, starting/loading       → orange, filled
///   • blocked      — enabled but a required grant missing → orange, filled
///   • running      — warm / active / ready              → green, filled
///   • failed       — present but won't start (retry)    → red, filled + "!"
enum EngineStatus: Equatable, Sendable {
    case missing
    case idle
    /// (progress 0…1, current file index, total file count) — index/count 0 when single-file.
    case downloading(Double, Int, Int)
    case warming
    case running
    case failed(String)
    /// Enabled, but a required OS permission is missing → orange "warning".
    case blocked

    /// Short status word — resolved by the shared Rust formatter (`ds_engine_state_word`)
    /// so the state→word mapping lives in ONE place for every UI. Shown via `troubleNote` in
    /// the row's expansion when the engine isn't ready (there is no hover tooltip anymore).
    var word: String {
        func w(_ state: String, _ progress: Double = 0, _ why: String = "",
               _ idx: Int = 0, _ cnt: Int = 0) -> String {
            state.withCString { sp in
                why.withCString { wp in
                    guard let ptr = ds_engine_state_word_files(
                        sp, progress, wp, Int64(idx), Int64(cnt)) else { return state }
                    defer { ds_string_free(ptr) }
                    return String(cString: ptr)
                }
            }
        }
        switch self {
        case .missing: return w("missing")
        case .idle: return w("idle")
        case .downloading(let p, let idx, let cnt): return w("downloading", p, "", idx, cnt)
        case .warming: return w("warming")
        case .running: return w("running")
        case .failed(let why): return w("failed", 0, why)
        case .blocked: return w("blocked")
        }
    }

    /// The one-line note for the row's expanded section when the engine ISN'T ready: its state
    /// word for the pending/orange (downloading, starting, needs-permission, not-downloaded) and
    /// failed/red states. `nil` when running or idle — then the row shows its normal stats.
    var troubleNote: String? {
        switch self {
        case .missing, .downloading, .warming, .blocked, .failed: return word
        case .idle, .running: return nil
        }
    }
}

/// The single status indicator: right-aligned, color + shape coded, no extra text.
struct StatusDot: View {
    let status: EngineStatus
    init(_ status: EngineStatus) { self.status = status }

    private let size: CGFloat = 10

    // No hover tooltip on any dot — a not-ready engine surfaces its state as a line in the
    // row's expanded section instead (see `EngineStatus.troubleNote` / `statEngineRow`).
    var body: some View {
        Group {
            switch status {
            case .missing:
                Circle().strokeBorder(Color.secondary.opacity(0.5), lineWidth: 1.5)
            case .idle:
                Circle().fill(Color.secondary.opacity(0.45))
            case .downloading(let p, _, _):
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

/// Trailing indicator for an EXPANDABLE row: the row's status dot when collapsed,
/// an up-chevron when expanded — a pure crossfade (no rotation/scale) so it's clear
/// which header owns the open section. Rides the caller's `withAnimation` around the
/// expand toggle.
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
    @State private var engineExpanded = false
    @State private var capsExpanded = false
    @State private var kokoroExpanded = false
    @State private var parakeetExpanded = false
    @State private var systemExpanded = false
    @State private var ttsSystemExpanded = false
    @State private var claudeCodeExpanded = false
    @State private var diarExpanded = false

    /// The app version number ("0.2.0"), from the shared Rust source; a dash if absent.
    private var versionNumber: String {
        guard let ptr = ds_version() else { return L.t("common.dash") }
        defer { ds_string_free(ptr) }
        return String(cString: ptr)
    }


    /// Roll-up grant state of the OS permissions nested under Caps Lock — folded into the
    /// Caps header dot via `capsCombined`. Orange ONLY when a permission is actually DENIED;
    /// a not-yet-requested one (the mic is `.unknown` until first dictation prompts it) is
    /// not a problem the user must act on, so it must not flag the header. (Input Monitoring
    /// is intentionally absent: Accessibility subsumes it, so it never needs a separate grant.)
    private var permsRollup: Grant {
        [core.perms.accessibility, core.perms.microphone].contains(.denied) ? .denied : .granted
    }

    /// The Caps Lock header dot's combined state: the live caps loop (running / blocked /
    /// idle) folded together with the nested permission grants. A DENIED permission surfaces
    /// as orange on the header — the same "needs action" cue as caps being enabled-but-blocked
    /// — so the collapsed header flags a permission problem without the user opening it.
    private var capsCombined: EngineStatus {
        if permsRollup == .denied { return .blocked }
        if core.activity.capsRunning { return .running }
        return core.activity.capsWanted ? .blocked : .idle
    }

    var body: some View {
        // A Control-Center / HUD layout: the Status pane of the merged sidebar window. The
        // window chrome (the continuous Liquid-Glass slab + the state-tinted traffic-light
        // strip) lives ONCE on the `MainWindow` container; this view is just the scrollable
        // content — the former Form sections are translucent "platters" on the shared glass.
        ScrollView {
            VStack(spacing: 12) {
                // Engine platter — the headline row; expands to lifetime totals (seconds
                // spoken + heard, all sessions). Client integrations are wired via the
                // `wire` MCP tool, not here — the panel stays status-only.
                Platter {
                    engineRowBlock
                }

                // Engines platter — one row per engine; the lifecycle dot folds together
                // "downloaded?" and "running?". Each row leads with its ROLE (TTS / STT);
                // the concrete backend/model is the light secondary qualifier.
                Platter {
                    ttsEngineRow
                    PlatterDivider()
                    sttEngineRow
                    // Speaker diarization (on-demand) — same lifecycle dot as STT/TTS;
                    // gray/idle when disabled, green when enabled+ready.
                    PlatterDivider()
                    statEngineRow(L.t("status.engine.role_diar"), L.t("status.engine.pyannote"), core.engineDots.diarizer, $diarExpanded) { diarStats() }
                }

                // Caps Lock platter — the dictation capture loop; expands to its tap/hold
                // reference followed by the OS permissions it needs (Accessibility / Mic),
                // whose grant state also folds into this header's dot.
                Platter {
                    capsRowBlock
                }
            }
            .windowContentInset()
            // Wrap the window to the Status page on first open: this measures the natural content
            // height (incl. the 16pt top/bottom insets) and sizes the window to it ONCE, so the
            // last platter sits exactly one side-margin from the bottom instead of floating above
            // empty space. Measured on the VStack inside the ScrollView, so it's the un-clipped
            // height; the one-shot resize lives in `WrapWindowToContentHeight`.
            .background(GeometryReader { proxy in
                WrapWindowToContentHeight(contentHeight: proxy.size.height)
            })
        }
        .scrollIndicators(.hidden)
        // Fills the detail pane; the window is resized via the container, not this pane.
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// The headline engine row: tap to expand to lifetime totals; while open the dot
    /// crossfades to a chevron (ExpandDot), same as every expandable row.
    @ViewBuilder
    private var engineRowBlock: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                // App name + version, mirroring the engine rows' role + secondary-detail
                // layout (TTS · Kokoro, STT · Parakeet).
                HStack(spacing: 6) {
                    Text(L.t("common.app_name")).glassRowTitle()
                    // The version links to the homepage: its own tap gesture takes clicks
                    // within its bounds, so a tap elsewhere on the row still expands usage.
                    Text(versionNumber).glassRowDetail()
                        .contentShape(Rectangle())
                        .busyCursor()
                        .onTapGesture { core.openHomepage() }
                }
                Spacer()
                ExpandDot(expanded: engineExpanded) { StatusDot(core.activity.engineRunning ? .running : .idle) }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { withAnimation(.snappy(duration: 0.2)) { engineExpanded.toggle() } }
            if engineExpanded {
                PlatterDivider()
                detailBlock { dontSpeakDetails() }
            }
        }
    }

    /// The TTS engine row, showing the CONCRETE engine for the selected `tts_engine`:
    /// system → "System" (macOS `say`, nothing to download), built_in → "Kokoro" (the
    /// neural model it runs; the setting is "built_in" but here we name what's speaking).
    @ViewBuilder
    private var ttsEngineRow: some View {
        switch core.selection.ttsEngine {
        case "off":
            offEngineRow(L.t("status.engine.role_tts"))
        case "system":
            statEngineRow(L.t("status.engine.role_tts"), L.t("status.engine.system"), core.engineDots.ttsSystem, $ttsSystemExpanded) { ttsStats() }
        default:
            statEngineRow(L.t("status.engine.role_tts"), L.t("status.engine.kokoro"), core.engineDots.kokoro, $kokoroExpanded) { ttsStats() }
        }
    }

    /// A disabled (off) engine row: just the role + a gray idle dot (no detail label, no
    /// tooltip — the gray dot alone conveys "off"), not expandable. Used when
    /// `tts_engine`/`stt_engine` is `off`.
    @ViewBuilder
    private func offEngineRow(_ role: String) -> some View {
        HStack(spacing: 8) {
            Text(role).glassRowTitle()
            Spacer()
            StatusDot(.idle)
        }
        .frame(maxWidth: .infinity)
        .platterRow()
    }

    /// The STT engine row, showing the CONCRETE engine ACTUALLY running for the selected
    /// `stt_engine`: claude_code → "Claude Code" (delegate), system → "System" (on-device
    /// recognizer), built_in → "Parakeet" (the model it runs). Nothing to download for the
    /// first two; here we name what's actually transcribing.
    @ViewBuilder
    private var sttEngineRow: some View {
        switch core.selection.sttEngine {
        case "off":
            offEngineRow(L.t("status.engine.role_stt"))
        case "claude_code":
            statEngineRow(L.t("status.engine.role_stt"), L.t("status.engine.claude_code"), core.engineDots.claudeCode, $claudeCodeExpanded) { sttStats() }
        case "system":
            statEngineRow(L.t("status.engine.role_stt"), L.t("status.engine.system"), core.engineDots.system, $systemExpanded) { sttStats() }
        default:
            statEngineRow(L.t("status.engine.role_stt"), L.t("status.engine.parakeet"), core.engineDots.parakeet, $parakeetExpanded) { sttStats() }
        }
    }

    /// Caps Lock — the push-to-talk / barge-in capture loop (green while the engine's
    /// Caps loop is live, orange when enabled but blocked by a missing permission, gray
    /// when off). A subsystem status, NOT a permission, so it leads this group. Expands to
    /// a brief tap/hold reference (what the key does in each mode).
    @ViewBuilder
    private var capsRowBlock: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                Text(L.t("status.caps_lock")).glassRowTitle()
                Spacer()
                ExpandDot(expanded: capsExpanded) {
                    StatusDot(capsCombined)
                }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { withAnimation(.snappy(duration: 0.2)) { capsExpanded.toggle() } }
            if capsExpanded {
                PlatterDivider()
                detailBlock {
                    hint("status.caps_tap")
                    hint("status.caps_hold")
                }
                // The OS permissions the Caps loop + dictation need — formerly their own
                // section, now nested here (their grant state folds into the header dot above).
                PlatterDivider()
                permRow(L.t("status.permission.accessibility"), core.perms.accessibility,
                        L.t("status.permission.accessibility_purpose"), "Privacy_Accessibility")
                PlatterDivider()
                permRow(L.t("status.permission.microphone"), core.perms.microphone,
                        L.t("status.permission.microphone_purpose"), "Privacy_Microphone")
            }
        }
    }

    /// Stacks an expanded row's detail content with consistent platter insets — the
    /// in-platter equivalent of the grouped-Form sub-rows it replaces.
    @ViewBuilder
    private func detailBlock<C: View>(@ViewBuilder _ content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 8) { content() }
            // Restore the Form's label-left / value-right spread for the LabeledContent
            // rows, which a plain VStack would otherwise center.
            .labeledContentStyle(.spread)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 14)
            .padding(.vertical, 9)
    }

    /// A muted one-line hint — the empty-states explainer.
    @ViewBuilder
    private func hint(_ key: String) -> some View {
        Text(L.t(key)).glassCaption()
    }

    /// Interpolated variant (e.g. the claude_code hint naming the synthesized key).
    @ViewBuilder
    private func hint(_ key: String, _ params: [String: String]) -> some View {
        Text(L.t(key, params)).glassCaption()
    }

    /// One permission row: name + what it's for, a live grant badge, and a button
    /// that opens the exact System Settings → Privacy pane.
    @ViewBuilder
    private func permRow(_ name: String, _ grant: Grant, _ purpose: String, _ pane: String) -> some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(name).glassRowTitle()
                Text(purpose).glassCaption()
            }
            Spacer()
            // Open the matching System Settings pane — icon button, BEFORE the dot.
            Button { core.openPrivacyPane(pane) } label: {
                Image(systemName: "arrow.up.forward.app")
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help(L.t("status.permission.open_settings_help"))
            .linkCursor()
            // Status dot — far right, same column as the Caps Lock dot.
            grantDot(grant)
        }
        .frame(maxWidth: .infinity)
        .platterRow()
    }

    /// Grant shown as a single dot (no text), matching the StatusDot column:
    /// green = granted, orange = not granted, gray ring = unknown.
    @ViewBuilder
    private func grantDot(_ grant: Grant) -> some View {
        Group {
            switch grant {
            case .granted: Circle().fill(Color.green)
            case .denied: Circle().fill(Color.orange)
            case .unknown: Circle().strokeBorder(Color.secondary.opacity(0.5), lineWidth: 1.5)
            }
        }
        .frame(width: 10, height: 10)
        .help(grant == .granted ? L.t("status.grant.granted") : (grant == .denied ? L.t("status.grant.denied") : L.t("status.grant.unknown")))
    }

    /// An engine row that expands to reveal its stats: a tappable header (the ROLE — TTS /
    /// STT / diarization — with the concrete backend/model as a light secondary qualifier),
    /// a status dot that crossfades to a chevron while open, and the stats shown via `if`
    /// when expanded. Models download automatically on first activation, so there is NO
    /// manual Download/Retry button — the dot alone conveys missing → downloading → running.
    @ViewBuilder
    private func statEngineRow<Content: View>(_ role: String, _ detail: String, _ status: EngineStatus,
                                              _ expanded: Binding<Bool>,
                                              @ViewBuilder _ content: () -> Content) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                HStack(spacing: 6) {
                    Text(role).glassRowTitle()
                    Text(detail).glassRowDetail()
                }
                Spacer()
                ExpandDot(expanded: expanded.wrappedValue) { StatusDot(status) }
            }
            .frame(maxWidth: .infinity)
            .platterRow()
            .contentShape(Rectangle())
            .onTapGesture { withAnimation(.snappy(duration: 0.2)) { expanded.wrappedValue.toggle() } }
            if expanded.wrappedValue {
                PlatterDivider()
                detailBlock {
                    // Not ready (pending/failed) → show the state here, where the stats would be;
                    // running/idle → the engine's own stats. (Replaces the old dot tooltip.)
                    if let note = status.troubleNote {
                        Text(note).glassCaption()
                    } else {
                        content()
                    }
                }
            }
        }
    }

    /// The "DontSpeak" row's expanded details: lifetime usage — total seconds spoken (TTS)
    /// and heard (STT), summed across all sessions and persisted by the engine. Updates
    /// live off the status push.
    @ViewBuilder
    private func dontSpeakDetails() -> some View {
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

    /// A lifetime-total row label: the metric name (TTS/STT) with a light "all-time"
    /// qualifier, so the cumulative-across-all-sessions meaning is clear. No icon.
    @ViewBuilder
    private func lifetimeLabel(_ name: String) -> some View {
        HStack(spacing: 6) {
            Text(name).glassRowTitle()
            Text(L.t("status.stats.lifetime_all_time")).glassRowDetail()
        }
    }

    /// Seconds → a duration shown DOWN TO SECONDS so these lifetime rows visibly tick up
    /// as usage accrues. Resolved by the shared Rust formatter (`ds_duration_live`)
    /// so the bucket selection + leading-zero-unit rule live in ONE place for every UI.
    private func durationText(_ secs: Double) -> String {
        smTake(ds_duration_live(secs))
    }

    /// A session-count row — "<count>  <secs> s" — via the SHARED `ds_stats_count`
    /// formatter (was a hand-built HStack; de-dup-first, so the secondary tint is flattened).
    @ViewBuilder
    private func countRow(_ label: String, _ count: Int, _ secs: Double) -> some View {
        LabeledContent {
            Text(smTake(ds_stats_count(UInt64(count), secs))).monospacedDigit()
        } label: {
            Text(label).glassRowTitle()
        }
    }

    /// A stat RANGE row — "avg<unit>  ·  lo–hi" — via the SHARED `ds_stats_range`
    /// formatter (replaces the two-tone `StatRange`; de-dup-first, styling flattened for now).
    @ViewBuilder
    private func rangeRow(_ title: String, _ lo: Double, _ avg: Double, _ hi: Double,
                          _ precision: UInt32, _ unitKey: String) -> some View {
        LabeledContent {
            Text(smTake(ds_stats_range(lo, avg, hi, precision, unitKey))).monospacedDigit()
        } label: {
            Text(title).glassRowTitle()
        }
    }

    /// TTS stats for the ACTIVE engine. System `say` synthesizes in the OS (no local RTF
    /// to report), so it shows a one-line note + a link out to System Settings → Spoken
    /// Content where its voices and per-language packs live. Kokoro shows the live stats.
    @ViewBuilder
    private func ttsStats() -> some View {
        if core.selection.ttsEngine == "system" {
            // System `say` synthesizes in the OS — no local stats. A normal expander row
            // (label left, open-icon in the value column) whose WHOLE row is clickable,
            // opening Spoken Content where the `say` voices and per-language packs live.
            LabeledContent {
                Image(systemName: "arrow.up.forward.app").foregroundStyle(.secondary)
            } label: {
                Text(L.t("status.tts_system_settings"))
            }
            .contentShape(Rectangle())
            .onTapGesture { core.openSpokenContentSettings() }
            .linkCursor()
        } else {
            // Lead with the active RUNTIME — CPU / CUDA / Core ML / Core ML · ANE — the
            // speech-OUT analogue of the Parakeet runtime row, so "Kokoro on the ANE vs CPU"
            // has a clean readout. (System TTS, handled above, has no local runtime.)
            if let prov = core.selection.ttsProvider {
                LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(prov))
            }
            // Ready by the time we get here (pending/failed is handled in statEngineRow).
            let s = core.stats.tts
            if s.utterances == 0 {
                hint("status.no_data")
            } else {
                rangeRow(L.t("status.stats.realtime"), s.rtfMin, s.rtfAvg, s.rtfMax, 2, "status.stats.unit.times")
                rangeRow(L.t("status.stats.first_audio"), s.firstMinMs / 1000, s.firstAvgMs / 1000, s.firstMaxMs / 1000, 1, "status.stats.unit.seconds")
                countRow(L.t("status.stats.spoken"), s.utterances, s.audioSecs)
                if s.failures > 0 {
                    LabeledContent(L.t("status.stats.failures"), value: "\(s.failures)").foregroundStyle(.red)
                }
            }
        }
    }

    /// STT stats for the ACTIVE engine (Parakeet / System / Claude Code) — the same
    /// realtime-factor + count display for whichever is selected; the "not yet" hint is
    /// engine-specific so it never mislabels System/Claude Code as Parakeet.
    @ViewBuilder
    private func sttStats() -> some View {
        // For built_in (Parakeet), lead with the active RUNTIME — Core ML / ANE vs ONNX —
        // the speech-IN analogue of the Runtime row's TTS provider, so "Parakeet on the ANE
        // vs CPU" has a clean readout. (System/Claude Code have no local runtime to show.)
        if core.selection.sttEngine == "built_in", let prov = core.selection.sttProvider {
            LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(prov))
        }
        // Ready by the time we get here (pending/failed is handled in statEngineRow). Claude
        // Code does no local transcription — it delegates — so instead of stats it names the
        // key it sends through; the local engines show their realtime/count stats.
        let s = core.stats.stt
        if core.selection.sttEngine == "claude_code" {
            if let k = core.selection.claudeCodeKey, !k.isEmpty {
                hint("status.stt_claude_code", ["key": k])
            } else {
                hint("status.stt_claude_code_off")
            }
        } else if s.transcriptions == 0 {
            hint("status.no_data")
        } else {
            rangeRow(L.t("status.stats.realtime"), s.rtfMin, s.rtfAvg, s.rtfMax, 2, "status.stats.unit.times")
            countRow(L.t("status.stats.transcribed"), s.transcriptions, s.audioSecs)
        }
    }

    /// The runtime TOKEN → short label, via the SHARED `ds_runtime_label` formatter
    /// (was a hand-written switch duplicated with the Windows/Linux hosts; now ONE mapping).
    private func runtimeLabel(_ provider: String) -> String {
        smTake(ds_runtime_label(provider))
    }

    /// Diarization stats. Numbers only make sense once at least one voice is enrolled (so
    /// the recognized names + sensitivity have something to label); until then show only a
    /// prompt to enroll — the green dot already conveys "engine ready". Once set up, lead
    /// with the RUNTIME line (Core ML / ANE), mirroring STT/TTS, then who it recognizes and
    /// the clustering sensitivity.
    @ViewBuilder
    private func diarStats() -> some View {
        let s = core.stats.diarization
        if !s.enabled {
            hint("status.diarization_disabled")
        } else if s.speakers.isEmpty {
            // Enabled + ready, but not set up yet — prompt to enroll; no numbers.
            hint("status.diarization_no_speakers")
        } else {
            if !s.runtime.isEmpty {
                LabeledContent(L.t("status.engine.role_runtime"), value: runtimeLabel(s.runtime))
            }
            LabeledContent(L.t("status.diarization_enrolled"),
                           value: s.speakers.joined(separator: ", "))
            LabeledContent(L.t("status.diarization_sensitivity"),
                           value: String(format: "%.2f", s.clusteringThreshold))
        }
    }

    /// Take ownership of a Rust-owned `char*` (always free it) and return a Swift String.
    private func smTake(_ ptr: UnsafeMutablePointer<CChar>?) -> String {
        guard let ptr else { return "" }
        defer { ds_string_free(ptr) }
        return String(cString: ptr)
    }
}

/// Shows the pointing-hand (link) cursor on hover, to signal a clickable link.
/// Uses the native `pointerStyle` on macOS 15+; on older macOS it sets `NSCursor`
/// continuously — a plain `.onHover` + push/pop is unreliable INSIDE a Form/List,
/// where the backing table view resets the cursor on every mouse-moved event.
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
    func busyCursor() -> some View { modifier(BusyCursorOnHover()) }
}

/// Shows the colorful spinning wait pinwheel (the macOS "beachball") on hover. Uses the
/// private `+[NSCursor busyButClickableCursor]` — the same rainbow cursor the system shows
/// for a busy-but-responsive app — and falls back to the link cursor if the selector is
/// ever absent. `onContinuousHover` (not pointerStyle) because there is no SwiftUI
/// pointerStyle for the beachball, and a plain hover is unreliable inside a Form/List.
private struct BusyCursorOnHover: ViewModifier {
    private static var busy: NSCursor {
        let sel = NSSelectorFromString("busyButClickableCursor")
        if NSCursor.responds(to: sel),
           let c = NSCursor.perform(sel)?.takeUnretainedValue() as? NSCursor {
            return c
        }
        return .pointingHand
    }
    func body(content: Content) -> some View {
        content.onContinuousHover { phase in
            switch phase {
            case .active: Self.busy.set()
            case .ended: NSCursor.arrow.set()
            }
        }
    }
}

