import SwiftUI
import WidgetKit

// The pieces every widget shares: the entry, loading a section (cache first, then the server),
// and the Lucina look (design/tokens.json via LucinaTokens).

enum LoadState: Equatable {
    case ok
    /// The app is signed out: the shared keychain has no token.
    case signedOut
    /// The server could not be reached; the entry shows the last copy, if any.
    case offline
}

struct SectionEntry<S: WidgetSection>: TimelineEntry {
    var date: Date
    var section: S?
    var state: LoadState
    var relevance: TimelineEntryRelevance? = nil
}

enum SectionLoader {
    /// A section younger than this is used as is (the app just pushed it, or a check-off just
    /// changed it), so a reload right after a change does not fetch again.
    static let freshFor: TimeInterval = 60

    static func load<S: WidgetSection>(_ type: S.Type, preview: Bool = false,
                                       fetch: @escaping @Sendable (WidgetFetcher, S?) async throws -> S) async -> (S?, LoadState) {
        let cached = WidgetStore.read(S.self)
        guard let fetcher = WidgetFetcher.fromBridge() else { return (preview ? cached : nil, .signedOut) }
        if let cached, Date().timeIntervalSince(cached.fetchedAt) < freshFor { return (cached, .ok) }
        do {
            let fresh = try await fetch(fetcher, cached)
            WidgetStore.write(fresh)
            return (WidgetStore.read(S.self) ?? fresh, .ok)
        } catch {
            return (cached, .offline)
        }
    }

    /// The next refresh: `minutes` from now, or earlier at `boundary` (a start time, midnight).
    static func policy(minutes: Double, boundary: Date? = nil) -> TimelineReloadPolicy {
        var next = Date().addingTimeInterval(minutes * 60)
        if let boundary, boundary > Date(), boundary < next { next = boundary }
        return .after(next)
    }

    static var midnight: Date { Calendar.current.startOfDay(for: Date()).addingTimeInterval(86_400 + 60) }
}

// MARK: - Look

extension View {
    /// The widget's card background (dropped by StandBy and tinted Home Screens).
    func lucinaWidgetBackground() -> some View {
        containerBackground(for: .widget) { LucinaTokens.Colors.card }
    }
}

/// The small header row: an SF Symbol and a title, in the accent colour.
struct WidgetHeader: View {
    let title: LocalizedStringKey
    let symbol: String
    var tint: Color = LucinaTokens.Colors.accent
    var trailing: String? = nil

    var body: some View {
        HStack(spacing: LucinaTokens.Space.xs) {
            Image(systemName: symbol).font(.caption.weight(.bold)).foregroundStyle(tint)
            Text(title).font(LucinaTokens.Fonts.caption).foregroundStyle(tint).lineLimit(1)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(1)
            }
        }
        .widgetAccentable()
    }
}

/// What a widget shows without data: signed out, offline with nothing cached, or simply empty.
struct WidgetMessage: View {
    let symbol: String
    let text: LocalizedStringKey

    var body: some View {
        VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
            Image(systemName: symbol).font(.title3).foregroundStyle(LucinaTokens.Colors.textTertiary)
            Text(text).font(LucinaTokens.Fonts.footnote).foregroundStyle(LucinaTokens.Colors.textSecondary)
                .multilineTextAlignment(.leading).lineLimit(3)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
    }

    static func signedOut() -> WidgetMessage { WidgetMessage(symbol: "person.crop.circle.badge.questionmark", text: "Open Secretary and sign in.") }
    static func offline() -> WidgetMessage { WidgetMessage(symbol: "wifi.slash", text: "Lucina is unreachable. Showing nothing until she's back.") }
}

/// A round "+" or action button (a link into the app).
struct CircleAction: View {
    let symbol: String
    var size: CGFloat = 30

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size * 0.45, weight: .bold))
            .foregroundStyle(LucinaTokens.Colors.onAccent)
            .frame(width: size, height: size)
            .background(Circle().fill(LucinaTokens.Colors.accentFill))
            .widgetAccentable()
    }
}

/// A tiny line chart (the weight trend), scaled to its own min and max.
struct Sparkline: View {
    @Environment(\.redactionReasons) private var redaction
    let values: [Double]
    var color: Color = LucinaTokens.Colors.accent

    var body: some View {
        GeometryReader { geo in
            // A shape is not redacted by itself: locked, the trend is a flat placeholder line.
            if redaction.contains(.privacy) {
                Capsule().fill(color.opacity(0.3)).frame(height: 3).frame(maxHeight: .infinity)
            } else if values.count >= 2, let low = values.min(), let high = values.max() {
                let span = max(high - low, 0.1)
                let step = geo.size.width / CGFloat(values.count - 1)
                let point = { (i: Int) in
                    CGPoint(x: CGFloat(i) * step, y: geo.size.height * (1 - CGFloat((values[i] - low) / span)))
                }
                Path { path in
                    path.move(to: point(0))
                    for i in values.indices.dropFirst() { path.addLine(to: point(i)) }
                }
                .stroke(color, style: StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
                Circle().fill(color).frame(width: 5, height: 5).position(point(values.count - 1))
            }
        }
        .widgetAccentable()
    }
}

/// Times as the phone shows them ("14:30" or "2:30 PM").
enum WidgetFormat {
    static func time(_ date: Date) -> String { date.formatted(date: .omitted, time: .shortened) }

    /// "14:30", "Tomorrow 09:00", "Wed 09:00".
    static func dayTime(_ date: Date, allDay: Bool = false) -> String {
        let cal = Calendar.current
        let day: String? = cal.isDateInToday(date) ? nil
            : cal.isDateInTomorrow(date) ? String(localized: "Tomorrow")
            : date.formatted(.dateTime.weekday(.abbreviated))
        if allDay { return day ?? String(localized: "Today") }
        return [day, time(date)].compactMap { $0 }.joined(separator: " ")
    }

    static func price(_ dkk: Double?) -> String? {
        dkk.map { $0.formatted(.number.precision(.fractionLength(0...2))) + " kr" }
    }
}
