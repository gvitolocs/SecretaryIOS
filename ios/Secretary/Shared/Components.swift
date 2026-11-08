import SwiftUI

/// The Lucina look (the Weight, Settings and Chat tabs), from the design tokens in
/// design/tokens.json (generated into LucinaTokens.swift). Screens use these names and the
/// components here and in DesignSystem.swift, never their own colours or corner radii.
enum Theme {
    typealias Tokens = LucinaTokens
    /// Periwinkle #7F8FE0 (dark) / indigo #4A5BB5 (light), from the Lucina icon.
    static let accent = Tokens.Colors.accent
    /// Filled surfaces that carry white text (buttons, the user's bubbles): #4A5BB5 in
    /// both modes (6.1:1 under white), because #7F8FE0 is only 3:1 under white text.
    static let accentFill = Tokens.Colors.accentFill
    static let accentSoft = Tokens.Colors.accentSoft
    static let onAccent = Tokens.Colors.onAccent
    static let page = Tokens.Colors.bg
    static let card = Tokens.Colors.card
    static let cardRaised = Tokens.Colors.cardRaised
    static let field = Tokens.Colors.field
    static let separator = Tokens.Colors.separator
    static let text = Tokens.Colors.text
    static let textSecondary = Tokens.Colors.textSecondary
    static let textTertiary = Tokens.Colors.textTertiary
    static let positive = Tokens.Colors.positive
    static let negative = Tokens.Colors.negative
    static let warning = Tokens.Colors.warning
    static let info = Tokens.Colors.info
    static let cornerRadius = Tokens.Radius.card
}

struct Card<Content: View>: View {
    var spacing: CGFloat = 12
    var cornerRadius: CGFloat = Theme.cornerRadius
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: spacing) { content }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(LucinaTokens.Space.cardPadding)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
    }
}

struct Eyebrow: View {
    let text: LocalizedStringKey

    init(text: LocalizedStringKey) { self.text = text }
    init(verbatim: String) { text = LocalizedStringKey(verbatim) }

    var body: some View {
        Text(text)
            .textCase(.uppercase)
            .font(.caption2.weight(.semibold))
            .tracking(1.2)
            .foregroundStyle(.secondary)
    }
}

struct Metric: View {
    let value: String
    let label: LocalizedStringKey

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: value)
                .font(.title3.weight(.semibold).monospacedDigit())
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            Eyebrow(text: label)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

struct PrimaryButton: View {
    let title: LocalizedStringKey
    var icon: String = "plus"
    var isBusy = false
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 8) {
                if isBusy {
                    ProgressView().tint(.white)
                } else {
                    Image(systemName: icon)
                }
                Text(title)
            }
            .font(.headline)
            .frame(maxWidth: .infinity)
            .padding(.vertical, 14)
        }
        .buttonStyle(.borderedProminent)
        .buttonBorderShape(.roundedRectangle(radius: LucinaTokens.Radius.button))
        .tint(Theme.accentFill)
        .disabled(isBusy)
    }
}

struct EmptyCard: View {
    let icon: String
    let title: LocalizedStringKey
    let detail: LocalizedStringKey

    var body: some View {
        Card {
            Image(systemName: icon)
                .font(.title)
                .foregroundStyle(Theme.accent)
                .accessibilityHidden(true)
            Text(title).font(.title3.bold())
            Text(detail).font(.subheadline).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
    }
}

/// A dismissible error line shown above content.
struct ErrorBanner: View {
    let message: String
    var systemImage = "exclamationmark.triangle.fill"
    var onDismiss: (() -> Void)?

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: systemImage)
                .foregroundStyle(Theme.warning)
                .accessibilityHidden(true)
            Text(message)
                .font(.subheadline)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            if let onDismiss {
                Button(action: onDismiss) {
                    Image(systemName: "xmark")
                        .font(.footnote.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .padding(4)
                }
                .accessibilityLabel("Dismiss")
            }
        }
        .padding(12)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: LucinaTokens.Radius.tile, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("errorBanner")
    }
}

func dateLabel(_ milliseconds: Int) -> String {
    Date(timeIntervalSince1970: Double(milliseconds) / 1000).formatted(date: .abbreviated, time: .omitted)
}

func dateTimeLabel(_ milliseconds: Int) -> String {
    Date(timeIntervalSince1970: Double(milliseconds) / 1000).formatted(date: .abbreviated, time: .shortened)
}

extension View {
    /// Dismisses the keyboard.
    func endEditing() {
        UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }
}
