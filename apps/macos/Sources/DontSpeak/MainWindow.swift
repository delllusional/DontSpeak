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
            // FIXED sidebar width — equal min/ideal/max pins the column so it's just wide enough
            // for the four labels and never resizes; only the detail pane takes the window's
            // slack. (Use the range form, NOT the single-value `navigationSplitViewColumnWidth(_:)`
            // — that variant crashes AppKit's split-view KVO on window open. The divider drag
            // itself is disabled separately via `.lockSidebarDivider()` below.)
            .navigationSplitViewColumnWidth(min: 150, ideal: 150, max: 150)
            // Let the window's glass slab show through the sidebar rather than an opaque list.
            .scrollContentBackground(.hidden)
            // Let the sidebar material RESPECT the title-bar safe area (no `ignoresSafeArea`), so
            // it stops at the bar instead of bleeding up under the traffic lights. The window glass
            // slab (behind) and the state-tint strip overlay (in FRONT, full width) still cover the
            // top region, so the strip stays continuous — the sidebar just no longer tucks under it.
            .background { Rectangle().fill(.ultraThinMaterial) }
            // Push the first row a small margin below the title-bar bar. `contentMargins(_:for:
            // .scrollContent)` is a NO-OP on a macOS sidebar List (verified: even +80 moved nothing);
            // the List DOES honor the safe area (that's how it already clears the title bar), so
            // extend the top safe area with a clear spacer — it shifts the first row down by exactly
            // this height. Half the standard inset (8pt) reads level with the detail's first platter.
            .safeAreaInset(edge: .top, spacing: 0) {
                Color.clear.frame(height: Glass.windowTopInset / 2)
            }
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
        // window wraps the Status page snugly instead of sprawling. The ideal height EQUALS the
        // min, so first-open lands at the minimal snug-to-Status size (the last platter one
        // side-margin from the bottom); the user drags the window out for the wider panes
        // (Tools/Credits), which scroll internally. Restoration is disabled (see
        // `closeOnlyWindow`) so every open uses this size, not the last dragged frame.
        .frame(minWidth: 460, idealWidth: 510, minHeight: 320, idealHeight: 320)
        // One continuous glass slab behind everything; the host window is itself clear. The
        // TITLE-BAR strip tints to the live state and crossfades between states — the colored
        // traffic-light bar, unchanged, now spanning the full window width.
        .windowGlass(topTint: stateTint, topHeight: titleBarHeight)
        .glassWindow()
        // No minimize button (Close + standard resizable zoom), as the old windows had.
        .closeOnlyWindow()
        // Pin the sidebar/detail divider so it can't be dragged (the fixed column width alone
        // doesn't stop AppKit's split divider from being grabbed and pushed off-screen).
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
