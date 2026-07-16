// Combined activity log via `ds_logs_json` / `LogFeed` (`ds_logs_wait`). Filter + source
// order: pure `LogCatalog`; colors: shared `Brand.logSourcePalette` / `logLevelColor`.

import AppKit
import DontSpeakLogic
import SwiftUI

struct LogView: View {
    @State private var feed = LogFeed()
    @State private var filter: String = ""
    @State private var showClearConfirm = false

    private var shown: [(index: Int, line: LogLine)] {
        LogCatalog.filterIndexed(feed.lines, query: filter)
    }

    var body: some View {
        VStack(spacing: 10) {
            HStack(spacing: 8) {
                SearchField(text: $filter)
                Button {
                    showClearConfirm = true
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .foregroundStyle(.secondary)
                .disabled(feed.lines.isEmpty)
                .help(L.t("logs.clear"))
            }
            .confirmationDialog(
                L.t("logs.clear_confirm_title"),
                isPresented: $showClearConfirm,
                titleVisibility: .visible
            ) {
                Button(L.t("logs.clear_confirm_action"), role: .destructive) {
                    feed.clear()
                }
                Button(L.t("common.cancel"), role: .cancel) {}
            }

            ScrollView {
                logBody
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
                    .padding(14)
            }
            .scrollIndicators(.visible)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .platterBackground()
        }
        .windowContentInset()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { feed.start() }
        .onDisappear { feed.stop() }
    }

    @ViewBuilder
    private var logBody: some View {
        // Filter once per render (emptiness check + ForEach).
        let result = shown
        if result.isEmpty {
            Text(L.t(feed.lines.isEmpty ? "logs.empty" : "logs.no_match"))
                .glassCaption()
        } else {
            LazyVStack(alignment: .leading, spacing: 2) {
                // Unfiltered index = stable row id while the filter box is typed.
                ForEach(result, id: \.index) { row in
                    lineText(row.line)
                }
            }
            .font(.system(.caption, design: .monospaced))
        }
    }

    /// Source tag (palette color) + non-INFO level + message (ERROR/WARN tint). Mirrors Windows.
    private func lineText(_ line: LogLine) -> Text {
        let levelColor = Brand.logLevelColor(line.level).map { Color(nsColor: $0) }
        var t = Text(line.source).fontWeight(.semibold)
            .foregroundStyle(sourceColor(line.source))
        t = t + Text("  ")
        if !line.level.isEmpty, line.level != "INFO" {
            t = t + Text(line.level + " ").foregroundStyle(levelColor ?? .secondary)
        }
        if let levelColor {
            t = t + Text(line.text).foregroundStyle(levelColor)
        } else {
            t = t + Text(line.text)
        }
        return t
    }

    private func sourceColor(_ source: String) -> Color {
        let palette = Brand.logSourcePalette
        guard !source.isEmpty, !palette.isEmpty,
            let idx = LogCatalog.colorIndex(for: source, in: feed.orderedSources)
        else { return .secondary }
        return Color(nsColor: palette[idx % palette.count])
    }
}

/// Custom search field: native NSSearchField draws an opaque white bezel that clashes with glass.
private struct SearchField: View {
    @Binding var text: String
    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("", text: $text)
                .textFieldStyle(.plain)
        }
        .font(.body)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .glassBackground(cornerRadius: 8)
    }
}
