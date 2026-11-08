import SwiftUI
import WidgetKit

/// The next appointment: time, place, travel time and when to leave; tap for the in-app map.
struct AppointmentWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<AppointmentSection>

        func placeholder(in context: Context) -> Entry {
            let start = Date().addingTimeInterval(5400)
            return Entry(date: Date(), section: AppointmentSection(
                fetchedAt: Date(),
                next: .init(id: "sample", title: "Dentist", location: "Vesterbrogade 12", start: start,
                            end: start.addingTimeInterval(1800), allDay: false, travelMinutes: 18, travelMode: "transit"),
                then: nil), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(AppointmentSection.self, preview: context.isPreview) { f, _ in try await f.appointment() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(AppointmentSection.self) { f, _ in try await f.appointment() }
                let now = Date()
                // New entries when it is time to leave and when it starts, so the widget says so on time.
                var dates = [now]
                if let event = section?.next {
                    for date in [event.leaveAt, event.start].compactMap({ $0 }) where date > now { dates.append(date) }
                }
                let entries = dates.map { date in
                    Entry(date: date, section: section, state: state,
                          relevance: section?.next.map { TimelineEntryRelevance(score: $0.start.timeIntervalSince(date) < 3600 ? 80 : 20) })
                }
                completion(Timeline(entries: entries, policy: SectionLoader.policy(minutes: 30, boundary: section?.next?.end)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.appointment.rawValue, provider: Provider()) { entry in
            AppointmentWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Next appointment")
        .description("Your next appointment, with the travel time and when to leave.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct AppointmentWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<AppointmentSection>

    private var event: AppointmentSection.Event? { entry.section?.next }

    /// The in-app place map (`/map/place?…`, maps feature); it geocodes an address by itself.
    private var link: URL {
        guard let event else { return WidgetRoute.url("calendar") }
        guard let place = event.location, !place.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            var calendar = URLComponents()
            calendar.percentEncodedQuery = "event=" + WidgetRoute.segment(event.id)
                + "&at=" + String(format: "%.0f", event.start.timeIntervalSince1970 * 1000)
            return WidgetRoute.url("calendar?" + (calendar.percentEncodedQuery ?? ""))
        }
        var query = URLComponents()
        let iso = ISO8601DateFormatter()
        query.queryItems = [URLQueryItem(name: "title", value: event.title), URLQueryItem(name: "address", value: place),
                            URLQueryItem(name: "when", value: iso.string(from: event.start)),
                            URLQueryItem(name: "end", value: iso.string(from: event.end)), URLQueryItem(name: "eventId", value: event.id)]
        return WidgetRoute.url("map/place?\(query.percentEncodedQuery ?? "")")
    }

    var body: some View {
        content.widgetURL(link)
    }

    @ViewBuilder private var content: some View {
        switch family {
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                if let event {
                    VStack(spacing: 0) {
                        Image(systemName: "calendar").font(.caption2)
                        Text(event.allDay ? String(localized: "Day") : WidgetFormat.time(event.start))
                            .font(.system(size: 12, weight: .semibold)).minimumScaleFactor(0.6).lineLimit(1)
                    }
                } else {
                    Image(systemName: "calendar")
                }
            }
        case .accessoryInline:
            if let event {
                Text("\(Image(systemName: "calendar")) \(WidgetFormat.dayTime(event.start, allDay: event.allDay)) \(event.title)").privacySensitive()
            } else {
                Text("\(Image(systemName: "calendar")) No appointments")
            }
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                if let event {
                    Text(WidgetFormat.dayTime(event.start, allDay: event.allDay)).font(.headline).widgetAccentable()
                    Text(event.title).font(.caption).lineLimit(1).privacySensitive()
                    if let line = travelLine(event, short: true) {
                        Text(line).font(.caption2).lineLimit(1)
                    } else if let place = event.location {
                        Text(place).font(.caption2).lineLimit(1).privacySensitive()
                    }
                } else {
                    Label("Calendar", systemImage: "calendar").font(.headline).widgetAccentable()
                    Text("Nothing else today or tomorrow.").font(.caption).lineLimit(2)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .systemMedium:
            HStack(alignment: .top, spacing: LucinaTokens.Space.lg) {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    WidgetHeader(title: "Next up", symbol: "calendar")
                    main(lines: 2)
                }
                if let then = entry.section?.then, event != nil {
                    VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                        WidgetHeader(title: "Then", symbol: "arrow.turn.down.right", tint: LucinaTokens.Colors.textSecondary)
                        Text(WidgetFormat.dayTime(then.start, allDay: then.allDay)).font(LucinaTokens.Fonts.footnote.weight(.semibold))
                            .foregroundStyle(LucinaTokens.Colors.text)
                        Text(then.title).font(LucinaTokens.Fonts.footnote).foregroundStyle(LucinaTokens.Colors.textSecondary)
                            .lineLimit(3).privacySensitive()
                        Spacer(minLength: 0)
                    }
                    .frame(maxWidth: 110, alignment: .leading)
                }
            }
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Next up", symbol: "calendar")
                main(lines: 2)
            }
        }
    }

    @ViewBuilder private func main(lines: Int) -> some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if let event {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xxs) {
                Text(WidgetFormat.dayTime(event.start, allDay: event.allDay))
                    .font(LucinaTokens.Fonts.title3).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1).minimumScaleFactor(0.7)
                Text(event.title).font(LucinaTokens.Fonts.subheadline.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(lines).privacySensitive()
                if let place = event.location {
                    Label(place, systemImage: "mappin.and.ellipse").font(LucinaTokens.Fonts.caption2)
                        .foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1).privacySensitive()
                }
                Spacer(minLength: 0)
                if let line = travelLine(event, short: false) {
                    Label(line, systemImage: symbol(event.travelMode)).font(LucinaTokens.Fonts.caption)
                        .foregroundStyle(leaveSoon(event) ? LucinaTokens.Colors.warning : LucinaTokens.Colors.accent)
                        .lineLimit(1).minimumScaleFactor(0.8).widgetAccentable()
                } else if !event.allDay, event.start > entry.date {
                    Text(event.start, style: .relative).font(LucinaTokens.Fonts.caption).foregroundStyle(LucinaTokens.Colors.accent)
                } else if !event.allDay {
                    Text("Now · until \(WidgetFormat.time(event.end))").font(LucinaTokens.Fonts.caption)
                        .foregroundStyle(LucinaTokens.Colors.positive).lineLimit(1).widgetAccentable()
                }
            }
        } else if entry.state == .offline {
            WidgetMessage.offline()
        } else {
            WidgetMessage(symbol: "calendar", text: "Nothing else today or tomorrow.")
        }
    }

    private func leaveSoon(_ event: AppointmentSection.Event) -> Bool {
        guard let leave = event.leaveAt else { return false }
        return leave.timeIntervalSince(entry.date) < 15 * 60
    }

    /// "18 min · leave 10:05", "Leave now".
    private func travelLine(_ event: AppointmentSection.Event, short: Bool) -> String? {
        guard let minutes = event.travelMinutes, let leave = event.leaveAt else { return nil }
        if event.start <= entry.date { return String(localized: "Now") }
        if leave <= entry.date { return String(localized: "Leave now · \(minutes) min") }
        let time = WidgetFormat.time(leave)
        return short ? String(localized: "Leave \(time)") : String(localized: "\(minutes) min · leave \(time)")
    }

    private func symbol(_ mode: String?) -> String {
        switch mode {
        case "walk", "walking": return "figure.walk"
        case "transit": return "tram.fill"
        case "bike", "bicycling": return "bicycle"
        default: return "car.fill"
        }
    }
}
