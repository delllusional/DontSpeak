import XCTest

@testable import DontSpeakLogic

final class LogModelTests: XCTestCase {
    private func line(_ source: String, _ level: String, _ text: String) -> LogLine {
        LogLine(source: source, level: level, text: text)
    }

    // MARK: - Decoding (the wire shape ds_logs_json returns)

    /// A well-formed line decodes its three fields.
    func testDecodeFullLine() throws {
        let json = #"[{"source":"tts","level":"INFO","text":"spoke 3 words"}]"#
        let lines = try JSONDecoder().decode([LogLine].self, from: Data(json.utf8))
        XCTAssertEqual(lines, [line("tts", "INFO", "spoke 3 words")])
    }

    /// Missing fields default to empty so a partial line is kept (never dropped or a decode
    /// failure that would blank the whole tab).
    func testDecodeMissingFieldsDefaultToEmpty() throws {
        let json = #"[{"text":"orphan"},{"source":"caps"}]"#
        let lines = try JSONDecoder().decode([LogLine].self, from: Data(json.utf8))
        XCTAssertEqual(lines, [line("", "", "orphan"), line("caps", "", "")])
    }

    // MARK: - Source ordering (fixes each source's stable palette color)

    /// Distinct sources are returned in FIRST-APPEARANCE order, deduplicated.
    func testDistinctSourcesPreserveFirstAppearanceOrder() {
        let lines = [
            line("engine", "INFO", "a"),
            line("tts", "INFO", "b"),
            line("engine", "WARN", "c"),
            line("caps", "INFO", "d"),
            line("tts", "INFO", "e"),
        ]
        XCTAssertEqual(LogCatalog.distinctSources(lines), ["engine", "tts", "caps"])
    }

    /// The color index is the source's first-appearance position; unknown sources are nil.
    func testColorIndex() {
        let ordered = ["engine", "tts", "caps"]
        XCTAssertEqual(LogCatalog.colorIndex(for: "engine", in: ordered), 0)
        XCTAssertEqual(LogCatalog.colorIndex(for: "caps", in: ordered), 2)
        XCTAssertNil(LogCatalog.colorIndex(for: "mcp", in: ordered))
    }

    // MARK: - Filtering

    private let sample = [
        LogLine(source: "tts", level: "INFO", text: "spoke a sentence"),
        LogLine(source: "stt", level: "ERROR", text: "mic blocked"),
        LogLine(source: "caps", level: "WARN", text: "held too long"),
    ]

    /// A blank or whitespace-only query keeps every line.
    func testEmptyOrBlankQueryKeepsAll() {
        XCTAssertEqual(LogCatalog.filter(sample, query: ""), sample)
        XCTAssertEqual(LogCatalog.filter(sample, query: "   \t "), sample)
    }

    /// The query is a case-insensitive substring over the MESSAGE.
    func testFilterMatchesMessageCaseInsensitively() {
        let r = LogCatalog.filter(sample, query: "BLOCKED")
        XCTAssertEqual(r, [sample[1]])
    }

    /// …over the SOURCE…
    func testFilterMatchesSource() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "caps"), [sample[2]])
    }

    /// …and over the LEVEL.
    func testFilterMatchesLevel() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "error"), [sample[1]])
    }

    /// Surrounding whitespace is trimmed before matching (so a stray trailing space still hits).
    func testFilterTrimsQuery() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "  stt  "), [sample[1]])
    }

    /// A query that matches nothing yields an empty result (the view shows "no_match").
    func testFilterNoMatchIsEmpty() {
        XCTAssertTrue(LogCatalog.filter(sample, query: "zzz").isEmpty)
    }

    /// Surviving lines keep their index in the ORIGINAL array (the stable row identity the
    /// UI diffs by), not their position in the filtered result.
    func testFilterIndexedKeepsOriginalIndices() {
        let r = LogCatalog.filterIndexed(sample, query: "n")  // "seNtence" (0) + "loNg" (2)
        XCTAssertEqual(r.map(\.index), [0, 2])
        XCTAssertEqual(r.map(\.line), [sample[0], sample[2]])
    }
}
