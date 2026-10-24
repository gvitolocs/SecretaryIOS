import SwiftUI

// The Gym tab's names for the shared Lucina look (Theme, design/tokens.json). The tab used
// to keep the original LucinaGym palette (cream, forest, lime) in light mode; it now matches
// the Weight, Settings and Chat tabs.

enum GymPalette {
    static let background = Theme.page
    /// Tints and icons (periwinkle).
    static let forest = Theme.accent
    /// Filled surfaces under white text (the primary button, the workout in progress).
    static let forestFill = Theme.accentFill
    static let ink = Theme.text
    static let muted = Theme.textSecondary
    /// Highlights on a filled accent surface.
    static let lime = Theme.onAccent
}

struct GymCard<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        Card(spacing: 16) { content }
    }
}

struct GymEyebrow: View {
    let text: LocalizedStringKey

    init(text: LocalizedStringKey) { self.text = text }
    /// Dynamic text (dates, counts) shown as is.
    init(verbatim: String) { text = LocalizedStringKey(verbatim) }

    var body: some View {
        Text(text)
            .textCase(.uppercase)
            .font(LucinaTokens.Fonts.caption2.weight(.semibold))
            .tracking(1.2)
            .foregroundStyle(GymPalette.muted)
    }
}

struct GymMetric: View {
    let value: String
    let label: LocalizedStringKey

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(verbatim: value)
                .font(LucinaTokens.Fonts.title2.monospacedDigit())
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            GymEyebrow(text: label)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

struct GymPrimaryButton: View {
    let title: LocalizedStringKey
    var icon: String = "plus"
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label(title, systemImage: icon)
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 15)
        }
        .buttonStyle(.plain)
        .foregroundStyle(Theme.onAccent)
        .background(GymPalette.forestFill, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.button, style: .continuous))
    }
}

struct GymEmptyCard: View {
    let icon: String
    let title: LocalizedStringKey
    let detail: LocalizedStringKey

    var body: some View {
        GymCard {
            Image(systemName: icon).font(.title).foregroundStyle(GymPalette.forest).accessibilityHidden(true)
            Text(title).font(LucinaTokens.Fonts.title3)
            Text(detail).font(.subheadline).foregroundStyle(GymPalette.muted)
        }
        .accessibilityElement(children: .combine)
    }
}

extension View {
    /// The Gym tab's environment: the shared text colour and accent tint (light and dark).
    func gymStyle() -> some View {
        foregroundStyle(GymPalette.ink)
            .tint(GymPalette.forest)
    }

    /// Monospaced digits in set fields.
    func gymFieldFont() -> some View {
        font(.body.monospacedDigit())
    }
}
