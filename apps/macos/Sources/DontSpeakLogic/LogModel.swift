//  LogModel.swift
//
//  Pure, dependency-free model for the Logs tab: the one log line shape plus the
//  source-ordering and filtering rules the view relies on. Split into this framework-free
//  target so the FILTER + per-source color assignment (the logic, not the rendering) is
//  unit-testable without linking the Rust FFI staticlib or any system framework. `Codable`
//  is the Swift standard library, so decoding the shape needs no `import Foundation`; the
//  UI does the actual `JSONDecoder` pass and the coloring.

/// One activity-log line, from the combined-log JSON (`ds_logs_json` → an array of
/// `{source, level, text}`). Missing fields default to empty so a partial line never drops.
public struct LogLine: Decodable, Equatable, Sendable {
    public let source: String
    public let level: String
    public let text: String

    public init(source: String, level: String, text: String) {
        self.source = source
        self.level = level
        self.text = text
    }

    private enum CodingKeys: String, CodingKey { case source, level, text }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        source = (try c.decodeIfPresent(String.self, forKey: .source)) ?? ""
        level = (try c.decodeIfPresent(String.self, forKey: .level)) ?? ""
        text = (try c.decodeIfPresent(String.self, forKey: .text)) ?? ""
    }
}

/// The pure rules the Logs view applies to a decoded `[LogLine]`: the stable per-source order
/// (which fixes each source's palette color) and the free-text filter. No Foundation — case
/// folding is `lowercased()` (Swift stdlib), matching the engine/UI's ASCII source/level tokens.
public enum LogCatalog {
    /// The distinct sources in FIRST-APPEARANCE order. The view colors each source by its index
    /// here (modulo the palette length), so the mapping is stable and identical on every platform
    /// reading the same lines.
    public static func distinctSources(_ lines: [LogLine]) -> [String] {
        var seen: Set<String> = []
        var ordered: [String] = []
        for l in lines where !seen.contains(l.source) {
            seen.insert(l.source)
            ordered.append(l.source)
        }
        return ordered
    }

    /// The palette index for `source` (its first-appearance position), or `nil` if it isn't in
    /// `orderedSources`. The caller takes this modulo the palette length.
    public static func colorIndex(for source: String, in orderedSources: [String]) -> Int? {
        orderedSources.firstIndex(of: source)
    }

    /// Lines matching `query` — a case-insensitive substring over the message, source, OR level
    /// (the same fields, same semantics as the Windows filter). A blank/whitespace query keeps
    /// every line.
    public static func filter(_ lines: [LogLine], query: String) -> [LogLine] {
        filterIndexed(lines, query: query).map(\.line)
    }

    /// As `filter`, but each surviving line keeps its index in the ORIGINAL array — a stable
    /// row identity for UI diffing. (Offsets into the *filtered* array renumber on every
    /// filter keystroke, so identical rows read as new to the differ.)
    public static func filterIndexed(
        _ lines: [LogLine], query: String
    ) -> [(index: Int, line: LogLine)] {
        let q = query.trimmingCharactersInWhitespace().lowercased()
        let all = lines.enumerated().map { (index: $0.offset, line: $0.element) }
        guard !q.isEmpty else { return all }
        return all.filter {
            $0.line.text.lowercased().contains(q)
                || $0.line.source.lowercased().contains(q)
                || $0.line.level.lowercased().contains(q)
        }
    }
}

private extension String {
    /// Trim leading/trailing ASCII whitespace without Foundation (keeps this target framework
    /// free). The filter box only ever holds spaces/tabs to trim.
    func trimmingCharactersInWhitespace() -> String {
        let ws: Set<Character> = [" ", "\t", "\n", "\r"]
        var chars = self[...]
        while let f = chars.first, ws.contains(f) { chars = chars.dropFirst() }
        while let l = chars.last, ws.contains(l) { chars = chars.dropLast() }
        return String(chars)
    }
}
