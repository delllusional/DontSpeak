// MCP tools reference from `ds_tools_json` (shared ds-tools catalog — same list Claude sees).

import CDontSpeak
import SwiftUI

struct ToolParam: Identifiable, Sendable {
    let name: String
    let type: String
    let required: Bool
    let detail: String  // "one of: a, b" / "0.5–2.0" / ""
    let description: String
    var id: String { name }
}

struct ToolInfo: Identifiable, Sendable {
    let name: String
    let summary: String
    let params: [ToolParam]
    var id: String { name }
}

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
    /// Pre-built by `status_fmt::tool_param_detail` — no host-side derivation.
    let detail: String?
}

private func toToolParam(_ p: ParamDTO) -> ToolParam {
    ToolParam(
        name: p.name,
        type: p.type ?? L.t("tools.param.type_any"),
        required: p.required ?? false,
        detail: p.detail ?? "",
        description: p.description ?? ""
    )
}

/// Params stay in authored order (ordered array, not JSON-Schema `properties`).
private func loadTools() -> [ToolInfo] {
    guard let dtos = ffiDecode([ToolDTO].self, ds_tools_json) else { return [] }
    return dtos.map { t in
        ToolInfo(name: t.name, summary: t.description ?? "", params: (t.params ?? []).map(toToolParam))
    }
}

struct ToolsView: View {
    @State private var tools: [ToolInfo] = []
    @State private var expanded: Set<String> = []

    var body: some View {
        toolList
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // Catalog is process-immutable; re-navigating re-fires onAppear — load once.
            .onAppear { if tools.isEmpty { tools = loadTools() } }
    }

    @ViewBuilder private var toolList: some View {
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
            .windowContentInset()
        }
        .scrollIndicators(.hidden)
    }

    @ViewBuilder
    private func toolRow(_ tool: ToolInfo) -> some View {
        DisclosureRow(expanded: $expanded, id: tool.name) {
            Text(tool.name)
                .font(.system(.body, design: .monospaced)).fontWeight(.semibold)
        } content: {
            toolDetail(tool)
        }
    }

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
