import SwiftUI
import WidgetKit

/// One tap to record a voice memo: the memos feature's StartMemoRecordingIntent (an
/// AudioRecordingIntent) records in the background with its Live Activity, without opening the app.
struct MemoWidget: Widget {
    struct Provider: TimelineProvider {
        struct Entry: TimelineEntry { let date: Date }

        func placeholder(in context: Context) -> Entry { Entry(date: Date()) }
        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) { completion(Entry(date: Date())) }
        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            completion(Timeline(entries: [Entry(date: Date())], policy: .never))
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.memo.rawValue, provider: Provider()) { _ in
            MemoWidgetView().lucinaWidgetBackground()
        }
        .configurationDisplayName("Voice memo")
        .description("Start recording a voice memo in one tap. Lucina transcribes and summarises it.")
        .supportedFamilies([.systemSmall, .accessoryCircular, .accessoryInline])
    }
}

struct MemoWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    private let url = WidgetRoute.url("memos/record")

    var body: some View {
        switch family {
        case .accessoryCircular:
            Button(intent: StartMemoRecordingIntent()) {
                ZStack {
                    AccessoryWidgetBackground()
                    Image(systemName: "mic.fill").font(.title3)
                }
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Record a memo")
        case .accessoryInline:
            Label("Record memo", systemImage: "mic.fill").widgetURL(url)
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                WidgetHeader(title: "Voice memo", symbol: "waveform", tint: LucinaTokens.Colors.negative)
                Spacer(minLength: 0)
                Button(intent: StartMemoRecordingIntent()) {
                HStack {
                    Spacer()
                    ZStack {
                        Circle().fill(LucinaTokens.Colors.negative.opacity(0.18)).frame(width: 74, height: 74)
                        Circle().fill(LucinaTokens.Colors.negative).frame(width: 56, height: 56)
                        Image(systemName: "mic.fill").font(.title2.weight(.semibold)).foregroundStyle(.white)
                    }
                    .widgetAccentable()
                    Spacer()
                }
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Record a memo")
                Spacer(minLength: 0)
                Text("Tap to record").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                    .frame(maxWidth: .infinity)
            }
            .widgetURL(url)
        }
    }
}
