//  LogView.swift
//
//  The Log tab: a read-only tail of the COMBINED activity log (the unified log + every
//  sibling auxiliary log), read via the FFI (ds_logs_json → the shared ds-config combined-log
//  reader) — the SAME file the engine writes, so it can't drift from what actually happened.
//  Reloaded each time the tab is shown (no poll timer). A top filter bar narrows lines live;
//  below it the color-coded (per-source + ERROR/WARN), selectable, wrapped log scrolls.
//
//  The ordering + filter rules are the pure `LogCatalog` (in DontSpeakLogic, unit-tested); the
//  colors are the shared `Brand.logSourcePalette` / `Brand.logLevelColor`. This view only
//  wires the FFI read to that logic and renders it.

import SwiftUI
import AppKit
import CDontSpeak
import DontSpeakLogic

/// Read the combined log tail from the FFI and decode it into `[LogLine]`. `maxBytes` caps the
/// tail PER source file (matching the Windows tab's 64 KB), so a long-running session shows a
/// bounded, recent window rather than the whole history.
private func loadLogLines(maxBytes: UInt32 = 64 * 1024) -> [LogLine] {
    ffiDecode([LogLine].self) { ds_logs_json(maxBytes) } ?? []
}

struct LogView: View {
    @State private var lines: [LogLine] = []
    /// Distinct sources in first-appearance order (empties dropped) — each source's palette
    /// color is its index here, so the coloring is stable + identical to every other platform.
    @State private var orderedSources: [String] = []
    @State private var filter: String = ""

    private var shown: [LogLine] { LogCatalog.filter(lines, query: filter) }

    var body: some View {
        // The Logs pane of the merged sidebar window — a filter bar over a scrollable colored
        // log on one platter. The glass slab + traffic-light strip live on `MainWindow`.
        VStack(spacing: 10) {
            // The live filter field — a glass-styled search field (magnifier glyph, no
            // placeholder); see `SearchField`. A native NSSearchField / `.roundedBorder` drew an
            // opaque white box that clashed with the glass.
            SearchField(text: $filter)

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
        .onAppear(perform: reload)
    }

    /// (Re)read the log and recompute the stable source order. Called on appear; the Logs tab
    /// reloads on show rather than polling, so reopening it picks up new lines.
    private func reload() {
        lines = loadLogLines()
        orderedSources = LogCatalog.distinctSources(lines).filter { !$0.isEmpty }
    }

    @ViewBuilder
    private var logBody: some View {
        // Run the filter ONCE per render and reuse it (it was previously read twice — for the
        // emptiness check and the ForEach — re-filtering every line twice per keystroke).
        let result = shown
        if result.isEmpty {
            // Distinguish "nothing logged yet" from "filter matched nothing", like Windows.
            Text(L.t(lines.isEmpty ? "logs.empty" : "logs.no_match"))
                .glassCaption()
        } else {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(result.enumerated()), id: \.offset) { _, line in
                    lineText(line)
                }
            }
            .font(.system(.caption, design: .monospaced))
        }
    }

    /// One rendered log line: the source tag (its stable palette color, semibold) + the level
    /// token when it isn't the ordinary INFO + the message (ERROR/WARN tint the message; INFO
    /// keeps the default text color). Mirrors the Windows `RenderLogLines`.
    private func lineText(_ line: LogLine) -> Text {
        let levelColor = Brand.logLevelColor(line.level).map { Color(nsColor: $0) }
        var t = Text(line.source).fontWeight(.semibold)
            .foregroundStyle(sourceColor(line.source))
        t = t + Text("  ")
        if !line.level.isEmpty, line.level != "INFO" {
            t = t + Text(line.level + " ").foregroundStyle(levelColor ?? .secondary)
        }
        // ERROR/WARN tint the message; INFO/unknown keeps the default text color.
        if let levelColor {
            t = t + Text(line.text).foregroundStyle(levelColor)
        } else {
            t = t + Text(line.text)
        }
        return t
    }

    /// The stable per-source color: the shared palette indexed by the source's first-appearance
    /// position. Empty source or an empty palette ⇒ the secondary text color.
    private func sourceColor(_ source: String) -> Color {
        let palette = Brand.logSourcePalette
        guard !source.isEmpty, !palette.isEmpty,
              let idx = LogCatalog.colorIndex(for: source, in: orderedSources)
        else { return .secondary }
        return Color(nsColor: palette[idx % palette.count])
    }
}

/// The Logs filter field. A native `NSSearchField` draws an OPAQUE white bezel (it matches
/// system chrome, not our glass), so it stood out as a white box on the panel. Instead we style
/// our own: a plain `TextField` + magnifier glyph on the SAME glass surface as the window slab /
/// sidebar (`glassBackground`), so it reads as lighter than — and distinct from — the
/// `platterBackground` log card below it. Empty by design — no placeholder string.
private struct SearchField: View {
    @Binding var text: String
    var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)
            TextField("", text: $text)
                .textFieldStyle(.plain)        // no opaque bezel / focus box — just the glyph + text
        }
        .font(.body)                           // standard control text size
        .padding(.horizontal, 10)
        .padding(.vertical, 7)                 // ≈ the system's standard single-line control height
        .glassBackground(cornerRadius: 8)      // the glass slab look of the left sidebar, not the card material
    }
}
