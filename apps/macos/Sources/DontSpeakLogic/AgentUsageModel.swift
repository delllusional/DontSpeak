import Foundation

/// One gauge inside a card. Mirrors Rust `UsageRow`.
///
/// `period` is an opaque wire token (`session` / `week` / `month` / …) so a new
/// Rust period does not fail the whole card decode (parity with Windows/Linux).
/// `remainingLabel` is not on the wire — hosts fill it at the data-source boundary via
/// `ds_usage_resets_in` so SwiftUI `body` never calls FFI on every recompute.
public struct UsageRow: Decodable, Equatable, Sendable, Identifiable {
    public var id: String { period }
    public let period: String
    public let usedPercent: Double
    public let resetsAtUnix: Int64
    /// Preformatted remaining duration; empty until filled at the data boundary.
    public let remainingLabel: String

    enum CodingKeys: String, CodingKey {
        case period
        case usedPercent = "used_percent"
        case resetsAtUnix = "resets_at_unix"
    }

    public init(
        period: String,
        usedPercent: Double,
        resetsAtUnix: Int64,
        remainingLabel: String = ""
    ) {
        self.period = period
        self.usedPercent = usedPercent
        self.resetsAtUnix = resetsAtUnix
        self.remainingLabel = remainingLabel
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        period = try c.decode(String.self, forKey: .period)
        usedPercent = try c.decode(Double.self, forKey: .usedPercent)
        resetsAtUnix = try c.decode(Int64.self, forKey: .resetsAtUnix)
        remainingLabel = ""
    }

    public func withRemainingLabel(_ label: String) -> UsageRow {
        UsageRow(
            period: period,
            usedPercent: usedPercent,
            resetsAtUnix: resetsAtUnix,
            remainingLabel: label
        )
    }
}

/// Backing model for one Usage card: agent type + rows. Mirrors Rust `UsageCard`.
public struct UsageCard: Decodable, Equatable, Sendable, Identifiable {
    public var id: String { agent }
    public let agent: String
    /// Signed-in account (usually email) when the client exposes one; nil/empty otherwise.
    public let account: String?
    public let rows: [UsageRow]

    enum CodingKeys: String, CodingKey {
        case agent, account, rows
    }

    public init(agent: String, rows: [UsageRow], account: String? = nil) {
        self.agent = agent
        self.account = account
        self.rows = rows
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        agent = try c.decode(String.self, forKey: .agent)
        account = try c.decodeIfPresent(String.self, forKey: .account)
        rows = try c.decode([UsageRow].self, forKey: .rows)
    }

    public func withRows(_ rows: [UsageRow]) -> UsageCard {
        UsageCard(agent: agent, rows: rows, account: account)
    }
}

/// Tab deck: ordered cards. Mirrors Rust `UsageDeck`.
public struct UsageDeck: Decodable, Equatable, Sendable {
    public let cards: [UsageCard]

    public init(cards: [UsageCard]) {
        self.cards = cards
    }

    public static func decodeDeck(_ data: Data) -> UsageDeck? {
        try? JSONDecoder().decode(Self.self, from: data)
    }

    public static func decodeCard(_ data: Data) -> UsageCard? {
        try? JSONDecoder().decode(UsageCard.self, from: data)
    }
}
