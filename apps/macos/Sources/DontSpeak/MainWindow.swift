// Single sidebar window (Status / Tools / Logs / Libraries). Chrome lives once:
// Liquid-Glass slab + frosted title-bar strip tinted from `TrayState` (orange dictating /
// purple narrating).

import AppKit
import SwiftUI

/// Sidebar screens; titles from shared i18n (`common.nav_*`) to match Windows tabs.
enum AppScreen: String, CaseIterable, Identifiable {
    case status, tools, log, credits
    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .status: return "common.nav_status"
        case .tools: return "common.nav_tools"
        case .log: return "common.nav_log"
        case .credits: return "common.nav_credits"
        }
    }

    var systemImage: String {
        switch self {
        case .status: return "waveform"
        case .tools: return "wrench.and.screwdriver"
        case .log: return "doc.plaintext"
        case .credits: return "books.vertical"
        }
    }
}

struct MainWindow: View {
    @Environment(Core.self) private var core

    /// Title-bar wash from the same `TrayState.tint` as the menu-bar pill.
    private var stateTint: Color {
        guard let c = TrayState.current(core).tint else { return .clear }
        return Color(nsColor: c).opacity(0.5)
    }

    /// System title-bar height (no hardcoded constant). Plain `[.titled]` mask: the real
    /// window's `.fullSizeContentView` would make the inset zero.
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
            // Range form only — single-value `navigationSplitViewColumnWidth(_:)` crashes
            // AppKit split-view KVO on open. Divider drag stopped via `.lockSidebarDivider()`.
            .navigationSplitViewColumnWidth(min: 150, ideal: 150, max: 150)
            .scrollContentBackground(.hidden)
            // Respect title-bar safe area so the sidebar material doesn't tuck under traffic lights.
            .background { Rectangle().fill(.ultraThinMaterial) }
            // `contentMargins` is a no-op on macOS sidebar List; safe-area inset shifts the first row.
            .safeAreaInset(edge: .top, spacing: 0) {
                Color.clear.frame(height: Glass.windowTopInset / 2)
            }
            // Dropping the collapse control removes its toolbar so the state-tint strip spans full width.
            .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.balanced)
        // idealHeight == minHeight: first open snug to Status; wider panes scroll inside.
        // Restoration off (`closeOnlyWindow`) so every open uses this size.
        .frame(minWidth: 460, idealWidth: 510, minHeight: 320, idealHeight: 320)
        .windowGlass(topTint: stateTint, topHeight: titleBarHeight)
        .glassWindow()
        .closeOnlyWindow()
        .lockSidebarDivider()
    }

    @ViewBuilder private var detail: some View {
        switch core.screen {
        case .status: StatusView()
        case .tools: ToolsView()
        case .log: LogView()
        case .credits: CreditsView()
        }
    }
}
