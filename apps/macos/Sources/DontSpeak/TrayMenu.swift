//  TrayMenu.swift
//
//  The menu-bar dropdown: Status (engine health & OS permissions), Tools (the MCP
//  tool reference), and Quit. Quitting THIS app stops the in-process engine it hosts
//  (the app owns the engine's lifecycle). All control lives in DontSpeak.

import SwiftUI

struct TrayMenu: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(Core.self) private var core

    var body: some View {
        // Mute: silences the voice without stopping it (playback keeps draining); the menu-bar
        // icon shows a diagonal slash while muted. A `Toggle` in a menu is the CORRECT checkmark idiom — SwiftUI
        // renders it as a native menu item with a RESERVED checkmark gutter, so the row width +
        // text alignment stay identical with and without the check (no size jump). A Button with
        // a `systemImage: "checkmark"` has no gutter and shifts the text when it toggles.
        Toggle(L.t("tray.mute"), isOn: Binding(
            get: { core.activity.muted },
            set: { core.setMuted($0) }
        ))

        Divider()

        Button(L.t("common.nav_status")) { openWindow.activating("status") }
        Button(L.t("common.nav_tools")) { openWindow.activating("tools") }

        Divider()

        Button(L.t("tray.quit")) {
            // Quits the menu-bar app, which stops the in-process engine it hosts.
            NSApp.terminate(nil)
        }
    }
}
