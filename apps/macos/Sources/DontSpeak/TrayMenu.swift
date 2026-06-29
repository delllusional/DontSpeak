//  TrayMenu.swift
//
//  The menu-bar dropdown: Mute, Settings (the single sidebar window), and Quit. Quitting
//  THIS app stops the in-process engine it hosts (the app owns the engine's lifecycle).
//  All control lives in DontSpeak.

import SwiftUI

struct TrayMenu: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(Core.self) private var core

    var body: some View {
        // Mute: silences the voice without stopping it (playback keeps draining). A Button with a
        // LEADING speaker glyph (crossed-out while muted, like the menu-bar icon) so its icon sits
        // in the SAME column as Settings/Quit — rather than a `Toggle`, whose checkmark lands in a
        // separate gutter to the left and reads as out of line with the other rows. The glyph
        // itself carries the on/off state, so no checkmark is needed.
        Button {
            core.setMuted(!core.activity.muted)
        } label: {
            Label(L.t("tray.mute"),
                  systemImage: core.activity.muted ? "speaker.slash" : "speaker.wave.2")
        }

        // Settings: opens the single sidebar window (Status / Tools / Logs / Libraries), landing
        // on Status. The crossed wrench-and-screwdriver is the app's Tools/settings glyph.
        // No divider above it: per Apple's HIG, separators group RELATED items — a divider between
        // every item just makes lone single-item groups. Mute + Settings form one group; only Quit
        // (the terminating action) gets its own separator below, the standard app-menu convention.
        Button {
            open(.status)
        } label: {
            Label(L.t("tray.settings"), systemImage: "wrench.and.screwdriver")
        }

        Divider()

        // Quit: the standard macOS power glyph. Quits the menu-bar app, which stops the
        // in-process engine it hosts.
        Button {
            NSApp.terminate(nil)
        } label: {
            Label(L.t("tray.quit"), systemImage: "power")
        }
    }

    /// Select `screen`, then bring the app forward and open the single window. Setting the
    /// screen first means an already-open window also jumps to it.
    private func open(_ screen: AppScreen) {
        core.screen = screen
        openWindow.activating("main")
    }
}
