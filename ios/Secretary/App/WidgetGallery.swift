#if DEBUG || SCREENSHOTS
import SwiftUI
import UIKit
import WidgetKit

/// `-SecretaryWidgetGallery <page>`: the widgets' own views (Extensions/SecretaryWidgets), drawn
/// in the app at their Home Screen and Lock Screen sizes from the cached sections, so a device
/// screenshot shows them without placing them by hand. Run `-SecretaryWidgetSelfTest YES` first
/// to fill the cache. Pages: 0 Ask + appointment, 1 shopping, 2 weight + training,
/// 3 tasks + countdown, 4 Pokémon, 5 films + memo, 6 Lock Screen, 7 Lock Screen while locked (privacy redaction).
struct WidgetGallery: View {
    let page: Int

    // iPhone 16e (390 pt wide) widget sizes.
    private static let small = CGSize(width: 158, height: 158)
    private static let medium = CGSize(width: 338, height: 158)
    private static let large = CGSize(width: 338, height: 354)
    private static let circular = CGSize(width: 72, height: 72)
    private static let rectangular = CGSize(width: 160, height: 72)
    private static let inline = CGSize(width: 300, height: 22)

    var body: some View {
        ScrollView { sheet }
            .background(Color(white: page >= 6 ? 0.08 : 0.55).ignoresSafeArea())
    }

    /// The page itself, without the scroll view (also what `export` renders).
    private var sheet: some View {
        VStack(spacing: 16) {
            Text("Widget gallery · page \(page)").font(.caption).foregroundStyle(.secondary)
            content
        }
        .padding(.vertical, 24)
        .frame(maxWidth: .infinity)
    }

    /// `-SecretaryWidgetGalleryExport YES`: every page rendered to Documents/widget-gallery-<n>.png
    /// (390 pt wide, @3x, dark), for `devicectl device copy from` when no USB screenshot is possible.
    static func exportIfRequested() {
        guard UserDefaults.standard.bool(forKey: "SecretaryWidgetGalleryExport"),
              let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        for page in 0...7 {
            let view = WidgetGallery(page: page).sheet
                .frame(width: 390)
                .background(Color(white: page >= 6 ? 0.08 : 0.55))
                .environment(\.colorScheme, .dark)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 3
            if let data = renderer.uiImage?.pngData() {
                try? data.write(to: docs.appendingPathComponent("widget-gallery-\(page).png"), options: .atomic)
            }
        }
        try? "done\n".write(to: docs.appendingPathComponent("widget-gallery-done.txt"), atomically: true, encoding: .utf8)
    }

    @ViewBuilder private var content: some View {
        let now = Date()
        switch page {
        case 0:
            let ask = SectionEntry(date: now, section: WidgetStore.read(AskSection.self), state: .ok)
            let next = SectionEntry(date: now, section: WidgetStore.read(AppointmentSection.self), state: .ok)
            HStack(spacing: 22) {
                home(Self.small) { AskLucinaWidgetView(familyOverride: .systemSmall, entry: ask) }
                home(Self.small) { AppointmentWidgetView(familyOverride: .systemSmall, entry: next) }
            }
            home(Self.medium) { AskLucinaWidgetView(familyOverride: .systemMedium, entry: ask) }
            home(Self.medium) { AppointmentWidgetView(familyOverride: .systemMedium, entry: next) }
        case 1:
            let shopping = SectionEntry(date: now, section: WidgetStore.read(ShoppingSection.self), state: .ok)
            home(Self.medium) { ShoppingWidgetView(familyOverride: .systemMedium, entry: shopping) }
            home(Self.large) { ShoppingWidgetView(familyOverride: .systemLarge, entry: shopping) }
        case 2:
            let weight = SectionEntry(date: now, section: WidgetStore.read(WeightSection.self), state: .ok)
            let training = SectionEntry(date: now, section: WidgetStore.read(TrainingSection.self), state: .ok)
            HStack(spacing: 22) {
                home(Self.small) { WeightWidgetView(familyOverride: .systemSmall, entry: weight) }
                home(Self.small) { TrainingWidgetView(familyOverride: .systemSmall, entry: training) }
            }
            home(Self.medium) { WeightWidgetView(familyOverride: .systemMedium, entry: weight) }
            home(Self.medium) { TrainingWidgetView(familyOverride: .systemMedium, entry: training) }
        case 3:
            let tasks = SectionEntry(date: now, section: WidgetStore.read(TasksSection.self), state: .ok)
            HStack(spacing: 22) {
                home(Self.small) { TasksWidgetView(familyOverride: .systemSmall, entry: tasks) }
                home(Self.small) { ShoppingWidgetView(familyOverride: .systemSmall,
                                                      entry: SectionEntry(date: now, section: WidgetStore.read(ShoppingSection.self), state: .ok)) }
            }
            home(Self.large) { TasksWidgetView(familyOverride: .systemLarge, entry: tasks) }
            HStack(spacing: 22) {
                home(Self.small) { CountdownWidgetView(familyOverride: .systemSmall, entry: Self.countdown(now)) }
                home(Self.small) { CountdownWidgetView(familyOverride: .systemSmall, entry: CountdownWidget.Entry(date: now, events: [])) }
            }
            home(Self.medium) { CountdownWidgetView(familyOverride: .systemMedium, entry: Self.countdown(now)) }
        case 4:
            let screen = WidgetStore.imageURL("pokemon-screen.png").flatMap { UIImage(contentsOfFile: $0.path) }
            let pokemon = PokemonWidget.Entry(date: now, section: WidgetStore.read(PokemonSection.self), state: .ok, screen: screen)
            home(Self.medium) { PokemonWidgetView(familyOverride: .systemMedium, entry: pokemon) }
            home(Self.large) { PokemonWidgetView(familyOverride: .systemLarge, entry: pokemon) }
        case 5:
            let section = WidgetStore.read(FilmsSection.self)
            let films = FilmsWidget.Entry(date: now, section: section, state: .ok, posters: Self.posters(section))
            HStack(spacing: 22) {
                home(Self.small) { FilmsWidgetView(familyOverride: .systemSmall, entry: films) }
                home(Self.small) { MemoWidgetView(familyOverride: .systemSmall) }
            }
            home(Self.medium) { FilmsWidgetView(familyOverride: .systemMedium, entry: films) }
            HStack(spacing: 22) {
                home(Self.small) { PokemonWidgetView(familyOverride: .systemSmall, entry: PokemonWidget.Entry(
                    date: now, section: WidgetStore.read(PokemonSection.self), state: .ok)) }
                home(Self.small) { TasksWidgetView(familyOverride: .systemSmall,
                                                   entry: SectionEntry(date: now, section: WidgetStore.read(TasksSection.self), state: .signedOut)) }
            }
        default:
            lockScreen(redacted: page >= 7)
        }
    }

    @ViewBuilder private func lockScreen(redacted: Bool) -> some View {
        let now = Date()
        let ask = SectionEntry(date: now, section: WidgetStore.read(AskSection.self), state: .ok)
        let next = SectionEntry(date: now, section: WidgetStore.read(AppointmentSection.self), state: .ok)
        let shopping = SectionEntry(date: now, section: WidgetStore.read(ShoppingSection.self), state: .ok)
        let weight = SectionEntry(date: now, section: WidgetStore.read(WeightSection.self), state: .ok)
        let training = SectionEntry(date: now, section: WidgetStore.read(TrainingSection.self), state: .ok)
        let tasks = SectionEntry(date: now, section: WidgetStore.read(TasksSection.self), state: .ok)
        let pokemon = PokemonWidget.Entry(date: now, section: WidgetStore.read(PokemonSection.self), state: .ok)
        let films = FilmsWidget.Entry(date: now, section: WidgetStore.read(FilmsSection.self), state: .ok)
        let countdown = Self.countdown(now)
        Group {
            lock(Self.inline) { AppointmentWidgetView(familyOverride: .accessoryInline, entry: next) }
            lock(Self.inline) { WeightWidgetView(familyOverride: .accessoryInline, entry: weight) }
            HStack(spacing: 10) {
                lock(Self.circular) { AskLucinaWidgetView(familyOverride: .accessoryCircular, entry: ask) }
                lock(Self.circular) { AppointmentWidgetView(familyOverride: .accessoryCircular, entry: next) }
                lock(Self.circular) { ShoppingWidgetView(familyOverride: .accessoryCircular, entry: shopping) }
                lock(Self.circular) { WeightWidgetView(familyOverride: .accessoryCircular, entry: weight) }
            }
            HStack(spacing: 10) {
                lock(Self.circular) { TrainingWidgetView(familyOverride: .accessoryCircular, entry: training) }
                lock(Self.circular) { TasksWidgetView(familyOverride: .accessoryCircular, entry: tasks) }
                lock(Self.circular) { MemoWidgetView(familyOverride: .accessoryCircular) }
            }
            HStack(spacing: 10) {
                lock(Self.rectangular) { AskLucinaWidgetView(familyOverride: .accessoryRectangular, entry: ask) }
                lock(Self.rectangular) { AppointmentWidgetView(familyOverride: .accessoryRectangular, entry: next) }
            }
            HStack(spacing: 10) {
                lock(Self.rectangular) { ShoppingWidgetView(familyOverride: .accessoryRectangular, entry: shopping) }
                lock(Self.rectangular) { WeightWidgetView(familyOverride: .accessoryRectangular, entry: weight) }
            }
            HStack(spacing: 10) {
                lock(Self.rectangular) { TasksWidgetView(familyOverride: .accessoryRectangular, entry: tasks) }
                lock(Self.rectangular) { TrainingWidgetView(familyOverride: .accessoryRectangular, entry: training) }
            }
            HStack(spacing: 10) {
                lock(Self.rectangular) { PokemonWidgetView(familyOverride: .accessoryRectangular, entry: pokemon) }
                lock(Self.rectangular) { FilmsWidgetView(familyOverride: .accessoryRectangular, entry: films) }
            }
            lock(Self.inline) { CountdownWidgetView(familyOverride: .accessoryInline, entry: countdown) }
            lock(Self.rectangular) { CountdownWidgetView(familyOverride: .accessoryRectangular, entry: countdown) }
        }
        .redacted(reason: redacted ? .privacy : [])
    }

    /// The pinned events, else three samples (today, tomorrow, in 15 days).
    private static func countdown(_ now: Date) -> CountdownWidget.Entry {
        let pinned = Countdown.visible(Countdown.pinned, fresher: Countdown.upcoming, now: now)
        guard pinned.isEmpty else { return CountdownWidget.Entry(date: now, events: pinned) }
        let day = { (offset: Int) in Calendar.current.date(byAdding: .day, value: offset, to: now) ?? now }
        return CountdownWidget.Entry(date: now, events: [
            CountdownEvent(id: "a", title: String(localized: "Network exam"), start: day(15), allDay: true),
            CountdownEvent(id: "b", title: "Dentist", start: day(1).addingTimeInterval(3600)),
            CountdownEvent(id: "c", title: "Hack Night", start: now.addingTimeInterval(7200)),
        ].sorted { $0.start < $1.start })
    }

    private static func posters(_ section: FilmsSection?) -> [String: UIImage] {
        var posters: [String: UIImage] = [:]
        for film in section?.films ?? [] {
            if let url = WidgetStore.imageURL("poster-\(film.id).jpg"), let image = UIImage(contentsOfFile: url.path) { posters[film.id] = image }
        }
        return posters
    }

    /// A Home Screen tile: the card colour, the system's corner radius and content margins.
    private func home<V: View>(_ size: CGSize, @ViewBuilder _ view: () -> V) -> some View {
        view()
            .padding(16)
            .frame(width: size.width, height: size.height)
            .background(LucinaTokens.Colors.card)
            .clipShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
    }

    /// A Lock Screen accessory: white on the wallpaper, like the vibrant rendering.
    private func lock<V: View>(_ size: CGSize, @ViewBuilder _ view: () -> V) -> some View {
        view()
            .frame(width: size.width, height: size.height)
            .foregroundStyle(.white)
            .environment(\.colorScheme, .dark)
    }
}
#endif
