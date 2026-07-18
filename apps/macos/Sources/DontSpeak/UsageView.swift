import DontSpeakLogic
import Foundation
import SwiftUI

/// Usage tab — card-backed model shared with WinUI/GTK.
///
/// On tab select:
/// 1. Paint only cards that already have cached rows
/// 2. Async load every installed agent; insert/update a card when that load has rows
///
/// First visit with no cache: list stays empty until at least one agent returns data.
struct UsageView: View {
    @Environment(Core.self) private var core
    @State private var cards: [UsageCard] = []
    /// Rust skeleton order (ClientSource::CLIENTS).\n    @State private var canonicalAgents: [String] = []
    /// Skeleton + all per-agent loads finished for this generation.\n    @State private var settled = false
    @State private var generation = 0

    var body: some View {
        Group {
            if cards.isEmpty {
                if settled {
                    Text(L.t("usage.unavailable"))
                        .foregroundStyle(.secondary)
                } else {
                    // Color.clear keeps onAppear alive (EmptyView never appears).\n                    Color.clear
                }
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(cards) { card in
                            UsageCardView(
                                card: card,
                                speaking: core.activity.ttsSource == card.agent
                            )
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .windowContentInset()
                }
                .scrollIndicators(.hidden)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear {
            Task { await onTabSelected() }
        }
    }

    @MainActor private func onTabSelected() async {
        generation += 1
        let gen = generation
        settled = false

        // 1) Skeleton: installed agents + last-good cache, already decoded by the adapter.
        let deck = await AgentUsageDataSource.readCachedDeck()
        guard gen == generation else { return }
        guard let deck else {
            settled = cards.isEmpty
            return
        }

        let allAgents = deck.cards.map(\.agent)
        canonicalAgents = allAgents
        let installed = Set(allAgents)
        cards.removeAll { !installed.contains($0.agent) }
        for cached in deck.cards where !cached.rows.isEmpty {
            applyCard(cached, animated: false)
        }

        if allAgents.isEmpty {
            settled = true
            return
        }

        // 2) Force-load each agent independently; UI updates as each finishes.
        // Task inherits @MainActor isolation from onTabSelected — no MainActor.run hop.
        let pending = PendingCount(allAgents.count)
        for agent in allAgents {
            Task {
                let updated = await AgentUsageDataSource.refreshCard(agent)
                guard gen == generation else { return }
                if let updated, !updated.rows.isEmpty {
                    applyCard(updated)
                }
                if pending.decrement() {
                    settled = true
                }
            }
        }
    }

    @MainActor private func applyCard(_ updated: UsageCard, animated: Bool = true) {
        if let idx = cards.firstIndex(where: { $0.agent == updated.agent }) {
            guard cards[idx] != updated else { return }
            if animated {
                withAnimation(.easeInOut(duration: 0.2)) { cards[idx] = updated }
            } else {
                cards[idx] = updated
            }
        } else {
            // Rust owns agent identity and order; the skeleton deck carries both.
            let insert = {
                cards.append(updated)
                cards.sort { lhs, rhs in
                    agentRank(lhs.agent) < agentRank(rhs.agent)
                }
            }
            if animated {
                withAnimation(.easeInOut(duration: 0.2), insert)
            } else {
                insert()
            }
        }
    }

    private func agentRank(_ agent: String) -> Int {
        canonicalAgents.firstIndex(of: agent) ?? canonicalAgents.count
    }

}

/// Thread-safe countdown for settling the empty/unavailable state.
private final class PendingCount: @unchecked Sendable {
    private var value: Int
    private let lock = NSLock()
    init(_ value: Int) { self.value = value }
    /// Returns true when count reaches zero.
    func decrement() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        value -= 1
        return value <= 0
    }
}

private struct UsageCardView: View {
    let card: UsageCard
    /// In-flight TTS matches this agent — brand-purple wash (parity with top-bar speaking tint).
    var speaking: Bool = false
    /// Session-only reveal; resets when the view is recreated (tab reload / process restart).
    @State private var accountRevealed = false

    private var accountLabel: String? {
        guard let account = card.account?.trimmingCharacters(in: .whitespacesAndNewlines),
              !account.isEmpty
        else { return nil }
        return account
    }

    var body: some View {
        // Custom header: provider left / account right (same caption secondary as remaining).
        // Email is fully transparent until tapped; reveal is not persisted.
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .lastTextBaseline) {
                Text(providerTitle(card.agent))
                    .glassSectionHeader()
                Spacer(minLength: 8)
                if let accountLabel {
                    Text(accountLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .opacity(accountRevealed ? 1 : 0)
                        .contentShape(Rectangle())
                        .onTapGesture { accountRevealed.toggle() }
                        .accessibilityLabel(accountRevealed ? accountLabel : "Account")
                        .accessibilityAddTraits(.isButton)
                }
            }
            .padding(.leading, 6)
            VStack(spacing: 0) {
                ForEach(Array(card.rows.enumerated()), id: \.offset) { index, row in
                    if index > 0 { PlatterDivider() }
                    UsageRowView(row: row)
                }
            }
            .platterBackground(cornerRadius: Glass.platterCorner)
        }
        .overlay {
            if speaking {
                RoundedRectangle(cornerRadius: Glass.platterCorner, style: .continuous)
                    .fill(Color.smSeedPurple.opacity(0.30))
                    .allowsHitTesting(false)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: speaking)
    }
}

/// Localized provider title, or a prettified agent token when the catalog has no entry.
private func providerTitle(_ agent: String) -> String {
    let key = "usage.provider.\(agent)"
    let localized = L.t(key)
    if localized != key { return localized }
    return agent
        .split(separator: "_")
        .map { part in
            guard let first = part.first else { return "" }
            return String(first).uppercased() + part.dropFirst()
        }
        .joined(separator: " ")
}

private struct UsageRowView: View {
    let row: UsageRow

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            // Bottom-align period + remaining so different font sizes share one baseline.
            HStack(alignment: .lastTextBaseline) {
                Text(L.t("usage.\(row.period)")).glassRowTitle()
                Spacer(minLength: 8)
                // Remaining till reset (minute-granularity) sits top-right; percent is the bar only.
                if !row.remainingLabel.isEmpty {
                    Text(row.remainingLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
            }
            ProgressView(value: row.usedPercent, total: 100)
                .tint(Color.smSeedPurple)
        }
        .platterRow()
    }
}
