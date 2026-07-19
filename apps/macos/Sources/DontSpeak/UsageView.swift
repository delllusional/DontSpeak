import DontSpeakLogic
import Foundation
import SwiftUI

/// Agents tab — card model shared with WinUI/GTK.
/// Tab select: paint cached rows, then async force-load each installed agent.
/// No cache: empty until at least one agent returns data.
struct UsageView: View {
    @Environment(Core.self) private var core
    @State private var cards: [UsageCard] = []
    /// ClientSource::CLIENTS order from the skeleton deck.
    @State private var canonicalAgents: [String] = []
    @State private var settled = false
    @State private var generation = 0

    var body: some View {
        Group {
            if cards.isEmpty {
                if settled {
                    Text(L.t("usage.unavailable"))
                        .foregroundStyle(.secondary)
                } else {
                    // Color.clear keeps onAppear alive (EmptyView never appears).
                    Color.clear
                }
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(cards) { card in
                            UsageCardView(
                                card: card,
                                speaking: core.activity.speakingSource == card.agent,
                                onAuthorize: authorize
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
        .task { await onTabSelected() }
    }

    @MainActor private func onTabSelected() async {
        generation += 1
        let gen = generation
        settled = false

        let deck = await AgentUsageDataSource.readCachedDeck()
        guard !Task.isCancelled, gen == generation else { return }
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

        await withTaskGroup(of: UsageCard?.self) { group in
            for agent in allAgents {
                group.addTask { await AgentUsageDataSource.refreshCard(agent) }
            }
            for await updated in group {
                guard !Task.isCancelled, gen == generation else {
                    group.cancelAll()
                    return
                }
                if let updated, !updated.rows.isEmpty || updated.needsAuth {
                    applyCard(updated)
                }
            }
        }
        guard !Task.isCancelled, gen == generation else { return }
        settled = true
    }

    /// User-click authorize: blocking FFI off the main actor, then the same
    /// generation-checked apply as a refresh.
    @MainActor private func authorize(_ agent: String) async {
        let gen = generation
        let updated = await AgentUsageDataSource.authorizeCard(agent)
        guard gen == generation, let updated else { return }
        if !updated.rows.isEmpty || updated.needsAuth {
            applyCard(updated)
        }
    }

    @MainActor private func applyCard(_ updated: UsageCard, animated: Bool = true) {
        if let idx = cards.firstIndex(where: { $0.agent == updated.agent }) {
            guard cards[idx] != updated else { return }
            if cards[idx].hasSameWireValue(as: updated) {
                cards[idx] = updated
                return
            }
            if animated {
                withAnimation(.easeInOut(duration: 0.2)) { cards[idx] = updated }
            } else {
                cards[idx] = updated
            }
        } else {
            // Rust owns agent identity/order via the skeleton deck.
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

private struct UsageCardView: View {
    let card: UsageCard
    /// TTS matches this agent — pastel wash (top bar stays brand purple).
    var speaking: Bool = false
    /// Runs the blocking authorize FFI and applies the result (UsageView owns both).
    var onAuthorize: (String) async -> Void = { _ in }
    /// Session-only; resets when the view is recreated.
    @State private var accountRevealed = false
    /// In-flight authorize; disables the button until the FFI returns.
    @State private var authorizing = false
    /// Frozen while speaking; re-rolled only on false → true.
    @State private var wash: Color?

    private var accountLabel: String? {
        guard let account = card.account?.trimmingCharacters(in: .whitespacesAndNewlines),
              !account.isEmpty
        else { return nil }
        return account
    }

    var body: some View {
        // Account transparent until tapped; reveal not persisted.
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
                        .accessibilityLabel(accountRevealed ? accountLabel : L.t("usage.account_hidden"))
                        .accessibilityAddTraits(.isButton)
                }
            }
            .padding(.leading, 6)
            VStack(spacing: 0) {
                ForEach(Array(card.rows.enumerated()), id: \.offset) { index, row in
                    if index > 0 { PlatterDivider() }
                    UsageRowView(row: row)
                }
                if card.needsAuth {
                    if !card.rows.isEmpty { PlatterDivider() }
                    UsageAuthRowView(authorizing: authorizing) {
                        guard !authorizing else { return }
                        authorizing = true
                        let agent = card.agent
                        Task {
                            await onAuthorize(agent)
                            authorizing = false
                        }
                    }
                }
            }
            .platterBackground(cornerRadius: Glass.platterCorner)
        }
        .overlay {
            if speaking, let wash {
                RoundedRectangle(cornerRadius: Glass.platterCorner, style: .continuous)
                    .fill(wash)
                    .allowsHitTesting(false)
            }
        }
        .onAppear {
            // First paint may already be speaking; onChange only fires later edges.
            if speaking { wash = Brand.randomPastelWash() }
        }
        .onChange(of: speaking) { _, on in
            if on { wash = Brand.randomPastelWash() }
        }
        .animation(.easeInOut(duration: 0.2), value: speaking)
    }
}

/// Catalog title, or prettified token when missing.
private func providerTitle(_ agent: String) -> String {
    let key = "usage.provider.\(agent)"
    let localized = L.t(key)
    if localized != key { return localized }
    return prettifyUsageToken(agent)
}

private func periodTitle(_ period: String) -> String {
    let key = "usage.\(period)"
    let localized = L.t(key)
    if localized != key { return localized }
    return prettifyUsageToken(period)
}

private func prettifyUsageToken(_ token: String) -> String {
    token
        .split(separator: "_")
        .map { part in
            guard let first = part.first else { return "" }
            return String(first).uppercased() + part.dropFirst()
        }
        .joined(separator: " ")
}

/// Guarded-credentials row: explanation + the only UI path that may prompt.
private struct UsageAuthRowView: View {
    let authorizing: Bool
    let onAuthorize: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            Text(L.t("usage.needs_auth"))
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            if authorizing {
                ProgressView()
                    .controlSize(.small)
            }
            Button(L.t("usage.authorize"), action: onAuthorize)
                .disabled(authorizing)
        }
        .platterRow()
    }
}

private struct UsageRowView: View {
    let row: UsageRow

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .lastTextBaseline) {
                Text(periodTitle(row.period)).glassRowTitle()
                Spacer(minLength: 8)
                // Remaining top-right; percent is the bar only.
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
