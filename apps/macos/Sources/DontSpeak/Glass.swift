//  Glass.swift
//
//  Shared Liquid-Glass surfaces for the app's windows and floating overlay. macOS 26
//  (Tahoe) gives us real Liquid Glass via `.glassEffect`; earlier systems fall back to
//  a translucent material so the same call site renders sensibly everywhere. Kept in one
//  place so the availability branch lives ONCE (the dictation overlay and the Status /
//  Tools windows all share it).
//
//  The visual model is the Control-Center / HUD pattern: the WINDOW is one continuous
//  glass slab (`glassWindow` + `windowGlass`), and content is grouped into translucent
//  "platters" (`Platter` / `platterBackground`) that float on it — a secondary MATERIAL
//  surface, not opaque cards, so text stays legible over any desktop behind the glass.

import SwiftUI

/// Corner radii for the glass surfaces — the window slab is rounder than the platters
/// that nest inside it, so the platters read as sitting *within* the panel.
enum Glass {
    /// The floating overlay / panel corner (matches the dictation pill).
    static let panelCorner: CGFloat = 18
    /// An inner platter corner — tighter than the panel so it nests cleanly.
    static let platterCorner: CGFloat = 12
    /// The content margin between a window's glass edge and its platters — 16pt. Shared by every
    /// screen so margins can't drift.
    static let windowInset: CGFloat = 16
    /// The margin between the content and the title-bar strip — the SAME as the sides, so the
    /// content is inset evenly on all four edges. The detail respects the title-bar safe area, so
    /// this is its whole top gap; the full-height sidebar bleeds UNDER the bar, so there it's
    /// added ON TOP of the title-bar height (see MainWindow) to land its first row level.
    static let windowTopInset: CGFloat = windowInset
}

/// Liquid Glass on macOS 26+, ultra-thin material otherwise, clipped to a rounded rect.
/// For FLOATING shapes (the dictation overlay) — a single bare `.glassEffect` with the
/// default `.regular` variant, the idiomatic form per Apple's "Applying Liquid Glass to
/// custom views": `GlassEffectContainer` is for grouping/morphing multiple glass shapes
/// (we have one), and `.tint` is reserved for meaning, not a uniform look. The material
/// reflects surrounding content, so it reads glassier over dark apps than bright ones —
/// that backdrop adaptation is intended.
private struct GlassBackground: ViewModifier {
    var cornerRadius: CGFloat = Glass.panelCorner
    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        // A faint UNIFORM hairline around the whole card: gives every edge the same crisp
        // highlight so Liquid Glass's brighter top-edge sheen doesn't read as one edge being
        // lighter than the others.
        let hairline = shape.strokeBorder(.white.opacity(0.08))
        if #available(macOS 26, *) {
            content.glassEffect(.regular, in: shape).overlay(hairline)
        } else {
            content
                .background(.ultraThinMaterial, in: shape)
                .overlay(hairline)
        }
    }
}

/// Full-bleed glass for a window: a continuous Liquid-Glass slab BEHIND the content, plus a
/// frosted TITLE-BAR strip OVERLAY in FRONT of it (so content scrolling under the strip is
/// blurred — the glass-toolbar look). The window itself supplies the outer rounding (see
/// `glassWindow`), so both layers fill edge-to-edge and ignore safe area.
private struct WindowGlassBackground: ViewModifier {
    /// The live state wash for the TITLE-BAR strip (narrating = purple, dictating = orange,
    /// clear when idle) and its measured height. The wash is a flat color fill — NOT a glass
    /// tint — so it shows whether or not the window is key (a material tint follows the
    /// window's active state and washes out when unfocused). `topHeight == 0` ⇒ no strip.
    var topTint: Color = .clear
    var topHeight: CGFloat = 0
    func body(content: Content) -> some View {
        content
            // The frosted Liquid-Glass slab (untinted) BEHIND the content, filling the window.
            .background {
                if #available(macOS 26, *) {
                    Rectangle().fill(.clear).glassEffect(.regular, in: Rectangle())
                } else {
                    Rectangle().fill(.ultraThinMaterial)
                }
            }
            // The TITLE-BAR strip, IN FRONT of the content: a frosted band so content
            // scrolling up UNDER it is BLURRED (the glass-toolbar look). The state tint
            // (purple narrating / orange dictating) washes over the frost. Pinned to the top,
            // sized to the title-bar height, hit-test-transparent so it never blocks content
            // or the system traffic-light buttons.
            .overlay(alignment: .top) {
                ZStack {
                    if #available(macOS 26, *) {
                        Rectangle().fill(.clear).glassEffect(.regular, in: Rectangle())
                    } else {
                        Rectangle().fill(.ultraThinMaterial)
                    }
                    Rectangle()
                        .fill(topTint)
                        .animation(.easeInOut(duration: 0.5), value: topTint)
                }
                .frame(height: topHeight)
                .frame(maxWidth: .infinity)
                .ignoresSafeArea(edges: .top)
                .allowsHitTesting(false)
            }
    }
}

extension View {
    /// Floating-shape glass (the dictation overlay).
    func glassBackground(cornerRadius: CGFloat = Glass.panelCorner) -> some View {
        modifier(GlassBackground(cornerRadius: cornerRadius))
    }

    /// The window-filling glass slab (Status / Tools windows). Pair with `.glassWindow()`
    /// (in WindowHelpers) so the host `NSWindow` is itself clear and only this shows.
    func windowGlass(topTint: Color = .clear, topHeight: CGFloat = 0) -> some View {
        modifier(WindowGlassBackground(topTint: topTint, topHeight: topHeight))
    }

    /// The content inset shared by the Status and Tools windows — `Glass.windowInset` on the
    /// sides and bottom, `Glass.windowTopInset` (half) below the traffic-light bar, since the
    /// system title-bar safe area already supplies most of the top clearance. Do NOT
    /// `.ignoresSafeArea()` on the content: the system reserves the hidden title-bar height,
    /// so the first platter clears the traffic lights while the glass slab (`windowGlass`)
    /// still fills edge-to-edge behind the bar. Respecting the safe area also keeps the
    /// content-min size honest, so a resizable window wraps its content with no phantom
    /// bottom gap.
    func windowContentInset() -> some View {
        padding(.top, Glass.windowTopInset)
            .padding([.horizontal, .bottom], Glass.windowInset)
    }

    /// A translucent "platter" surface for a group of rows: a frosted MATERIAL card (not
    /// opaque) with a hairline edge, clipped to a rounded rect. Material — not a second
    /// glass layer — keeps text legible on the platter and avoids glass-on-glass muddiness
    /// over the window slab; it's the readable secondary surface the HUD pattern calls for.
    func platterBackground(cornerRadius: CGFloat = Glass.platterCorner) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        return
            self
            .background(.regularMaterial, in: shape)
            .clipShape(shape)
            .overlay(shape.strokeBorder(.separator.opacity(0.6), lineWidth: 0.5))
    }

    /// Consistent insets for one row inside a platter (replaces the grouped-Form row insets).
    func platterRow() -> some View {
        padding(.horizontal, 14).padding(.vertical, 9)
    }
}

/// A grouped "platter": an OPTIONAL header label rendered above the surface (standard
/// grouped-list style, on the bare window glass), then the rows on a translucent material
/// card. Status and Tools use headerless platters. The caller stacks rows inside with
/// `Divider()`s between them (none after the last).
struct Platter<Content: View>: View {
    var header: String? = nil
    var cornerRadius: CGFloat = Glass.platterCorner
    @ViewBuilder var content: () -> Content

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            if let header {
                Text(header)
                    .glassSectionHeader()
                    .padding(.leading, 6)
            }
            VStack(spacing: 0) { content() }
                .platterBackground(cornerRadius: cornerRadius)
        }
    }
}

/// A hairline divider between rows inside a platter — inset so it doesn't touch the
/// rounded corners. Use BETWEEN rows, never after the last.
struct PlatterDivider: View {
    var body: some View {
        Divider().padding(.leading, 14)
    }
}

/// A collapsible platter row: a tappable header (the caller's `header`, plus a trailing
/// rotating chevron) that reveals `content` when expanded. The open set is owned by the
/// caller and keyed by `id`, so a whole list of these shares ONE `@State` and the disclosure
/// look + animation live in one place — shared by the Tools and Libraries panes so they can't
/// drift.
struct DisclosureRow<Header: View, Content: View>: View {
    @Binding var expanded: Set<String>
    let id: String
    @ViewBuilder var header: () -> Header
    @ViewBuilder var content: () -> Content

    private var isOpen: Bool { expanded.contains(id) }

    var body: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(.snappy(duration: 0.2)) {
                    if isOpen { expanded.remove(id) } else { expanded.insert(id) }
                }
            } label: {
                HStack(spacing: 8) {
                    header()
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(.secondary)
                        .rotationEffect(.degrees(isOpen ? 90 : 0))
                }
                .frame(maxWidth: .infinity)
                .padding(.horizontal, 14).padding(.vertical, 10)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)

            if isOpen {
                PlatterDivider()
                content()
            }
        }
    }
}

/// Spreads a `LabeledContent`'s label to the leading edge and its value to the trailing
/// edge — the look `Form` gives for free, which we lose once detail rows live in a plain
/// VStack on a platter. Apply `.labeledContentStyle(.spread)` to a container of rows.
struct SpreadLabeledContentStyle: LabeledContentStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 8) {
            configuration.label
            Spacer(minLength: 12)
            configuration.content
        }
    }
}

extension LabeledContentStyle where Self == SpreadLabeledContentStyle {
    static var spread: SpreadLabeledContentStyle { .init() }
}

// MARK: - Semantic typography
//
// The glass panels use exactly four text roles. Naming them here (instead of spelling
// `.font(...) + .foregroundStyle(...)` inline at each call site) keeps the hierarchy
// consistent and in ONE place — the same single-source-of-truth approach as the colors
// and the glass surfaces. All map to Apple's relative text styles, so they track Dynamic
// Type / the system text size rather than hard-coding points.
extension View {
    /// A platter's group heading. Secondary + semibold subheadline
    /// — the grouped-list convention: quieter and smaller than the body-sized row TITLES it
    /// groups, so it reads as a label for the card rather than competing with its rows.
    func glassSectionHeader() -> some View {
        font(.subheadline).fontWeight(.semibold).foregroundStyle(.secondary)
    }

    /// A primary row title (engine name, role, permission name, tool name). The default body
    /// style, stated explicitly so the role is obvious at the call site.
    func glassRowTitle() -> some View {
        font(.body)
    }

    /// An inline secondary qualifier sitting beside a title or value — the concrete
    /// backend/model ("ONNX", "Kokoro"), the "all-time" tag, a min–max range, a duration.
    /// Subheadline + secondary so it recedes behind the primary text it annotates.
    func glassRowDetail() -> some View {
        font(.subheadline).foregroundStyle(.secondary)
    }

    /// A caption / hint / explanatory sub-line (empty-state hints, a permission's purpose).
    func glassCaption() -> some View {
        font(.caption).foregroundStyle(.secondary)
    }
}
