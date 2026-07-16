/// One activity-log line from `ds_logs_json` (`{source, level, text}`).
/// Missing fields default to empty so a partial line never drops.
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

/// Logs-tab pure rules: stable first-appearance source order (palette index) and free-text
/// filter. No Foundation — `lowercased()` matches the engine/UI's ASCII tokens.
public enum LogCatalog {
    /// Distinct sources in first-appearance order; view colors by index mod palette length.
    public static func distinctSources(_ lines: [LogLine]) -> [String] {
        var seen: Set<String> = []
        var ordered: [String] = []
        for l in lines where !seen.contains(l.source) {
            seen.insert(l.source)
            ordered.append(l.source)
        }
        return ordered
    }

    /// Palette index for `source`, or `nil` if absent from `orderedSources`.
    public static func colorIndex(for source: String, in orderedSources: [String]) -> Int? {
        orderedSources.firstIndex(of: source)
    }

    /// Case-insensitive substring over message, source, OR level (Windows filter semantics).
    /// Blank/whitespace query keeps every line.
    public static func filter(_ lines: [LogLine], query: String) -> [LogLine] {
        filterIndexed(lines, query: query).map(\.line)
    }

    /// Like `filter`, but keeps each line's ORIGINAL array index as the stable UI row id
    /// (filtered offsets renumber on every keystroke and look like new rows to the differ).
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
    /// ASCII whitespace trim without Foundation (keeps this target framework-free).
    func trimmingCharactersInWhitespace() -> String {
        let ws: Set<Character> = [" ", "\t", "\n", "\r"]
        var chars = self[...]
        while let f = chars.first, ws.contains(f) { chars = chars.dropFirst() }
        while let l = chars.last, ws.contains(l) { chars = chars.dropLast() }
        return String(chars)
    }
}
