import Charts
import SwiftUI

// The Lucina components, built on the design tokens (Theme / LucinaTokens). New screens
// compose these instead of styling their own:
//
//   ScrollView { … }.lucinaScreen("Title")        black page, large title, accent tint
//   List { … }.lucinaList()                         the same for inset grouped lists
//   Card { … }                                      dark-grey rounded card
//   StatCard(title:icon:value:detail:)              icon + label + big value (the Weight tiles)
//   StatGrid { StatCard … }                         two columns of stat cards
//   ChartCard(title:) { Chart { … }.lucinaChart() } titled card around a chart
//   PillInputRow / PillButtonRow                    the pill row ("Today's weight … kg [Save]")
//   SectionHeader("Title")                          section title over cards
//   ChipButton(title:isOn:)                         capsule filter chip
//   PrimaryButton(title:)                           filled accent button (Components.swift)

extension View {
    /// A screen of cards: page background, large title that collapses on scroll, accent tint.
    func lucinaScreen(_ title: LocalizedStringKey? = nil) -> some View {
        modifier(LucinaScreen(title: title))
    }

    /// An inset grouped list in the Lucina look (Settings, Weight).
    func lucinaList() -> some View {
        listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .background(Theme.page.ignoresSafeArea())
            .tint(Theme.accent)
    }

    /// The sheet chrome every sheet shares: page background and the accent tint.
    func lucinaSheet() -> some View {
        background(Theme.page.ignoresSafeArea())
            .scrollContentBackground(.hidden)
            .tint(Theme.accent)
            .presentationCornerRadius(LucinaTokens.Radius.sheet)
    }
}

private struct LucinaScreen: ViewModifier {
    let title: LocalizedStringKey?

    func body(content: Content) -> some View {
        if let title {
            content
                .background(Theme.page.ignoresSafeArea())
                .scrollContentBackground(.hidden)
                .tint(Theme.accent)
                .navigationTitle(title)
                .navigationBarTitleDisplayMode(.large)
        } else {
            content
                .background(Theme.page.ignoresSafeArea())
                .scrollContentBackground(.hidden)
                .tint(Theme.accent)
        }
    }
}

/// Icon + label over a big value, with an optional line under it (the Weight tab's tiles).
struct StatCard: View {
    let title: LocalizedStringKey
    let icon: String
    let value: String
    var detail: String?
    var valueColor: Color = Theme.text

    var body: some View {
        Card(spacing: 6) {
            Label(title, systemImage: icon)
                .font(LucinaTokens.Fonts.caption)
                .foregroundStyle(Theme.textSecondary)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
            Text(verbatim: value)
                .font(LucinaTokens.Fonts.title2.monospacedDigit())
                .foregroundStyle(valueColor)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
            if let detail {
                Text(verbatim: detail)
                    .font(LucinaTokens.Fonts.caption2)
                    .foregroundStyle(Theme.textSecondary)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// Two columns of stat cards.
struct StatGrid<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        LazyVGrid(columns: [GridItem(.flexible(), spacing: LucinaTokens.Space.gridGap),
                            GridItem(.flexible(), spacing: LucinaTokens.Space.gridGap)],
                  spacing: LucinaTokens.Space.gridGap) { content }
    }
}

/// A titled card around a chart (the Weight tab's "Trend").
struct ChartCard<Content: View>: View {
    let title: LocalizedStringKey
    var icon: String?
    @ViewBuilder var content: Content

    var body: some View {
        Card(spacing: LucinaTokens.Space.md) {
            HStack(spacing: 6) {
                if let icon { Image(systemName: icon).foregroundStyle(Theme.accent).accessibilityHidden(true) }
                Text(title).font(LucinaTokens.Fonts.headline)
            }
            content
        }
    }
}

extension View {
    /// The chart height from the tokens (axes keep the system's secondary labels and grid).
    func lucinaChart(height: CGFloat = LucinaTokens.Chart.height) -> some View {
        frame(height: height)
    }
}

enum LucinaChartStyle {
    /// The main series line.
    static var line: StrokeStyle { StrokeStyle(lineWidth: LucinaTokens.Chart.lineWidth, lineCap: .round, lineJoin: .round) }
    /// Goal and target rules.
    static var goal: StrokeStyle { StrokeStyle(lineWidth: 1, dash: LucinaTokens.Chart.goalDash) }
    static var goalColor: Color { LucinaTokens.Chart.goal.opacity(LucinaTokens.Chart.goalOpacity) }
    static var pointColor: Color { LucinaTokens.Chart.point.opacity(LucinaTokens.Chart.pointOpacity) }
    static var pointSize: CGFloat { LucinaTokens.Chart.pointSize }
    /// Bars and areas.
    static var area: LinearGradient {
        LinearGradient(colors: [Theme.accent.opacity(0.35), Theme.accent.opacity(0.02)], startPoint: .top, endPoint: .bottom)
    }
}

/// The pill row: icon, a text field, a unit and a filled trailing button.
struct PillInputRow: View {
    let icon: String
    let placeholder: LocalizedStringKey
    @Binding var text: String
    var unit: String?
    var buttonTitle: LocalizedStringKey = "Save"
    var keyboard: UIKeyboardType = .default
    var isBusy = false
    let onSubmit: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: icon).foregroundStyle(Theme.accent).accessibilityHidden(true)
            TextField(placeholder, text: $text)
                .keyboardType(keyboard)
                .submitLabel(.done)
                .onSubmit(onSubmit)
            if let unit { Text(verbatim: unit).foregroundStyle(Theme.textSecondary) }
            Button(action: onSubmit) {
                Group {
                    if isBusy { ProgressView().tint(Theme.onAccent) } else { Text(buttonTitle) }
                }
                .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
                .foregroundStyle(Theme.onAccent)
                .padding(.horizontal, 16)
                .frame(height: 34)
                .background(Theme.accentFill, in: Capsule())
            }
            .buttonStyle(.plain)
            .disabled(isBusy || text.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.leading, 16)
        .padding(.trailing, 6)
        .frame(minHeight: 46)
        .background(Theme.card, in: Capsule())
    }
}

/// The pill row as a button (it opens a sheet): icon, a prompt, a trailing accent symbol.
struct PillButtonRow: View {
    let icon: String
    let title: LocalizedStringKey
    var trailingText: String?
    var trailingIcon = "plus.circle.fill"
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 10) {
                Image(systemName: icon).foregroundStyle(Theme.accent).accessibilityHidden(true)
                Text(title).foregroundStyle(Theme.textSecondary)
                Spacer()
                if let trailingText { Text(verbatim: trailingText).foregroundStyle(Theme.textSecondary) }
                Image(systemName: trailingIcon)
                    .font(.title2)
                    .foregroundStyle(Theme.accent)
                    .accessibilityHidden(true)
            }
            .padding(.leading, 16)
            .padding(.trailing, 8)
            .frame(minHeight: 46)
            .background(Theme.card, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
    }
}

/// A section title over cards (the cards' own titles use `headline`).
struct SectionHeader<Trailing: View>: View {
    let title: LocalizedStringKey
    @ViewBuilder var trailing: Trailing

    init(_ title: LocalizedStringKey, @ViewBuilder trailing: () -> Trailing = { EmptyView() }) {
        self.title = title
        self.trailing = trailing()
    }

    var body: some View {
        HStack {
            Text(title).font(LucinaTokens.Fonts.title3)
            Spacer()
            trailing
        }
        .padding(.top, LucinaTokens.Space.sm)
        .accessibilityAddTraits(.isHeader)
    }
}

/// A capsule filter chip: accent fill when on, the field colour when off.
struct ChipButton: View {
    let title: LocalizedStringKey
    var icon: String?
    let isOn: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                if let icon { Image(systemName: icon).accessibilityHidden(true) }
                Text(title)
            }
            .font(LucinaTokens.Fonts.subheadline.weight(.semibold))
            .foregroundStyle(isOn ? Theme.onAccent : Theme.text)
            .padding(.horizontal, 14)
            .frame(height: 34)
            .background(isOn ? Theme.accentFill : Theme.field, in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(isOn ? .isSelected : [])
    }
}

/// A segmented bar of a screen's sections, as capsules (Gym's Training/Journal/Progress/Settings).
struct SectionBar<Item: Hashable & Identifiable>: View {
    let items: [Item]
    @Binding var selection: Item
    let title: (Item) -> LocalizedStringKey
    let icon: (Item) -> String

    var body: some View {
        HStack(spacing: 4) {
            ForEach(items) { item in
                let on = item == selection
                Button { selection = item } label: {
                    VStack(spacing: 3) {
                        Image(systemName: icon(item)).font(.system(size: 15, weight: .semibold))
                        Text(title(item)).font(LucinaTokens.Fonts.caption2.weight(.semibold))
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 7)
                    .foregroundStyle(on ? Theme.accent : Theme.textSecondary)
                    .background(on ? Theme.accentSoft : Color.clear, in: Capsule())
                    .contentShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(title(item))
                .accessibilityAddTraits(on ? .isSelected : [])
            }
        }
        .padding(4)
        .background(Theme.card, in: Capsule())
    }
}
