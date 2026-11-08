import SwiftUI
import WidgetKit

/// Ask Lucina: a field that opens the chat with the keyboard up, and her last reply.
struct AskLucinaWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<AskSection>

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: AskSection(fetchedAt: Date(), reply: "Your 10:30 is at the dentist. Leave at 10:05.", replyAt: Date()), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(AskSection.self, preview: context.isPreview) { fetcher, _ in try await fetcher.ask() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(AskSection.self) { fetcher, _ in try await fetcher.ask() }
                completion(Timeline(entries: [Entry(date: Date(), section: section, state: state)], policy: SectionLoader.policy(minutes: 30)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.ask.rawValue, provider: Provider()) { entry in
            AskLucinaWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Ask Lucina")
        .description("Ask Lucina something, and see her last reply.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct AskLucinaWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<AskSection>

    private var compose: URL { WidgetRoute.url("chat/compose") }

    var body: some View {
        switch family {
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                Image(systemName: "bubble.left.and.text.bubble.right.fill").font(.title3)
            }
            .widgetURL(compose)
            .accessibilityLabel("Ask Lucina")
        case .accessoryInline:
            Label("Ask Lucina", systemImage: "bubble.left.fill").widgetURL(compose)
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                Label("Lucina", systemImage: "bubble.left.fill").font(.headline).widgetAccentable()
                Text(entry.section?.reply ?? String(localized: "Tap to ask her anything."))
                    .font(.caption).lineLimit(2).privacySensitive()
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(compose)
        case .systemMedium:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                WidgetHeader(title: "Lucina", symbol: "sparkles", trailing: entry.section?.replyAt.map { WidgetFormat.dayTime($0) })
                reply(lines: 3)
                Spacer(minLength: 0)
                Link(destination: compose) { field }
            }
            .widgetURL(WidgetRoute.url("chat"))
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                WidgetHeader(title: "Lucina", symbol: "sparkles")
                reply(lines: 4)
                Spacer(minLength: 0)
                field
            }
            .widgetURL(compose)
        }
    }

    @ViewBuilder private func reply(lines: Int) -> some View {
        switch entry.state {
        case .signedOut:
            WidgetMessage.signedOut()
        default:
            if let text = entry.section?.reply {
                Text(text).font(LucinaTokens.Fonts.subheadline).foregroundStyle(LucinaTokens.Colors.text)
                    .lineLimit(lines).privacySensitive()
            } else {
                Text("Ask her anything: plans, lists, reminders.").font(LucinaTokens.Fonts.subheadline)
                    .foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(lines)
            }
        }
    }

    /// Looks like the chat's composer.
    private var field: some View {
        HStack(spacing: LucinaTokens.Space.xs) {
            Text("Ask Lucina…").font(LucinaTokens.Fonts.footnote).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
            Spacer(minLength: 0)
            Image(systemName: "arrow.up.circle.fill").font(.title3).foregroundStyle(LucinaTokens.Colors.accent).widgetAccentable()
        }
        .padding(.leading, LucinaTokens.Space.md)
        .padding(.trailing, LucinaTokens.Space.xs)
        .padding(.vertical, LucinaTokens.Space.xs)
        .background(Capsule().fill(LucinaTokens.Colors.field))
    }
}
