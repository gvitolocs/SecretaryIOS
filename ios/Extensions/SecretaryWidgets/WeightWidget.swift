import SwiftUI
import WidgetKit

/// Today's weight, the 30-day trend, and a "+" that opens the quick weight entry.
struct WeightWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<WeightSection>

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: WeightSection(fetchedAt: Date(), unit: "kg", currentLabel: "80.4 kg", lastAt: Date(),
                                                       weeklyChangeLabel: "-0.6 kg", weeklyChangeGrams: -600,
                                                       trend: [82, 81.8, 81.9, 81.5, 81.2, 81.3, 80.9, 80.7, 80.6, 80.4]), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(WeightSection.self, preview: context.isPreview) { f, _ in try await f.weight() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(WeightSection.self) { f, _ in try await f.weight() }
                // Midnight too: "today" flips to "log today's weight".
                completion(Timeline(entries: [Entry(date: Date(), section: section, state: state)],
                                    policy: SectionLoader.policy(minutes: 120, boundary: SectionLoader.midnight)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.weight.rawValue, provider: Provider()) { entry in
            WeightWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Weight")
        .description("Today's weight and the trend. Tap + to log a weigh-in.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct WeightWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<WeightSection>

    private var section: WeightSection? { entry.section }
    private var logURL: URL { WidgetRoute.url("weight/log") }

    var body: some View {
        switch family {
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                VStack(spacing: 0) {
                    Image(systemName: section?.loggedToday == true ? "scalemass.fill" : "plus").font(.caption2)
                    Text(number ?? "–").font(.system(size: 14, weight: .semibold)).minimumScaleFactor(0.6).privacySensitive()
                }
            }
            .widgetURL(logURL)
        case .accessoryInline:
            Text("\(Image(systemName: "scalemass")) \(section?.currentLabel ?? String(localized: "Log weight"))").privacySensitive()
                .widgetURL(logURL)
        case .accessoryRectangular:
            HStack(spacing: LucinaTokens.Space.sm) {
                VStack(alignment: .leading, spacing: 1) {
                    Label(section?.currentLabel ?? "–", systemImage: "scalemass.fill").font(.headline).privacySensitive().widgetAccentable()
                    Text(section?.loggedToday == true ? (section?.weeklyChangeLabel.map { String(localized: "\($0) this week") } ?? "")
                         : String(localized: "Tap to log today"))
                        .font(.caption).lineLimit(1).privacySensitive()
                }
                Sparkline(values: section?.trend ?? [], color: .primary).frame(width: 44, height: 24).privacySensitive()
            }
            .widgetURL(logURL)
        case .systemMedium:
            HStack(spacing: LucinaTokens.Space.lg) {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    header
                    figures
                }
                VStack(alignment: .trailing, spacing: LucinaTokens.Space.xs) {
                    Text("30 days").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                    Sparkline(values: section?.trend ?? []).privacySensitive()
                }
            }
            .widgetURL(WidgetRoute.url("weight"))
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                header
                figures
                Sparkline(values: section?.trend ?? []).frame(height: 26).privacySensitive()
            }
            .widgetURL(WidgetRoute.url("weight"))
        }
    }

    private var number: String? {
        section?.currentLabel?.split(separator: " ").first.map(String.init)
    }

    private var header: some View {
        HStack {
            WidgetHeader(title: "Weight", symbol: "scalemass.fill")
            Link(destination: logURL) { CircleAction(symbol: "plus", size: 24) }
        }
    }

    @ViewBuilder private var figures: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if let section, let current = section.currentLabel {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xxs) {
                Text(current).font(LucinaTokens.Fonts.title2).foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(1).minimumScaleFactor(0.7).privacySensitive()
                if let change = section.weeklyChangeLabel {
                    Text("\(change) this week").font(LucinaTokens.Fonts.caption)
                        .foregroundStyle((section.weeklyChangeGrams ?? 0) <= 0 ? LucinaTokens.Colors.positive : LucinaTokens.Colors.warning)
                        .lineLimit(1).privacySensitive()
                }
                Text(section.loggedToday ? String(localized: "Logged today") : String(localized: "Not logged today"))
                    .font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
            }
            Spacer(minLength: 0)
        } else {
            WidgetMessage(symbol: "scalemass", text: entry.state == .offline ? "Lucina is unreachable." : "No weigh-ins yet. Tap + to log one.")
        }
    }
}
