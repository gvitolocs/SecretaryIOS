import SwiftUI
import WidgetKit

/// This week's workouts against the weekly goal, and a button that starts a workout. The data
/// comes from the app (the training engine runs on the iPhone), published when it changes.
struct TrainingWidget: Widget {
    struct Provider: TimelineProvider {
        typealias Entry = SectionEntry<TrainingSection>

        func placeholder(in context: Context) -> Entry {
            let cal = Calendar.current
            let monday = cal.dateInterval(of: .weekOfYear, for: Date())?.start ?? Date()
            return Entry(date: Date(), section: TrainingSection(
                fetchedAt: Date(), weekCount: 2, weeklyGoal: 3,
                sessions: [.init(title: "Push", day: monday), .init(title: "Legs", day: monday.addingTimeInterval(2 * 86_400))],
                activeTitle: nil, activeSince: nil), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            completion(entry(context.isPreview ? placeholder(in: context).section : nil))
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            // A new week starts on the calendar's first weekday: refresh then, and at midnight.
            completion(Timeline(entries: [entry(nil)], policy: SectionLoader.policy(minutes: 180, boundary: SectionLoader.midnight)))
        }

        private func entry(_ fallback: TrainingSection?) -> Entry {
            let signedIn = BridgeCredentials.read() != nil
            var section = WidgetStore.read(TrainingSection.self) ?? fallback
            // Last week's count is not this week's.
            if let s = section, let week = Calendar.current.dateInterval(of: .weekOfYear, for: Date()), s.fetchedAt < week.start {
                section?.weekCount = 0
                section?.sessions = []
            }
            return Entry(date: Date(), section: signedIn || fallback != nil ? section : nil, state: signedIn || fallback != nil ? .ok : .signedOut)
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.training.rawValue, provider: Provider()) { entry in
            TrainingWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Training")
        .description("This week's workouts and your goal. Start a workout in one tap.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryCircular, .accessoryRectangular, .accessoryInline])
    }
}

struct TrainingWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: SectionEntry<TrainingSection>

    private var section: TrainingSection? { entry.section }
    private var count: Int { section?.weekCount ?? 0 }
    private var goal: Int { max(section?.weeklyGoal ?? 3, 1) }
    private var startURL: URL { WidgetRoute.url("gym/start") }

    var body: some View {
        switch family {
        case .accessoryCircular:
            Gauge(value: Double(min(count, goal)), in: 0...Double(goal)) {
                Image(systemName: "dumbbell.fill")
            } currentValueLabel: {
                Text("\(count)")
            }
            .gaugeStyle(.accessoryCircular)
            .widgetURL(startURL)
        case .accessoryInline:
            Text("\(Image(systemName: "dumbbell")) \(String(localized: "\(count) of \(goal) this week"))").widgetURL(startURL)
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 2) {
                Label(section?.activeTitle ?? String(localized: "Training"), systemImage: "dumbbell.fill").font(.headline).widgetAccentable()
                Text("\(count) of \(goal) this week").font(.caption)
                dots(size: 7)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(startURL)
        case .systemMedium:
            HStack(alignment: .top, spacing: LucinaTokens.Space.lg) {
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    WidgetHeader(title: "Training", symbol: "dumbbell.fill")
                    summary
                    Spacer(minLength: 0)
                    startButton
                }
                VStack(alignment: .leading, spacing: 3) {
                    Text("This week").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                    if let sessions = section?.sessions, !sessions.isEmpty {
                        ForEach(Array(sessions.suffix(4).enumerated()), id: \.offset) { _, session in
                            HStack(spacing: LucinaTokens.Space.xs) {
                                Text(session.day.formatted(.dateTime.weekday(.abbreviated))).font(LucinaTokens.Fonts.caption2.weight(.semibold))
                                    .foregroundStyle(LucinaTokens.Colors.accent).frame(width: 30, alignment: .leading)
                                Text(session.title).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
                            }
                        }
                    } else {
                        Text("No workouts yet.").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                    }
                    Spacer(minLength: 0)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .widgetURL(WidgetRoute.url("gym"))
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Training", symbol: "dumbbell.fill")
                summary
                Spacer(minLength: 0)
                startButton
            }
            .widgetURL(startURL)
        }
    }

    @ViewBuilder private var summary: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if section == nil {
            WidgetMessage(symbol: "dumbbell", text: "Open Training once to show your week here.")
        } else {
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                HStack(alignment: .firstTextBaseline, spacing: 2) {
                    Text("\(count)").font(LucinaTokens.Fonts.title).foregroundStyle(LucinaTokens.Colors.text)
                    Text("/ \(goal)").font(LucinaTokens.Fonts.subheadline).foregroundStyle(LucinaTokens.Colors.textSecondary)
                }
                dots(size: 9)
                if let active = section?.activeTitle {
                    Text("\(active) in progress").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.warning).lineLimit(1)
                }
            }
        }
    }

    private func dots(size: CGFloat) -> some View {
        HStack(spacing: 3) {
            ForEach(0..<min(goal, 7), id: \.self) { i in
                Circle().fill(i < count ? LucinaTokens.Colors.accent : LucinaTokens.Colors.field).frame(width: size, height: size)
            }
        }
        .widgetAccentable()
    }

    private var startButton: some View {
        Link(destination: startURL) {
            Label(section?.activeTitle == nil ? String(localized: "Start") : String(localized: "Resume"), systemImage: "play.fill")
                .font(LucinaTokens.Fonts.caption.weight(.bold)).foregroundStyle(LucinaTokens.Colors.onAccent)
                .padding(.horizontal, LucinaTokens.Space.md).padding(.vertical, LucinaTokens.Space.xs)
                .background(Capsule().fill(LucinaTokens.Colors.accentFill))
        }
        .widgetAccentable()
    }
}
