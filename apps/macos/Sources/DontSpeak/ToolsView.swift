//  ToolsView.swift
//
//  A read-only reference of the DontSpeak tools available to Claude over MCP. The
//  list is the SAME catalog the MCP server gives Claude — read via the FFI
//  (ds_tools_json → the shared ds-tools crate), so it never drifts. Each
//  tool shows what it does and the arguments it accepts (name, type, required,
//  allowed values), read from the catalog's ORDERED `params` array (authored order).

import CDontSpeak
import SwiftUI

/// One argument of a tool, from the catalog's ordered `params` array.
struct ToolParam: Identifiable, Sendable {
    let name: String
    let type: String
    let required: Bool
    let detail: String  // "one of: a, b" / "0.5–2.0" / ""
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
    /// The localized constraint qualifier (enum "one of: …" / numeric "lo–hi" / ""),
    /// pre-built by the shared `status_fmt::tool_param_detail` — no host-side derivation.
    let detail: String?
}

/// Map one decoded param to its view model. The `detail` qualifier is already built and
/// localized by the engine (`ds_tools_json`), so the host just carries it through.
private func toToolParam(_ p: ParamDTO) -> ToolParam {
    ToolParam(
        name: p.name,
        type: p.type ?? "any",
        required: p.required ?? false,
        detail: p.detail ?? "",
        description: p.description ?? ""
    )
}

/// Read the catalog from the FFI and decode it into typed `ToolInfo`s. The engine hands
/// the params as an ORDERED array (the authored order), so we render them as-is — no
/// sort, no inference from an unordered JSON-Schema `properties` object.
private func loadTools() -> [ToolInfo] {
    guard let dtos = ffiDecode([ToolDTO].self, ds_tools_json) else { return [] }
    return dtos.map { t in
        ToolInfo(name: t.name, summary: t.description ?? "", params: (t.params ?? []).map(toToolParam))
    }
}

struct ToolsView: View {
    @State private var tools: [ToolInfo] = []
    /// Names of the tools currently expanded (collapsed by default) — same disclosure idea
    /// as the Status window, but each row shows a plain rotating chevron rather than the
    /// status dot↔chevron crossfade.
    @State private var expanded: Set<String> = []

    var body: some View {
        // The Tools pane of the merged sidebar window — just the scrollable content; the glass
        // slab + traffic-light strip live once on the `MainWindow` container.
        toolList
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The catalog is immutable for the process lifetime, so load it ONCE — re-navigating
            // to this tab re-fires `onAppear` but must not re-run the FFI + JSON decode.
            .onAppear { if tools.isEmpty { tools = loadTools() } }
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
    /// reveals the summary and arguments when expanded — the shared `DisclosureRow`, the same
    /// disclosure look the Libraries pane uses.
    @ViewBuilder
    private func toolRow(_ tool: ToolInfo) -> some View {
        DisclosureRow(expanded: $expanded, id: tool.name) {
            Text(tool.name)
                .font(.system(.body, design: .monospaced)).fontWeight(.semibold)
        } content: {
            toolDetail(tool)
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
