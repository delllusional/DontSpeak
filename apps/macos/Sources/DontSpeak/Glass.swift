// Shared Liquid Glass surfaces. macOS 26+ uses `.glassEffect`; earlier OS falls back to
// ultraThinMaterial. Control-Center pattern: window = continuous slab; content groups =
// translucent platters (material, not glass-on-glass).

import SwiftUI

enum Glass {
    static let panelCorner: CGFloat = 18
    static let platterCorner: CGFloat = 12
    /// Content margin window edge → platters; shared so screens can't drift.
    static let windowInset: CGFloat = 16
    /// Same as sides. Full-height sidebar bleeds under the bar so MainWindow adds this on top
    /// of title-bar height; detail respects safe area so this is its whole top gap.
    static let windowTopInset: CGFloat = windowInset
    /// Shared expand/collapse curve (Status rows + DisclosureRow).
    static let expandAnimation: Animation = .snappy(duration: 0.2)
}

/// Floating shapes (dictation overlay): bare `.glassEffect(.regular)` — no container, no tint.
private struct GlassBackground: ViewModifier {
    var cornerRadius: CGFloat = Glass.panelCorner
    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        // Uniform hairline so Liquid Glass's brighter top-edge sheen doesn't look one-sided.
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

/// Full-bleed window glass + frosted title-bar strip in front (content scrolling under blurs).
private struct WindowGlassBackground: ViewModifier {
    /// Flat color fill for the strip (material tints wash out when unfocused).
    var topTint: Color = .clear
    var topHeight: CGFloat = 0
    func body(content: Content) -> some View {
        content
            .background {
                if #available(macOS 26, *) {
                    Rectangle().fill(.clear).glassEffect(.regular, in: Rectangle())
                } else {
                    Rectangle().fill(.ultraThinMaterial)
                }
            }
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
    func glassBackground(cornerRadius: CGFloat = Glass.panelCorner) -> some View {
        modifier(GlassBackground(cornerRadius: cornerRadius))
    }

    /// Pair with `.glassWindow()` so the host NSWindow is clear.
    func windowGlass(topTint: Color = .clear, topHeight: CGFloat = 0) -> some View {
        modifier(WindowGlassBackground(topTint: topTint, topHeight: topHeight))
    }

    /// Shared Status/Tools inset. System title-bar height keeps the first platter clear of
    /// traffic lights (content must not ignore safe area); glass slab still fills behind.
    /// Honest content-min size → resizable window wraps with no phantom bottom gap.
    func windowContentInset() -> some View {
        padding(.top, Glass.windowTopInset)
            .padding([.horizontal, .bottom], Glass.windowInset)
    }

    /// Material platter (keeps text legible on the window glass slab).
    func platterBackground(cornerRadius: CGFloat = Glass.platterCorner) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        return
            self
            .background(.regularMaterial, in: shape)
            .clipShape(shape)
            .overlay(shape.strokeBorder(.separator.opacity(0.6), lineWidth: 0.5))
    }

    func platterRow() -> some View {
        padding(.horizontal, 14).padding(.vertical, 9)
    }
}

/// Optional header above a material card. Status/Tools use headerless platters.
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

/// Inset hairline between platter rows (skip after the last).
struct PlatterDivider: View {
    var body: some View {
        Divider().padding(.leading, 14)
    }
}

func expandToggle(_ expanded: Binding<Bool>) {
    withAnimation(Glass.expandAnimation) { expanded.wrappedValue.toggle() }
}

/// Collapsible platter row; open set owned by caller keyed by `id` (Tools + Libraries share look).
struct DisclosureRow<Header: View, Content: View>: View {
    @Binding var expanded: Set<String>
    let id: String
    @ViewBuilder var header: () -> Header
    @ViewBuilder var content: () -> Content

    private var isOpen: Bool { expanded.contains(id) }

    var body: some View {
        VStack(spacing: 0) {
            Button {
                withAnimation(Glass.expandAnimation) {
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

/// Spread label leading / value trailing — Form's free layout lost when rows sit in a VStack.
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

// MARK: - Semantic typography (relative styles — track Dynamic Type)

extension View {
    func glassSectionHeader() -> some View {
        font(.subheadline).fontWeight(.semibold).foregroundStyle(.secondary)
    }

    func glassRowTitle() -> some View {
        font(.body)
    }

    func glassRowDetail() -> some View {
        font(.subheadline).foregroundStyle(.secondary)
    }

    func glassCaption() -> some View {
        font(.caption).foregroundStyle(.secondary)
    }
}
