import SwiftUI
import WidgetKit

/// Today's tasks (Todoist, via Lucina) and Lucina's reminders, completed right on the widget.
struct TasksWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<TasksSection>

        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: TasksSection(fetchedAt: Date(), items: [
                .init(id: "1", kind: .task, title: "Call the bank", due: nil, overdue: true, priority: 1),
                .init(id: "2", kind: .reminder, title: "Take the laundry out", due: Date().addingTimeInterval(3600), overdue: false, priority: 4),
                .init(id: "3", kind: .task, title: "Book the car service", due: nil, overdue: false, priority: 3),
            ], completedToday: 2), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(TasksSection.self, preview: context.isPreview) { f, _ in try await f.tasks() }
                completion(Entry(date: Date(), section: section ?? placeholder(in: context).section, state: context.isPreview ? .ok : state))
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let (section, state) = await SectionLoader.load(TasksSection.self) { f, _ in try await f.tasks() }
                let nextDue = section?.items.compactMap(\.due).filter { $0 > Date() }.min()
                completion(Timeline(entries: [Entry(date: Date(), section: section, state: state)],
                                    policy: SectionLoader.policy(minutes: 30, boundary: nextDue ?? SectionLoader.midnight)))
            }
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.tasks.rawValue, provider: Provider()) { entry in
            TasksWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Today")
        .description("Tasks and reminders due today. Tick them off without opening the app.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct TasksWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<TasksSection>

    private var items: [TasksSection.Item] { entry.section?.items ?? [] }
    private var url: URL { WidgetRoute.url("tasks") }

    var body: some View {
        switch family {
        case .accessoryCircular:
            Gauge(value: 0) {
                Image(systemName: "checklist")
            } currentValueLabel: {
                Text("\(items.count)")
            }
            .gaugeStyle(.accessoryCircularCapacity)
            .widgetURL(url)
        case .accessoryInline:
            Text("\(Image(systemName: "checklist")) \(String(localized: "\(items.count) due today"))").widgetURL(url)
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                Label(String(localized: "\(items.count) due today"), systemImage: "checklist").font(.headline).widgetAccentable()
                ForEach(items.prefix(2)) { item in
                    Text(item.title).font(.caption).lineLimit(1).privacySensitive()
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(url)
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Today", symbol: "checklist",
                             trailing: (entry.section?.completedToday ?? 0) > 0 ? String(localized: "\(entry.section?.completedToday ?? 0) done") : nil)
                list(limit: family == .systemLarge ? 8 : family == .systemMedium ? 4 : 3, compact: family == .systemSmall)
            }
            .widgetURL(url)
        }
    }

    @ViewBuilder private func list(limit: Int, compact: Bool) -> some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if items.isEmpty {
            WidgetMessage(symbol: entry.state == .offline ? "wifi.slash" : "checkmark.seal",
                          text: entry.state == .offline ? "Lucina is unreachable." : "All done for today.")
        } else {
            VStack(alignment: .leading, spacing: compact ? 4 : 7) {
                ForEach(items.prefix(limit)) { item in row(item, compact: compact) }
                if items.count > limit {
                    Text("+\(items.count - limit) more").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                }
            }
            Spacer(minLength: 0)
        }
    }

    private func row(_ item: TasksSection.Item, compact: Bool) -> some View {
        Button(intent: CompleteTaskIntent(item)) {
            HStack(alignment: .firstTextBaseline, spacing: LucinaTokens.Space.sm) {
                Image(systemName: item.kind == .reminder ? "bell.circle" : "circle")
                    .font(compact ? .footnote : .body).foregroundStyle(color(item)).widgetAccentable()
                VStack(alignment: .leading, spacing: 0) {
                    Text(item.title).font(compact ? LucinaTokens.Fonts.footnote : LucinaTokens.Fonts.subheadline)
                        .foregroundStyle(LucinaTokens.Colors.text).lineLimit(compact ? 1 : 2).privacySensitive()
                    if !compact, let due = item.due {
                        Text(WidgetFormat.time(due)).font(LucinaTokens.Fonts.caption2)
                            .foregroundStyle(item.overdue ? LucinaTokens.Colors.negative : LucinaTokens.Colors.textSecondary)
                    }
                }
                Spacer(minLength: 0)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text("Complete \(item.title)"))
    }

    /// Todoist's priority colours, as in the Tasks screen (TaskColors).
    private func color(_ item: TasksSection.Item) -> Color {
        switch item.priority {
        case 1: return LucinaTokens.Colors.negative
        case 2: return LucinaTokens.Colors.warning
        case 3: return LucinaTokens.Colors.accent
        default: return LucinaTokens.Colors.textSecondary
        }
    }
}
