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

    /// Unknown period tokens must not fail card decode (m2 / Windows+Linux parity).
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

    func testRejectsMalformedDeck() {
        let json = #"{"providers":[]}"#
        XCTAssertNil(UsageDeck.decodeDeck(Data(json.utf8)))
    }
}
