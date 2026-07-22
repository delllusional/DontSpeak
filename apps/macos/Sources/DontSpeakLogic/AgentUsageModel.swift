import Foundation

/// One gauge (Rust UsageRow). `period` is an opaque wire token so unknown periods still decode.
/// `remainingLabel` filled at data-source via ds_usage_resets_in.
public struct UsageRow: Decodable, Equatable, Sendable, Identifiable {
    public var id: String { period }
    public let period: String
    public let usedPercent: Double
    public let resetsAtUnix: Int64
    /// Preformatted remaining; empty until data boundary.
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

    public func hasSameWireValue(as other: UsageRow) -> Bool {
        period == other.period
            && usedPercent == other.usedPercent
            && resetsAtUnix == other.resetsAtUnix
    }
}

/// `needsAuth` marks guarded macOS keychain access.
public struct UsageCard: Decodable, Equatable, Sendable, Identifiable {
    public var id: String { agent }
    public let agent: String
    public let account: String?
    public let rows: [UsageRow]
    public let needsAuth: Bool

    enum CodingKeys: String, CodingKey {
        case agent, account, rows
        case needsAuth = "needs_auth"
    }

    public init(agent: String, rows: [UsageRow], account: String? = nil, needsAuth: Bool = false) {
        self.agent = agent
        self.account = account
        self.rows = rows
        self.needsAuth = needsAuth
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        agent = try c.decode(String.self, forKey: .agent)
        account = try c.decodeIfPresent(String.self, forKey: .account)
        rows = try c.decode([UsageRow].self, forKey: .rows)
        needsAuth = try c.decodeIfPresent(Bool.self, forKey: .needsAuth) ?? false
    }

    public func withRows(_ rows: [UsageRow]) -> UsageCard {
        UsageCard(agent: agent, rows: rows, account: account, needsAuth: needsAuth)
    }

    public func hasSameWireValue(as other: UsageCard) -> Bool {
        agent == other.agent
            && account == other.account
            && needsAuth == other.needsAuth
            && rows.count == other.rows.count
            && zip(rows, other.rows).allSatisfy { pair in
                pair.0.hasSameWireValue(as: pair.1)
            }
    }
}

/// Agents-tab paint rules; the WinUI/GTK ports mirror them inline.
public enum UsagePaint {
    /// A refresh result replaces the painted card when it carries rows or an auth
    /// prompt — or when the painted card has nothing to lose (materialized by speech).
    public static func replaces(painted: UsageCard?, with updated: UsageCard) -> Bool {
        if !updated.rows.isEmpty || updated.needsAuth { return true }
        guard let painted, painted.agent == updated.agent else { return false }
        return painted.rows.isEmpty && !painted.needsAuth
    }

    /// Spoken agents still owed a card, in canonical order: installed and unpainted.
    public static func materializable(
        spoken: Set<String>,
        canonical: [String],
        painted: [String]
    ) -> [String] {
        let shown = Set(painted)
        return canonical.filter { spoken.contains($0) && !shown.contains($0) }
    }
}

/// Tab deck (Rust UsageDeck).
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
