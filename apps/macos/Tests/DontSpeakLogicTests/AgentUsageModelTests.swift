import Foundation
import XCTest

@testable import DontSpeakLogic

final class AgentUsageModelTests: XCTestCase {
    func testDecodesDeckAndCard() throws {
        let deckJson = #"{"cards":[{"agent":"claude_code","account":"me@anthropic.test","rows":[{"period":"session","used_percent":20.0,"resets_at_unix":1800000000},{"period":"week","used_percent":40.0,"resets_at_unix":1800100000}]}]}"#
        let deck = try XCTUnwrap(UsageDeck.decodeDeck(Data(deckJson.utf8)))
        XCTAssertEqual(deck.cards.count, 1)
        XCTAssertEqual(deck.cards[0].agent, "claude_code")
        XCTAssertEqual(deck.cards[0].account, "me@anthropic.test")
        XCTAssertEqual(deck.cards[0].rows.map(\.period), ["session", "week"])
        XCTAssertEqual(deck.cards[0].rows.map(\.remainingLabel), ["", ""])

        let cardJson = #"{"agent":"grok","rows":[{"period":"week","used_percent":8,"resets_at_unix":1801000000}]}"#
        let card = try XCTUnwrap(UsageDeck.decodeCard(Data(cardJson.utf8)))
        XCTAssertEqual(card.agent, "grok")
        XCTAssertNil(card.account)
        XCTAssertEqual(card.rows.first?.usedPercent, 8)
        XCTAssertEqual(card.rows.first?.remainingLabel, "")
    }

    /// Unknown period tokens must not fail card decode (peer parity).
    func testDecodesUnknownPeriodAsOpaqueString() throws {
        let cardJson = #"{"agent":"claude_code","rows":[{"period":"daily","used_percent":15,"resets_at_unix":1800000000}]}"#
        let card = try XCTUnwrap(UsageDeck.decodeCard(Data(cardJson.utf8)))
        XCTAssertEqual(card.rows.first?.period, "daily")
        XCTAssertEqual(card.rows.first?.id, "daily")
    }

    func testWithRemainingLabel() {
        let row = UsageRow(period: "week", usedPercent: 10, resetsAtUnix: 1)
        XCTAssertEqual(row.remainingLabel, "")
        let filled = row.withRemainingLabel("2d 05h")
        XCTAssertEqual(filled.remainingLabel, "2d 05h")
        XCTAssertEqual(filled.period, "week")
        XCTAssertEqual(filled.resetsAtUnix, 1)
    }

    func testWireEqualityIgnoresPresentationLabel() {
        let first = UsageCard(
            agent: "codex",
            rows: [UsageRow(period: "week", usedPercent: 10, resetsAtUnix: 1, remainingLabel: "2d")]
        )
        let second = UsageCard(
            agent: "codex",
            rows: [UsageRow(period: "week", usedPercent: 10, resetsAtUnix: 1, remainingLabel: "1d 23h")]
        )
        XCTAssertTrue(first.hasSameWireValue(as: second))
        XCTAssertFalse(first.hasSameWireValue(as: UsageCard(
            agent: "codex",
            rows: [UsageRow(period: "week", usedPercent: 11, resetsAtUnix: 1)]
        )))
    }

    func testRejectsMalformedDeck() {
        let json = #"{"providers":[]}"#
        XCTAssertNil(UsageDeck.decodeDeck(Data(json.utf8)))
    }

    /// needs_auth defaults false; present only when true.
    func testDecodesNeedsAuthDefaultingFalse() throws {
        let legacy = #"{"agent":"claude_code","rows":[]}"#
        let card = try XCTUnwrap(UsageDeck.decodeCard(Data(legacy.utf8)))
        XCTAssertFalse(card.needsAuth)

        let guarded = #"{"agent":"claude_code","rows":[],"needs_auth":true}"#
        let guardedCard = try XCTUnwrap(UsageDeck.decodeCard(Data(guarded.utf8)))
        XCTAssertTrue(guardedCard.needsAuth)
        XCTAssertTrue(guardedCard.withRows([]).needsAuth)
    }

    /// Equality includes needsAuth so auth transitions repaint.
    func testWireEqualityDistinguishesNeedsAuth() {
        let plain = UsageCard(agent: "claude_code", rows: [])
        let guarded = UsageCard(agent: "claude_code", rows: [], needsAuth: true)
        XCTAssertFalse(plain.hasSameWireValue(as: guarded))
        XCTAssertTrue(guarded.hasSameWireValue(as: guarded))
    }

    private func card(_ agent: String, rows: Int = 0, needsAuth: Bool = false) -> UsageCard {
        UsageCard(
            agent: agent,
            rows: (0..<rows).map { UsageRow(period: "week", usedPercent: Double($0), resetsAtUnix: 1) },
            needsAuth: needsAuth
        )
    }

    /// An empty refresh refreshes a statless card but never blanks a card with rows.
    func testEmptyRefreshOnlyReplacesAStatlessCard() {
        let statless = card("qwen_code")
        let withRows = card("qwen_code", rows: 1)
        XCTAssertTrue(UsagePaint.replaces(painted: statless, with: card("qwen_code")))
        XCTAssertFalse(UsagePaint.replaces(painted: withRows, with: card("qwen_code")))
        XCTAssertTrue(UsagePaint.replaces(painted: withRows, with: card("qwen_code", rows: 2)))
        XCTAssertTrue(
            UsagePaint.replaces(painted: withRows, with: card("qwen_code", needsAuth: true)))
        // An auth prompt is data too: it must not be overwritten by an empty result.
        XCTAssertFalse(
            UsagePaint.replaces(painted: card("qwen_code", needsAuth: true), with: card("qwen_code")))
    }

    /// Unpainted agents stay unpainted on an empty result — speech is what materializes them.
    func testEmptyRefreshDoesNotCreateACard() {
        XCTAssertFalse(UsagePaint.replaces(painted: nil, with: card("qwen_code")))
        XCTAssertTrue(UsagePaint.replaces(painted: nil, with: card("qwen_code", rows: 1)))
    }

    func testMaterializableIsCanonicalOrderedInstalledAndIdempotent() {
        let canonical = ["claude_code", "codex", "qwen_code"]
        XCTAssertEqual(
            UsagePaint.materializable(
                spoken: ["qwen_code", "claude_code", "grok"],
                canonical: canonical,
                painted: []
            ),
            ["claude_code", "qwen_code"]
        )
        // Already painted → nothing owed; unknown agents never materialize.
        XCTAssertEqual(
            UsagePaint.materializable(
                spoken: ["qwen_code", "grok"],
                canonical: canonical,
                painted: ["qwen_code"]
            ),
            []
        )
    }
}
