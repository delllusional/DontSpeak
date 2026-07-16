import XCTest

@testable import DontSpeakLogic

final class LogModelTests: XCTestCase {
    private func line(_ source: String, _ level: String, _ text: String) -> LogLine {
        LogLine(source: source, level: level, text: text)
    }

    // MARK: - Decoding

    func testDecodeFullLine() throws {
        let json = #"[{"source":"tts","level":"INFO","text":"spoke 3 words"}]"#
        let lines = try JSONDecoder().decode([LogLine].self, from: Data(json.utf8))
        XCTAssertEqual(lines, [line("tts", "INFO", "spoke 3 words")])
    }

    /// Missing fields → empty; partial line kept (never blank the whole tab).
    func testDecodeMissingFieldsDefaultToEmpty() throws {
        let json = #"[{"text":"orphan"},{"source":"caps"}]"#
        let lines = try JSONDecoder().decode([LogLine].self, from: Data(json.utf8))
        XCTAssertEqual(lines, [line("", "", "orphan"), line("caps", "", "")])
    }

    // MARK: - Source ordering (stable palette color)

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

    func testEmptyOrBlankQueryKeepsAll() {
        XCTAssertEqual(LogCatalog.filter(sample, query: ""), sample)
        XCTAssertEqual(LogCatalog.filter(sample, query: "   \t "), sample)
    }

    func testFilterMatchesMessageCaseInsensitively() {
        let r = LogCatalog.filter(sample, query: "BLOCKED")
        XCTAssertEqual(r, [sample[1]])
    }

    func testFilterMatchesSource() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "caps"), [sample[2]])
    }

    func testFilterMatchesLevel() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "error"), [sample[1]])
    }

    func testFilterTrimsQuery() {
        XCTAssertEqual(LogCatalog.filter(sample, query: "  stt  "), [sample[1]])
    }

    func testFilterNoMatchIsEmpty() {
        XCTAssertTrue(LogCatalog.filter(sample, query: "zzz").isEmpty)
    }

    /// Original array indices are stable UI row ids (not filtered offsets).
    func testFilterIndexedKeepsOriginalIndices() {
        let r = LogCatalog.filterIndexed(sample, query: "n")  // "seNtence" (0) + "loNg" (2)
        XCTAssertEqual(r.map(\.index), [0, 2])
        XCTAssertEqual(r.map(\.line), [sample[0], sample[2]])
    }
}
