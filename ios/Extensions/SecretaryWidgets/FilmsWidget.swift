import SwiftUI
import UIKit
import WidgetKit

/// Films: continue watching where you stopped, on any device.
struct FilmsWidget: Widget {
    struct Entry: TimelineEntry {
        var date: Date
        var section: FilmsSection?
        var state: LoadState
        var posters: [String: UIImage] = [:]
    }

    struct Provider: TimelineProvider {
        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: FilmsSection(fetchedAt: Date(), films: [
                .init(id: "sample", title: "Babettes gæstebud", positionMs: 3_100_000, durationMs: 6_200_000, updatedAt: Date(), posterPath: nil),
            ]), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                var entry = await load(posters: true)
                if context.isPreview, entry.section?.films.isEmpty ?? true { entry = placeholder(in: context) }
                completion(entry)
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let entry = await load(posters: context.family == .systemSmall || context.family == .systemMedium)
                completion(Timeline(entries: [entry], policy: SectionLoader.policy(minutes: 120)))
            }
        }

        private func load(posters: Bool) async -> Entry {
            let (section, state) = await SectionLoader.load(FilmsSection.self) { f, _ in try await f.films() }
            var images: [String: UIImage] = [:]
            if posters, let section, let fetcher = WidgetFetcher.fromBridge() {
                for film in section.films.prefix(3) {
                    if let url = await fetcher.poster(film), let image = UIImage(contentsOfFile: url.path) { images[film.id] = image }
                }
            }
            return Entry(date: Date(), section: section, state: state, posters: images)
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.films.rawValue, provider: Provider()) { entry in
            FilmsWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Continue watching")
        .description("Pick up the film you were watching where you stopped.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular])
    }
}

struct FilmsWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: FilmsWidget.Entry

    private var films: [FilmsSection.Film] { entry.section?.films ?? [] }

    private func url(_ film: FilmsSection.Film?) -> URL {
        film.map { WidgetRoute.url("films/play/\(WidgetRoute.segment($0.id))") } ?? WidgetRoute.url("films")
    }

    var body: some View {
        switch family {
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 2) {
                Label("Continue", systemImage: "film.fill").font(.headline).widgetAccentable()
                if let film = films.first {
                    Text(film.title).font(.caption).lineLimit(1)
                    ProgressView(value: film.progress)
                } else {
                    Text("Nothing in progress").font(.caption)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .widgetURL(url(films.first))
        case .systemMedium:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                WidgetHeader(title: "Continue watching", symbol: "film.fill")
                if films.isEmpty {
                    empty
                } else {
                    HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
                        ForEach(films.prefix(3)) { film in
                            Link(destination: url(film)) { tile(film) }
                        }
                        Spacer(minLength: 0)
                    }
                }
            }
            .widgetURL(url(nil))
        default:
            VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                WidgetHeader(title: "Films", symbol: "film.fill")
                if let film = films.first {
                    poster(film).frame(maxHeight: .infinity)
                    Text(film.title).font(LucinaTokens.Fonts.caption.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
                    ProgressView(value: film.progress).tint(LucinaTokens.Colors.accent)
                } else {
                    empty
                }
            }
            .widgetURL(url(films.first))
        }
    }

    @ViewBuilder private var empty: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else {
            WidgetMessage(symbol: "film", text: entry.state == .offline ? "The library is unreachable." : "Nothing in progress. Start a film in Films.")
        }
    }

    private func tile(_ film: FilmsSection.Film) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            poster(film).frame(width: 84, height: 70)
            Text(film.title).font(LucinaTokens.Fonts.caption2.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
            ProgressView(value: film.progress).tint(LucinaTokens.Colors.accent)
            Text("\(film.remainingMinutes) min left").font(.system(size: 9)).foregroundStyle(LucinaTokens.Colors.textSecondary)
        }
        .frame(width: 84)
    }

    private func poster(_ film: FilmsSection.Film) -> some View {
        ZStack {
            RoundedRectangle(cornerRadius: LucinaTokens.Radius.field).fill(LucinaTokens.Colors.cardRaised)
            if let image = entry.posters[film.id] {
                Image(uiImage: image).resizable().aspectRatio(contentMode: .fill)
            } else {
                Image(systemName: "film").font(.title2).foregroundStyle(LucinaTokens.Colors.textTertiary)
            }
            Image(systemName: "play.circle.fill").font(.title2).foregroundStyle(.white.opacity(0.9)).shadow(radius: 3)
        }
        .clipShape(RoundedRectangle(cornerRadius: LucinaTokens.Radius.field))
    }
}
