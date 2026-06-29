//  MainWindow.swift
//
//  The single app window: a sidebar of screens (Status / Tools / Logs / Libraries — the same
//  set the Windows app exposes as top tabs) over one detail pane. This MERGES the former
//  standalone `status` + `tools` windows into one and adds the two screens macOS was missing.
//
//  The window CHROME lives here exactly ONCE: the continuous Liquid-Glass slab plus the
//  frosted TITLE-BAR strip that washes to the live engine state (orange dictating / purple
//  narrating). That strip — the colored traffic-light bar — is unchanged from the old windows:
//  same `windowGlass` overlay, same `TrayState` source, same system-derived height, now
//  spanning the full width above both the sidebar and the detail.

import SwiftUI
import AppKit

/// The screens in the sidebar, in display order. Titles come from the shared i18n catalog
/// (`common.nav_*`) so they match the Windows tabs; the SF Symbols are the macOS-native cue.
enum AppScreen: String, CaseIterable, Identifiable {
    case status, tools, logs, libraries
    var id: String { rawValue }

    var titleKey: String {
        switch self {
        case .status: return "common.nav_status"
        case .tools: return "common.nav_tools"
        case .logs: return "common.nav_logs"
        case .libraries: return "common.nav_libraries"
        }
    }

    var systemImage: String {
        switch self {
        case .status: return "waveform"
        case .tools: return "wrench.and.screwdriver"
        case .logs: return "doc.plaintext"
        case .libraries: return "books.vertical"
        }
    }
}

struct MainWindow: View {
    @Environment(Core.self) private var core

    /// The whole-window state wash — the live tray color (narrating = purple, dictating =
    /// orange) at soft opacity, clear when idle. From the shared `TrayState.tint` (the SAME
    /// source as the menu-bar pill), so the window's title-bar strip and the menu bar can't
    /// drift; `windowGlass` crossfades the strip between these values.
    private var stateTint: Color {
        guard let c = TrayState.current(core).tint else { return .clear }
        return Color(nsColor: c).opacity(0.5)
    }

    /// The title-bar height, derived from the system (no hardcoded constant) so the state-tint
    /// band covers exactly the traffic-light strip on any macOS version. `frameRect(forContentRect:)`
    /// adds the title-bar inset to a zero-height content rect — computed with a plain `[.titled]`
    /// mask because the real window's `.fullSizeContentView` would otherwise make the inset zero.
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
            // FIXED sidebar width — equal min/ideal/max pins the column so the split divider
            // can't be dragged: the sidebar is just wide enough for the four labels and never
            // resizes; only the detail pane takes the window's slack. (Use the range form, NOT
            // the single-value `navigationSplitViewColumnWidth(_:)` — that variant crashes
            // AppKit's split-view KVO on window open.)
            .navigationSplitViewColumnWidth(min: 170, ideal: 190, max: 240)
            // Let the window's glass slab show through the sidebar rather than an opaque list.
            .scrollContentBackground(.hidden)
            // Run the sidebar column the FULL window height — UP UNDER the title-bar strip — so
            // the left column reads like the System Settings sidebar (continuous from the top,
            // with the state-tint strip just washing over its top edge rather than stopping at a
            // seam). `ignoresSafeArea` bleeds the column material past the title-bar safe area;
            // the strip overlay (added in `windowGlass`, in FRONT) still spans the full width, so
            // its coloring stays unbroken. The rows themselves still start below the bar via the
            // content margin below.
            .background { Rectangle().fill(.ultraThinMaterial).ignoresSafeArea() }
            // Because the column bleeds UNDER the title-bar strip, its scroll content starts at the
            // very window top — so the first row would tuck under the traffic lights. Inset it by
            // the title-bar height PLUS the standard top margin, so the first row clears the bar and
            // lands LEVEL with the detail's first platter (which gets `windowTopInset` below the
            // title-bar safe area).
            .contentMargins(.top, titleBarHeight + Glass.windowTopInset, for: .scrollContent)
            // Drop the sidebar-collapse button. It lives in an AppKit toolbar pinned to the
            // title-bar region over the SIDEBAR — that toolbar is what kept the state-tint
            // strip from spanning the full width. With no toolbar, the strip covers the whole
            // top and the sidebar reads as sitting UNDER one continuous title bar.
            .toolbar(removing: .sidebarToggle)
        } detail: {
            detail
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .navigationSplitViewStyle(.balanced)
        // Opens COMPACT — a narrow sidebar (~150) over a Status-width detail (~350), so the
        // window wraps the Status page snugly instead of sprawling. Floors keep both columns
        // usable; the wider panes (Tools/Libraries) scroll internally and the user can drag the
        // window out when they want more room. `minHeight` is low so the first-open
        // wrap-to-Status resize (`WrapWindowToContentHeight`) can shrink to the short Status page
        // without the content-min floor fighting it.
        .frame(minWidth: 660, idealWidth: 720, minHeight: 320, idealHeight: 640)
        // One continuous glass slab behind everything; the host window is itself clear. The
        // TITLE-BAR strip tints to the live state and crossfades between states — the colored
        // traffic-light bar, unchanged, now spanning the full window width.
        .windowGlass(topTint: stateTint, topHeight: titleBarHeight)
        .glassWindow()
        // No minimize button (Close + standard resizable zoom), as the old windows had.
        .closeOnlyWindow()
    }

    @ViewBuilder private var detail: some View {
        switch core.screen {
        case .status: StatusView()
        case .tools: ToolsView()
        case .logs: LogsView()
        case .libraries: LibrariesView()
        }
    }
}
