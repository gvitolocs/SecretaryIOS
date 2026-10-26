import SwiftUI

/// Training + Weight in one tab, like Strong's Profile (owner, 2026-10-24): the weight on top
/// (today's weight, the four stat cards, the trend), training below (the week, start a
/// workout, routines, totals) with the journal, progress and training settings one tap away.
struct FitnessView: View {
    @Environment(WeightStore.self) private var weight
    @Environment(GymStore.self) private var gym
    @State private var addingWeight = false

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.lg) {
                    weightSection
                    trainingSection
                }
                .padding(.horizontal, LucinaTokens.Space.gutter)
                .padding(.top, LucinaTokens.Space.sm)
                .padding(.bottom, LucinaTokens.Space.xxl)
            }
            .task(id: gym.view == nil) { scrollForScreenshot(proxy) }
        }
        .refreshable {
            async let w: Void = weight.refresh()
            async let g: Void = gym.refresh()
            _ = await (w, g)
        }
        .lucinaScreen("Training")
        .sheet(isPresented: $addingWeight) { QuickWeightSheet() }
        .task { if weight.view == nil { await weight.refresh() } }
        .gymHost()
    }

    // MARK: Weight

    @ViewBuilder private var weightSection: some View {
        SectionHeader("Weight") {
            NavigationLink { WeightView() } label: { Text("All weigh-ins").font(LucinaTokens.Fonts.subheadline) }
                .accessibilityIdentifier("weightAll")
        }
        PillButtonRow(icon: "scalemass.fill", title: "Today's weight", trailingText: weight.unit.rawValue) {
            addingWeight = true
        }
        .accessibilityLabel("Add measurement")
        .accessibilityIdentifier("weightQuickAdd")
        if let error = weight.actionError {
            ErrorBanner(message: error) { weight.actionError = nil }
        }
        if let view = weight.view {
            WeightView.statsGrid(view)
            if let series = view.series, series.count >= 2 {
                ChartCard(title: "Trend", icon: "chart.xyaxis.line") {
                    WeightView.trendChart(view, series: series)
                }
            }
        } else if let error = weight.loadError {
            retryCard(title: "Couldn't load your weight", detail: error) { await weight.refresh() }
        } else {
            ProgressView().frame(maxWidth: .infinity).padding(.vertical, LucinaTokens.Space.lg)
        }
    }

    // MARK: Training

    @ViewBuilder private var trainingSection: some View {
        SectionHeader("Training").id("training")
        if let view = gym.view {
            TrainingContent(view: view, showsMotto: false)
            Card(spacing: 0) {
                row("Journal", icon: "book.closed", id: "Journal") { HistoryView(view: view).lucinaScreen("Journal") }
                Divider().padding(.leading, 40)
                row("Progress", icon: "chart.xyaxis.line", id: "Progress") { ProgressScreen(view: view).lucinaScreen("Progress") }
                Divider().padding(.leading, 40)
                row("Training settings", icon: "slider.horizontal.3", id: "Settings") {
                    GymSettingsScreen().navigationTitle("Training settings")
                }
            }
        } else if let error = gym.loadError {
            retryCard(title: "Couldn\u{2019}t open your gym data", detail: error) { await gym.refresh() }
        } else {
            ProgressView().frame(maxWidth: .infinity).padding(.vertical, LucinaTokens.Space.lg)
        }
    }

    private func row<Destination: View>(_ title: LocalizedStringKey, icon: String, id: String,
                                        @ViewBuilder destination: @escaping () -> Destination) -> some View {
        NavigationLink(destination: destination) {
            HStack(spacing: 12) {
                Image(systemName: icon).foregroundStyle(Theme.accent).frame(width: 28).accessibilityHidden(true)
                Text(title).foregroundStyle(Theme.text)
                Spacer()
                Image(systemName: "chevron.right").font(.footnote.weight(.semibold)).foregroundStyle(Theme.textTertiary)
                    .accessibilityHidden(true)
            }
            .padding(.vertical, 12)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("gymSection-\(id)")
    }

    /// `-SecretaryScrollTo training` (screenshots): start scrolled to the training half.
    private func scrollForScreenshot(_ proxy: ScrollViewProxy) {
        #if DEBUG || SCREENSHOTS
        if gym.view != nil, UserDefaults.standard.string(forKey: "SecretaryScrollTo") == "training" {
            proxy.scrollTo("training", anchor: .top)
        }
        #endif
    }

    private func retryCard(title: LocalizedStringKey, detail: String, retry: @escaping () async -> Void) -> some View {
        Card {
            Text(title).font(LucinaTokens.Fonts.headline)
            Text(detail).font(.subheadline).foregroundStyle(Theme.textSecondary)
            Button("Try again") { Task { await retry() } }
        }
    }
}
