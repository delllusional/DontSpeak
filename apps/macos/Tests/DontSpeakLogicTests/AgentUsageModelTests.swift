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

    /// Wire key is absent when false (legacy decks) and true only when guarded.
    func testDecodesNeedsAuthDefaultingFalse() throws {
        let legacy = #"{"agent":"claude_code","rows":[]}"#
        let card = try XCTUnwrap(UsageDeck.decodeCard(Data(legacy.utf8)))
        XCTAssertFalse(card.needsAuth)

        let guarded = #"{"agent":"claude_code","rows":[],"needs_auth":true}"#
        let guardedCard = try XCTUnwrap(UsageDeck.decodeCard(Data(guarded.utf8)))
        XCTAssertTrue(guardedCard.needsAuth)
        XCTAssertTrue(guardedCard.withRows([]).needsAuth)
    }

    /// Auth-state transitions must repaint: equality includes needsAuth.
    func testWireEqualityDistinguishesNeedsAuth() {
        let plain = UsageCard(agent: "claude_code", rows: [])
        let guarded = UsageCard(agent: "claude_code", rows: [], needsAuth: true)
        XCTAssertFalse(plain.hasSameWireValue(as: guarded))
        XCTAssertTrue(guarded.hasSameWireValue(as: guarded))
    }
}
