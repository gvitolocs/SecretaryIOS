import SwiftUI
import WidgetKit

/// The homelab at a glance (the Server plugin): overall status, how many things need attention, the first of them.
struct ServerWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<ServerSection>

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: ServerSection(fetchedAt: Date(), overall: "warn", attentionCount: 2,
                                                       titles: ["gmaps-server", "marketplace-rails-sync"], generatedAt: Date()), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(ServerSection.self, preview: context.isPreview) { f, _ in try await f.server() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(ServerSection.self) { f, _ in try await f.server() }
                completion(Timeline(entries: [Entry(date: Date(), section: section, state: state)], policy: SectionLoader.policy(minutes: 15)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.server.rawValue, provider: Provider()) { entry in
            ServerWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Server")
        .description("Hosts and services at a glance, and what needs attention.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular])
    }
}

struct ServerWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<ServerSection>

    private var section: ServerSection? { entry.section }
    private var url: URL { WidgetRoute.url("server-health") }

    private var color: Color {
        switch section?.overall ?? "" {
        case "ok": LucinaTokens.Colors.positive
        case "warn": LucinaTokens.Colors.warning
        case "crit": LucinaTokens.Colors.negative
        default: LucinaTokens.Colors.textTertiary
        }
    }

    /// "All systems normal", "2 need attention", or why there is nothing to show.
    private var status: String {
        guard let section else {
            return entry.state == .offline ? String(localized: "Server unreachable") : String(localized: "No data yet")
        }
        return section.attentionCount == 0 ? String(localized: "All systems normal") : String(localized: "\(section.attentionCount) need attention")
    }

    private var updated: String? {
        guard let date = section?.generatedAt ?? section?.fetchedAt else { return nil }
        return String(localized: "Updated \(WidgetFormat.time(date))")
    }

    var body: some View {
        switch family {
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                Label(String(localized: "Server"), systemImage: "server.rack").font(.headline).widgetAccentable()
                Text(status).font(.caption).lineLimit(1)
                if let first = section?.titles.first {
                    Text(first).font(.caption).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(url)
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Server", symbol: "server.rack")
                if entry.state == .signedOut {
                    WidgetMessage.signedOut()
                } else {
                    HStack(alignment: .firstTextBaseline, spacing: LucinaTokens.Space.sm) {
                        Circle().fill(color).frame(width: 10, height: 10).accessibilityHidden(true)
                        Text(status)
                            .font(family == .systemSmall ? LucinaTokens.Fonts.headline : LucinaTokens.Fonts.title3)
                            .foregroundStyle(LucinaTokens.Colors.text)
                            .lineLimit(2)
                            .minimumScaleFactor(0.8)
                    }
                    if family == .systemMedium, let titles = section?.titles, !titles.isEmpty {
                        VStack(alignment: .leading, spacing: 3) {
                            ForEach(Array(titles.prefix(2).enumerated()), id: \.offset) { _, title in
                                Label(title, systemImage: "exclamationmark.circle")
                                    .font(LucinaTokens.Fonts.subheadline)
                                    .foregroundStyle(LucinaTokens.Colors.text)
                                    .lineLimit(1)
                            }
                        }
                    }
                    Spacer(minLength: 0)
                    if let updated {
                        Text(updated).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .widgetURL(url)
        }
    }
}
