//  ToolsView.swift
//
//  A read-only reference of the DontSpeak tools available to Claude over MCP. The
//  list is the SAME catalog the MCP server gives Claude — read via the FFI
//  (ds_tools_json → the shared ds-tools crate), so it never drifts. Each
//  tool shows what it does and the arguments it accepts (name, type, required,
//  allowed values), read from the catalog's ORDERED `params` array (authored order).

import SwiftUI
import AppKit
import CDontSpeak

/// One argument of a tool, from the catalog's ordered `params` array.
struct ToolParam: Identifiable, Sendable {
    let name: String
    let type: String
    let required: Bool
    let detail: String       // "one of: a, b" / "0.5–2.0" / ""
    let description: String
    var id: String { name }
}

/// One tool as shown in the list.
struct ToolInfo: Identifiable, Sendable {
    let name: String
    let summary: String
    let params: [ToolParam]
    var id: String { name }
}

/// Wire shape of the FFI tools catalog (`ds_tools_json` → the shared ds-tools
/// crate's `catalog_ui`): an ordered array of tools, each with an ordered `params` array.
/// Decoded type-safely, then mapped to the `ToolParam`/`ToolInfo` view models above.
private struct ToolDTO: Decodable {
    let name: String
    let description: String?
    let params: [ParamDTO]?
}

private struct ParamDTO: Decodable {
    let name: String
    let type: String?
    let required: Bool?
    let description: String?
    let `enum`: [String]?
    let minimum: Double?
    let maximum: Double?
}

/// Format a JSON number without a trailing ".0" when it's whole (2.0 → "2").
private func num(_ v: Double) -> String {
    v == v.rounded() ? String(Int(v)) : String(v)
}

/// Map one decoded param to its view model, building the `detail` qualifier: an
/// enum's allowed values ("one of: …"), else a numeric min–max range.
private func toToolParam(_ p: ParamDTO) -> ToolParam {
    var detail = ""
    if let e = p.enum, !e.isEmpty {
        detail = L.t("tools.param.one_of", ["values": e.joined(separator: ", ")])
    } else if let lo = p.minimum, let hi = p.maximum {
        detail = "\(num(lo))–\(num(hi))"
    }
    return ToolParam(
        name: p.name,
        type: p.type ?? "any",
        required: p.required ?? false,
        detail: detail,
        description: p.description ?? ""
    )
}

/// Read the catalog from the FFI and decode it into typed `ToolInfo`s. The engine hands
/// the params as an ORDERED array (the authored order), so we render them as-is — no
/// sort, no inference from an unordered JSON-Schema `properties` object.
private func loadTools() -> [ToolInfo] {
    guard let ptr = ds_tools_json() else { return [] }
    defer { ds_string_free(ptr) }
    guard let data = String(cString: ptr).data(using: .utf8),
          let dtos = try? JSONDecoder().decode([ToolDTO].self, from: data)
    else { return [] }
    return dtos.map { t in
        ToolInfo(name: t.name, summary: t.description ?? "", params: (t.params ?? []).map(toToolParam))
    }
}

struct ToolsView: View {
    @State private var tools: [ToolInfo] = []
    /// Title-bar height (system-derived, no hardcoded constant) — sizes the frosted top strip
    /// that content blurs under. Mirrors the Status window's derivation.
    private var titleBarHeight: CGFloat {
        NSWindow.frameRect(forContentRect: .zero, styleMask: [.titled]).height
    }
    /// Names of the tools currently expanded (collapsed by default) — same disclosure idea
    /// as the Status window, but each row shows a plain rotating chevron rather than the
    /// status dot↔chevron crossfade.
    @State private var expanded: Set<String> = []

    var body: some View {
        toolList
            // Flexible on both axes — opens at 520×640, floors at 420×320, grows to fill
            // the window. A normal resizable window with an internal ScrollView (see
            // `toolList`): expanding a tool scrolls inside, the window never auto-resizes.
            .frame(minWidth: 420, idealWidth: 520, maxWidth: .infinity,
                   minHeight: 320, idealHeight: 640, maxHeight: .infinity)
            // One continuous glass slab behind everything; the host window is itself clear.
            // The title-bar height gives the frosted top strip its size, so tool rows BLUR as
            // they scroll under the traffic-light strip (same as the Status window).
            .windowGlass(topHeight: titleBarHeight)
            .glassWindow()
            .closeOnlyWindow()
            .onAppear { tools = loadTools() }
    }

    /// The tool catalog as a Control-Center / HUD layout matching the Status window: one
    /// glass slab with the tools on a single headerless "platter". Split out of `body` so
    /// the type-checker handles the content + chrome modifiers as two smaller expressions.
    @ViewBuilder private var toolList: some View {
        // Authored catalog order (related tools sit together — see `ds_tools::TOOLS`);
        // the FFI preserves it, so render as-is rather than re-sorting. A ScrollView so
        // expanding a tool scrolls inside the window rather than resizing it.
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                Platter {
                    ForEach(Array(tools.enumerated()), id: \.element.id) { idx, tool in
                        if idx > 0 { PlatterDivider() }
                        toolRow(tool)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // Uniform inset on all edges — shared with the Status window via
            // `windowContentInset()` (no per-screen copy of the value).
            .windowContentInset()
        }
        .scrollIndicators(.hidden)
    }

    /// One collapsible tool: a tappable header (name + a rotating chevron on the right) that
    /// reveals the summary and arguments when expanded — the Status window's disclosure idea,
    /// with a plain chevron instead of the status dot.
    @ViewBuilder
    private func toolRow(_ tool: ToolInfo) -> some View {
        let isOpen = expanded.contains(tool.name)
        VStack(spacing: 0) {
            Button {
                withAnimation(.snappy(duration: 0.2)) {
                    if isOpen { expanded.remove(tool.name) } else { expanded.insert(tool.name) }
                }
            } label: {
                HStack(spacing: 8) {
                    Text(tool.name)
                        .font(.system(.body, design: .monospaced)).fontWeight(.semibold)
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.secondary)
                        .rotationEffect(.degrees(isOpen ? 90 : 0))
                }
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 14).padding(.vertical, 10)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if isOpen {
                PlatterDivider()
                toolDetail(tool)
            }
        }
    }

    /// The expanded body of a tool row: what it does, then its arguments.
    @ViewBuilder
    private func toolDetail(_ tool: ToolInfo) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(tool.summary)
                .font(.callout).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            if tool.params.isEmpty {
                Text(L.t("tools.no_arguments"))
                    .font(.caption).foregroundStyle(.tertiary)
            } else {
                Text(L.t("tools.arguments"))
                    .font(.caption2).fontWeight(.semibold)
                    .foregroundStyle(.tertiary).textCase(.uppercase)
                    .padding(.top, 2)
                ForEach(tool.params) { p in
                    paramRow(p)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14).padding(.vertical, 10)
    }

    @ViewBuilder
    private func paramRow(_ p: ToolParam) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 6) {
                Text(p.name).font(.system(.caption, design: .monospaced)).fontWeight(.medium)
                Text(p.type).font(.caption2).foregroundStyle(.secondary)
                Text(p.required ? L.t("tools.param.required") : L.t("tools.param.optional"))
                    .font(.caption2)
                    .foregroundStyle(p.required ? Color.orange : Color.secondary)
                if !p.detail.isEmpty {
                    Text(p.detail).font(.caption2).foregroundStyle(.secondary)
                }
            }
            if !p.description.isEmpty {
                Text(p.description)
                    .glassCaption()
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.leading, 10)
        .padding(.vertical, 1)
    }
}
