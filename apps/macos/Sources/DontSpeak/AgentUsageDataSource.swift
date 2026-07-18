import CDontSpeak
import DontSpeakLogic
import Foundation

/// Lowest-level typed adapter for the Usage C ABI. JSON is decoded immediately after the
/// blocking call so view code receives only `UsageDeck` and `UsageCard` domain values.
///
/// Remaining duration labels are attached here (not in SwiftUI `body`) so render churn
/// never drives `ds_usage_resets_in` FFI calls.
enum AgentUsageDataSource {
    static func readCachedDeck() async -> UsageDeck? {
        await Task.detached(priority: .userInitiated) {
            guard let json = ffiString({ ds_agent_usage_skeleton_json() }) else { return nil }
            guard let deck = UsageDeck.decodeDeck(Data(json.utf8)) else { return nil }
            return attachRemaining(deck)
        }.value
    }

    static func refreshCard(_ agent: String) async -> UsageCard? {
        await Task.detached(priority: .userInitiated) {
            guard let json = ffiString({
                agent.withCString { ds_agent_usage_card_json($0, 1) }
            }) else { return nil }
            guard let card = UsageDeck.decodeCard(Data(json.utf8)) else { return nil }
            return attachRemaining(card)
        }.value
    }

    /// Fill `remainingLabel` once per row at the FFI boundary.
    private static func attachRemaining(_ deck: UsageDeck) -> UsageDeck {
        UsageDeck(cards: deck.cards.map(attachRemaining))
    }

    private static func attachRemaining(_ card: UsageCard) -> UsageCard {
        card.withRows(card.rows.map { row in
            let label = ffiString({ ds_usage_resets_in(row.resetsAtUnix) }) ?? ""
            return row.withRemainingLabel(label)
        })
    }
}
