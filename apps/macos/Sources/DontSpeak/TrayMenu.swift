// Menu-bar: Mute, Settings, Quit. Quit stops the in-process engine.

import SwiftUI

struct TrayMenu: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(Core.self) private var core

    var body: some View {
        // Button not Toggle: Toggle checkmark gutter misaligns the speaker glyph column.
        Button {
            core.setMuted(!core.activity.muted)
        } label: {
            Label(
                L.t("tray.mute"),
                systemImage: core.activity.muted ? "speaker.slash" : "speaker.wave.2")
        }

        // Mute + Settings grouped; only Quit gets a separator. Re-open keeps last sidebar selection.
        Button {
            openWindow.activating("main")
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
}
