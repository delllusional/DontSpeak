// Sidebar window (Agents / Status / Tools / Logs / Credits). Chrome once: Liquid-Glass +
// frosted title-bar tinted from TrayState (orange dictating / purple narrating).

import AppKit
import SwiftUI

/// Sidebar screens; titles from common.nav_* (Windows tab parity).
enum AppScreen: String, CaseIterable, Identifiable {
    case agents, status, tools, log, credits
    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .status: return "common.nav_status"
        case .agents: return "common.nav_agents"
        case .tools: return "common.nav_tools"
        case .log: return "common.nav_log"
        case .credits: return "common.nav_credits"
        }
    }

    var systemImage: String {
        switch self {
        case .status: return "waveform"
        case .agents: return "chart.bar"
        case .tools: return "wrench.and.screwdriver"
        case .log: return "doc.plaintext"
        case .credits: return "books.vertical"
        }
    }
}

struct MainWindow: View {
    @Environment(Core.self) private var core

    /// Title-bar wash from the same TrayState.tint as the menu-bar pill.
    private var stateTint: Color {
        guard let c = TrayState.current(core).tint else { return .clear }
        return Color(nsColor: c).opacity(0.5)
    }

    /// System title-bar height. Plain [.titled] — fullSizeContentView would zero the inset.
    private var titleBarHeight: CGFloat {
        NSWindow.frameRect(forContentRect: .zero, styleMask: [.titled]).height
    }

    var body: some View {
        @Bindable var core = core
        return NavigationSplitView {
            List(AppScreen.allCases, selection: $core.screen) { screen in
                Label(L.t(screen.titleKey), systemImage: screen.systemImage)
                    .tag(screen)
            }
            // Range form only — single-value navigationSplitViewColumnWidth crashes AppKit KVO.
            .navigationSplitViewColumnWidth(min: 150, ideal: 150, max: 150)
            .scrollContentBackground(.hidden)
            .background { Rectangle().fill(.ultraThinMaterial) }
            // contentMargins is a no-op on sidebar List; safeAreaInset shifts the first row.
            .safeAreaInset(edge: .top, spacing: 0) {
                Color.clear.frame(height: Glass.windowTopInset / 2)
            }
            // Drop collapse control so the state-tint strip spans full width.
            .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.balanced)
        // Fixed first-open size; restoration off (closeOnlyWindow).
        .frame(minWidth: 460, idealWidth: 510, minHeight: 320, idealHeight: 320)
        .windowGlass(topTint: stateTint, topHeight: titleBarHeight)
        .glassWindow()
        .closeOnlyWindow()
        .lockSidebarDivider()
    }

    @ViewBuilder private var detail: some View {
        switch core.screen {
        case .status: StatusView()
        case .agents: UsageView()
        case .tools: ToolsView()
        case .log: LogView()
        case .credits: CreditsView()
        }
    }
}
