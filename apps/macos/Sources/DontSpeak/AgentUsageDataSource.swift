import CDontSpeak
import DontSpeakLogic
import Foundation

/// Usage C ABI adapter. Decode at boundary; attach `remainingLabel` via ds_usage_resets_in.
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

    /// Blocking authorize + force load. Explicit click only; may ACL-prompt.
    static func authorizeCard(_ agent: String) async -> UsageCard? {
        await Task.detached(priority: .userInitiated) {
            guard let json = ffiString({
                agent.withCString { ds_agent_usage_card_authorize_json($0) }
            }) else { return nil }
            guard let card = UsageDeck.decodeCard(Data(json.utf8)) else { return nil }
            return attachRemaining(card)
        }.value
    }

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
