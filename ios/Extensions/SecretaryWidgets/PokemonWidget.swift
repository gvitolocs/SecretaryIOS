import SwiftUI
import UIKit
import WidgetKit

/// Lucina's Pokémon Red run: where she is, badges, her team and the live screen.
struct PokemonWidget: Widget {
    struct Entry: TimelineEntry {
        var date: Date
        var section: PokemonSection?
        var state: LoadState
        var screen: UIImage?
    }

    struct Provider: TimelineProvider {
        func placeholder(in context: Context) -> Entry {
            Entry(date: Date(), section: PokemonSection(
                fetchedAt: Date(), running: true, place: "Viridian Forest", badges: 1, objective: "Reach Pewter City",
                party: [.init(name: "Charmeleon", level: 18, hp: 50, maxHp: 56), .init(name: "Pidgey", level: 9, hp: 20, maxHp: 28)],
                lastNote: "Caught a Pidgey."), state: .ok)
        }

        func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
            Task {
                var entry = await load(screen: context.family != .accessoryRectangular && context.family != .accessoryInline)
                if context.isPreview, entry.section == nil { entry = placeholder(in: context) }
                completion(entry)
            }
        }

        func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
            Task {
                let entry = await load(screen: context.family == .systemMedium || context.family == .systemLarge)
                // While she plays the place changes often; paused, rarely.
                completion(Timeline(entries: [entry], policy: SectionLoader.policy(minutes: entry.section?.running == true ? 15 : 60)))
            }
        }

        private func load(screen: Bool) async -> Entry {
            let (section, state) = await SectionLoader.load(PokemonSection.self) { f, _ in try await f.pokemon() }
            var image: UIImage?
            if screen, state == .ok, let fetcher = WidgetFetcher.fromBridge() {
                let url = (try? await fetcher.pokemonScreen()) ?? WidgetStore.imageURL("pokemon-screen.png")
                image = url.flatMap { UIImage(contentsOfFile: $0.path) }
            } else if screen, let url = WidgetStore.imageURL("pokemon-screen.png") {
                image = UIImage(contentsOfFile: url.path)
            }
            return Entry(date: Date(), section: section, state: state, screen: image)
        }
    }

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetKind.pokemon.rawValue, provider: Provider()) { entry in
            PokemonWidgetView(entry: entry).lucinaWidgetBackground()
        }
        .configurationDisplayName("Lucina plays Pokémon")
        .description("Where Lucina is in Pokémon Red, her badges and her team.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .accessoryRectangular, .accessoryInline])
    }
}

struct PokemonWidgetView: View {
    @Environment(\.widgetFamily) private var environmentFamily
    /// Set by the app's widget gallery (screenshots); WidgetKit's own family otherwise.
    var familyOverride: WidgetFamily? = nil
    private var family: WidgetFamily { familyOverride ?? environmentFamily }
    let entry: PokemonWidget.Entry

    private var section: PokemonSection? { entry.section }
    private let url = WidgetRoute.url("pokemon")

    var body: some View {
        Group {
            switch family {
            case .accessoryInline:
                Text("\(Image(systemName: "gamecontroller")) \(section?.place ?? "Pokémon") · \(section?.badges ?? 0)🏅")
            case .accessoryRectangular:
                VStack(alignment: .leading, spacing: 1) {
                    Label(section?.place ?? "Pokémon", systemImage: "gamecontroller.fill").font(.headline).lineLimit(1).widgetAccentable()
                    Text("\(section?.badges ?? 0) badges · \(section?.running == true ? String(localized: "playing") : String(localized: "paused"))")
                        .font(.caption)
                    if let lead = section?.party.first { Text("\(lead.name) Lv \(lead.level)").font(.caption2).lineLimit(1) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            case .systemSmall:
                VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                    header
                    status
                    Spacer(minLength: 0)
                    team(limit: 2)
                }
            case .systemMedium:
                HStack(spacing: LucinaTokens.Space.md) {
                    screen.frame(width: 120)
                    VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                        header
                        status
                        Spacer(minLength: 0)
                        team(limit: 2)
                    }
                }
            default:
                VStack(alignment: .leading, spacing: LucinaTokens.Space.sm) {
                    header
                    HStack(alignment: .top, spacing: LucinaTokens.Space.md) {
                        screen.frame(width: 150)
                        VStack(alignment: .leading, spacing: LucinaTokens.Space.xs) {
                            status
                            if let objective = section?.objective {
                                Label(objective, systemImage: "flag.fill").font(LucinaTokens.Fonts.caption2)
                                    .foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(3)
                            }
                        }
                    }
                    team(limit: 6)
                    if let note = section?.lastNote {
                        Text(note).font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary).lineLimit(2)
                    }
                    Spacer(minLength: 0)
                }
            }
        }
        .widgetURL(url)
    }

    private var header: some View {
        WidgetHeader(title: "Pokémon", symbol: "gamecontroller.fill",
                     tint: section?.running == true ? LucinaTokens.Colors.positive : LucinaTokens.Colors.accent,
                     trailing: section.map { $0.running ? String(localized: "Playing") : String(localized: "Paused") })
    }

    @ViewBuilder private var status: some View {
        if entry.state == .signedOut {
            WidgetMessage.signedOut()
        } else if let section {
            VStack(alignment: .leading, spacing: 2) {
                Text(section.place).font(LucinaTokens.Fonts.headline).foregroundStyle(LucinaTokens.Colors.text).lineLimit(2).minimumScaleFactor(0.8)
                HStack(spacing: 2) {
                    ForEach(0..<8, id: \.self) { i in
                        Image(systemName: i < section.badges ? "seal.fill" : "seal").font(.system(size: 9))
                            .foregroundStyle(i < section.badges ? LucinaTokens.Colors.warning : LucinaTokens.Colors.textTertiary)
                    }
                }
            }
        } else {
            WidgetMessage(symbol: "gamecontroller", text: "The game isn't reachable right now.")
        }
    }

    private func team(limit: Int) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            ForEach(Array((section?.party ?? []).prefix(limit).enumerated()), id: \.offset) { _, mon in
                HStack(spacing: LucinaTokens.Space.xs) {
                    Text(mon.name).font(LucinaTokens.Fonts.caption2.weight(.semibold)).foregroundStyle(LucinaTokens.Colors.text).lineLimit(1)
                    Spacer(minLength: 2)
                    Text("Lv \(mon.level)").font(LucinaTokens.Fonts.caption2).foregroundStyle(LucinaTokens.Colors.textSecondary)
                }
                if mon.maxHp > 0 {
                    let frac = Double(mon.hp) / Double(mon.maxHp)
                    ProgressView(value: min(max(frac, 0), 1))
                        .tint(frac > 0.5 ? LucinaTokens.Colors.positive : frac > 0.2 ? LucinaTokens.Colors.warning : LucinaTokens.Colors.negative)
                        .scaleEffect(x: 1, y: 0.6, anchor: .center)
                }
            }
        }
    }

    @ViewBuilder private var screen: some View {
        ZStack {
            RoundedRectangle(cornerRadius: LucinaTokens.Radius.field).fill(Color.black)
            if let image = entry.screen {
                Image(uiImage: image).interpolation(.none).resizable().aspectRatio(160.0 / 144.0, contentMode: .fit)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .padding(LucinaTokens.Space.xs)
            } else {
                Image(systemName: "gamecontroller").foregroundStyle(.gray)
            }
        }
        .aspectRatio(160.0 / 144.0, contentMode: .fit)
    }
}
