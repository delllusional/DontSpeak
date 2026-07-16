// Menu-bar dropdown: Mute, Settings (sidebar window), Quit. Quitting stops the in-process engine.

import SwiftUI

struct TrayMenu: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(Core.self) private var core

    var body: some View {
        // Button (not Toggle): leading speaker glyph stays in the same column as Settings/Quit;
        // Toggle's checkmark gutter misaligns. Glyph carries on/off — no checkmark needed.
        Button {
            core.setMuted(!core.activity.muted)
        } label: {
            Label(
                L.t("tray.mute"),
                systemImage: core.activity.muted ? "speaker.slash" : "speaker.wave.2")
        }

        // Mute + Settings share a group; only Quit gets a separator (HIG: related items together).
        Button {
            open(.status)
        } label: {
            Label(L.t("tray.settings"), systemImage: "wrench.and.screwdriver")
        }

        Divider()

        Button {
            NSApp.terminate(nil)
        } label: {
            Label(L.t("tray.quit"), systemImage: "power")
        }
    }

    /// Select `screen` first so an already-open window also jumps to it.
    private func open(_ screen: AppScreen) {
        core.screen = screen
        openWindow.activating("main")
    }
}
