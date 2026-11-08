import AppIntents
import SwiftUI
import WidgetKit

/// The days left until pinned events: "15 days — Network exam" with the date below, one line per
/// event; "Today" and "Tomorrow" by name; an event drops off once past (Countdown.isUpcoming).
/// The events are the ones chosen in the widget's configuration, else the ones pinned in the app.
struct CountdownWidget: Widget {
    struct Entry: TimelineEntry {
        var date: Date
        var events: [CountdownEvent]
        var state: LoadState = .ok
    }

    struct Provider: AppIntentTimelineProvider {
        typealias Entry = CountdownWidget.Entry
        typealias Intent = CountdownWidgetConfiguration

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), events: [Self.sample])
        }

        func snapshot(for configuration: Intent, in context: Context) async -> Entry {
            let first = await entries(configuration, refresh: !context.isPreview).first
            let entry = first ?? placeholder(in: context)
            return context.isPreview && entry.events.isEmpty ? placeholder(in: context) : entry
        }

        func timeline(for configuration: Intent, in context: Context) async -> Timeline<Entry> {
            // An entry at each midnight (the count goes down) and when a timed event ends; the
            // last one asks for a new timeline.
            let all = await entries(configuration, refresh: true)
            return Timeline(entries: all, policy: .atEnd)
        }

        private func entries(_ configuration: Intent, refresh: Bool) async -> [Entry] {
            let upcoming: [CountdownEvent]
            if refresh { upcoming = await Countdown.refreshUpcoming() } else { upcoming = Countdown.upcoming }
            let chosen = configuration.events?.map(\.event) ?? []
            let events = chosen.isEmpty ? Countdown.pinned : chosen
            let state: LoadState = WidgetFetcher.fromBridge() == nil ? .signedOut : .ok
            let now = Date()
            return ([now] + Countdown.changes(after: now, events: events)).map { date in
                Entry(date: date, events: Countdown.visible(events, fresher: upcoming, now: date), state: state)
            }
        }

        static var sample: CountdownEvent {
            let day = Calendar.current.date(byAdding: .day, value: 15, to: Calendar.current.startOfDay(for: Date())) ?? Date()
            return CountdownEvent(id: "sample", title: String(localized: "Network exam"), start: day.addingTimeInterval(9 * 3600))
        }
    }

    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: WidgetKind.countdown.rawValue, intent: CountdownWidgetConfiguration.self, provider: Provider()) { entry in
            CountdownWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Countdown")
        .description("The days left until the events you pin: an exam, a trip, a birthday.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular, .accessoryInline])
    }
}

struct CountdownWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: CountdownWidget.Entry

    private var events: [CountdownEvent] { entry.events }

    var body: some View {
        content.widgetURL(WidgetRoute.url("countdowns"))
    }

    @ViewBuilder private var content: some View {
        switch family {
        case .accessoryInline:
            if let first = events.first {
                Text("\(Image(systemName: "hourglass")) \(Text(verbatim: "\(label(first)) · \(first.title)"))").privacySensitive()
            } else {
                Text("\(Image(systemName: "hourglass")) No countdowns")
            }
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                if events.count == 1, let event = events.first {
                    Text(verbatim: label(event)).font(.headline).widgetAccentable()
                    Text(verbatim: event.title).font(.caption).lineLimit(1).privacySensitive()
                    Text(verbatim: Countdown.dateText(event)).font(.caption2).lineLimit(1)
                } else if events.isEmpty {
                    Label("Countdown", systemImage: "hourglass").font(.headline).widgetAccentable()
                    Text("Pin an event in Secretary.").font(.caption).lineLimit(2)
                } else {
                    ForEach(events.prefix(3)) { event in
                        HStack(alignment: .firstTextBaseline, spacing: 4) {
                            Text(verbatim: label(event)).font(.headline).widgetAccentable()
                            Text(verbatim: event.title).font(.caption).lineLimit(1).privacySensitive()
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .systemMedium:
            if events.isEmpty {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    WidgetHeader(title: "Countdown", symbol: "hourglass")
                    message
                }
            } else {
                // One line per event, tiled: the title and date on the left, the count on the right.
                VStack(spacing: 0) {
                    ForEach(Array(events.prefix(3).enumerated()), id: \.element.id) { index, event in
                        if index > 0 { Divider().overlay(LucinaTokens.Colors.separator) }
                        row(event, figure: events.count == 1 ? 52 : events.count == 2 ? 40 : 30)
                            .frame(maxHeight: .infinity)
                    }
                }
            }
        default:
            if let event = events.first {
                // The owner's reference: the title top left, the count big in the bottom right.
                let parts = Countdown.parts(event, now: entry.date)
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xxs) {
                    Text(verbatim: event.title)
                        .font(.system(size: LucinaTokens.FontSize.title3, weight: .semibold))
                        .foregroundStyle(LucinaTokens.Colors.text)
                        .lineLimit(2).minimumScaleFactor(0.8).privacySensitive()
                    Text(verbatim: Countdown.dateText(event))
                        .font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                        .lineLimit(1).minimumScaleFactor(0.8)
                    Spacer(minLength: 0)
                    figure(parts, size: parts.value.allSatisfy(\.isNumber) ? 64 : 34)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                }
            } else {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    WidgetHeader(title: "Countdown", symbol: "hourglass")
                    message
                }
            }
        }
    }

    /// The small label above the big figure, right-aligned ("days" over "15").
    private func figure(_ parts: (value: String, unit: String?), size: CGFloat) -> some View {
        VStack(alignment: .trailing, spacing: -size * 0.08) {
            if let unit = parts.unit {
                Text(verbatim: unit).font(LucinaTokens.Fonts.subheadline.weight(.medium))
                    .foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
            }
            Text(verbatim: parts.value)
                .font(.system(size: size, weight: .heavy, design: .rounded).monospacedDigit())
                .foregroundStyle(LucinaTokens.Colors.text)
                .lineLimit(1).minimumScaleFactor(0.5)
                .widgetAccentable()
        }
    }

    /// "Network exam" and its date on the left, "days 15" on the right.
    private func row(_ event: CountdownEvent, figure size: CGFloat) -> some View {
        HStack(alignment: .center, spacing: LucinaTokens.Space.md) {
            VStack(alignment: .leading, spacing: 1) {
                Text(verbatim: event.title)
                    .font(LucinaTokens.Fonts.headline).foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(1).privacySensitive()
                Text(verbatim: Countdown.dateText(event))
                    .font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
            }
            Spacer(minLength: 0)
            let parts = Countdown.parts(event, now: entry.date)
            HStack(alignment: .lastTextBaseline, spacing: LucinaTokens.Space.xs) {
                if let unit = parts.unit {
                    Text(verbatim: unit).font(LucinaTokens.Fonts.caption.weight(.medium))
                        .foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
                }
                Text(verbatim: parts.value)
                    .font(.system(size: parts.value.allSatisfy(\.isNumber) ? size : size * 0.62, weight: .heavy, design: .rounded).monospacedDigit())
                    .foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(1).fixedSize()
                    .widgetAccentable()
            }
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var message: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else {
            WidgetMessage(symbol: "hourglass", text: "Pin an event in Secretary to count down to it.")
        }
    }

    private func label(_ event: CountdownEvent) -> String { Countdown.label(event, now: entry.date) }
}
