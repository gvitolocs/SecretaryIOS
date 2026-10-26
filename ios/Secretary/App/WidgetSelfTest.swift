#if DEBUG || SCREENSHOTS
import Foundation
import os
import WidgetKit

private let log = Logger(subsystem: "tech.getsecretary.app", category: "WidgetSelfTest")

/// `-SecretaryWidgetSelfTest YES`: checks the widgets' data path on the iPhone without UI tests.
/// Every section is fetched the way the extension does it (token from the shared keychain), saved
/// to the widget store and read back; the training section is published from the gym engine; the
/// widget links are opened and the resulting screen state checked (not `gym/start` or
/// `memos/record`, which would start a workout or a recording); the widgets placed on the Home
/// and Lock Screens are listed. One `WidgetSelfTest …` line per step, also in
/// Documents/widget-selftest.txt (copy it with `devicectl device copy from`). Names and texts are
/// left out: counts and true/false only.
@MainActor
enum WidgetSelfTest {
    private static var ran = false
    private static var lines: [String] = []

    static func runIfRequested(_ app: AppModel) async {
        guard !ran, UserDefaults.standard.bool(forKey: "SecretaryWidgetSelfTest") else { return }
        ran = true
        lines = []
        // The tabs' first `device.resume()` writes the bridge credentials.
        for _ in 0..<50 where BridgeCredentials.read() == nil { try? await Task.sleep(for: .milliseconds(200)) }
        guard let fetcher = WidgetFetcher.fromBridge() else {
            note("bridge ok=false (no token in the shared keychain)")
            return finish(passed: false)
        }
        note("bridge ok=true endpoints=\(BridgeCredentials.read()?.endpoints.count ?? 0) appGroup=\(WidgetStore.groupContainer != nil)")
        var passed = true

        passed = await check("ask", { try await fetcher.ask() }) { "reply=\($0.reply != nil)" } && passed
        passed = await check("appointment", { try await fetcher.appointment() }) {
            "next=\($0.next != nil) then=\($0.then != nil) place=\($0.next?.location != nil) travel=\($0.next?.travelMinutes != nil)"
        } && passed
        passed = await check("shopping", { try await fetcher.shopping(listId: nil, previous: WidgetStore.read(ShoppingSection.self)) }) {
            "list=\($0.listId) items=\($0.items.count) deals=\($0.deals.count)"
        } && passed
        passed = await check("weight", { try await fetcher.weight() }) {
            "current=\($0.currentLabel != nil) unit=\($0.unit) trend=\($0.trend.count) today=\($0.loggedToday)"
        } && passed
        passed = await check("tasks", { try await fetcher.tasks() }) {
            "items=\($0.items.count) tasks=\($0.items.filter { $0.kind == .task }.count) reminders=\($0.items.filter { $0.kind == .reminder }.count) done=\($0.completedToday)"
        } && passed
        passed = await check("pokemon", { try await fetcher.pokemon() }) { "running=\($0.running) badges=\($0.badges) party=\($0.party.count)" } && passed
        let screen = (try? await fetcher.pokemonScreen()) != nil
        note("pokemonScreen ok=\(screen)")
        passed = await check("films", { try await fetcher.films() }) { "films=\($0.films.count) posters=\($0.films.filter { $0.posterPath != nil }.count)" } && passed
        if let film = WidgetStore.read(FilmsSection.self)?.films.first {
            note("poster ok=\(await fetcher.poster(film) != nil)")
        }

        if app.gym.view == nil { await app.gym.refresh() }
        app.publishWidgets()
        let training = WidgetStore.read(TrainingSection.self)
        note("training ok=\(training != nil) week=\(training?.weekCount ?? -1) goal=\(training?.weeklyGoal ?? -1) sessions=\(training?.sessions.count ?? -1)")
        passed = passed && training != nil

        passed = routes(app) && passed

        let configurations: [WidgetInfo] = await withCheckedContinuation { done in
            WidgetCenter.shared.getCurrentConfigurations { done.resume(returning: (try? $0.get()) ?? []) }
        }
        let installed = configurations.map { "\($0.kind.replacingOccurrences(of: "tech.getsecretary.widget.", with: ""))/\(family($0.family))" }
        note("installed count=\(configurations.count) \(installed.sorted().joined(separator: " "))")
        WidgetStore.reload()
        finish(passed: passed)
    }

    private static func check<S: WidgetSection>(_ name: String, _ fetch: () async throws -> S, _ summary: (S) -> String) async -> Bool {
        let start = Date()
        do {
            let section = try await fetch()
            WidgetStore.write(section)
            // Dates are stored to the millisecond: compare what the widget shows, not the structs.
            let stored = WidgetStore.read(S.self)
            let roundTrip = stored.map(summary) == summary(section) && abs((stored?.fetchedAt ?? .distantPast).timeIntervalSince(section.fetchedAt)) < 0.01
            note("\(name) ok=\(roundTrip) ms=\(Int(Date().timeIntervalSince(start) * 1000)) stored=\(roundTrip) \(summary(section))")
            return roundTrip
        } catch {
            note("\(name) ok=false error=\(APIError.wrap(error).code)")
            return false
        }
    }

    private static func routes(_ app: AppModel) -> Bool {
        let tab = app.selectedTab
        var ok = true
        func expect(_ route: String, _ condition: () -> Bool) {
            app.openWidgetRoute(route)
            let pass = condition()
            ok = ok && pass
            note("route \(route) ok=\(pass)")
        }
        let url = WidgetRoute.url("films/play/abc%2F1")
        note("route parse ok=\(WidgetRoute.route(from: url) == "films/play/abc%2F1" && WidgetRoute.route(from: URL(string: "https://x/open/y")!) == nil)")
        expect("chat/compose") { app.selectedTab == .chat && app.chat.prefill != nil }
        expect("shopping/add") { app.selectedTab == .shopping }
        expect("weight/log") { app.selectedTab == .fitness }
        expect("pokemon") { app.selectedTab == .plugins && app.showPokemon }
        app.showPokemon = false
        expect("tasks") { app.selectedTab == .plugins && app.showTasks }
        app.showTasks = false
        expect("films/play/selftest") { app.showMedia && app.openFilmID == "selftest" }
        app.showMedia = false
        app.openFilmID = nil
        app.showShoppingAdd = false
        app.showQuickWeight = false
        app.selectedTab = tab
        return ok
    }

    private static func family(_ family: WidgetFamily) -> String {
        switch family {
        case .systemSmall: return "small"
        case .systemMedium: return "medium"
        case .systemLarge: return "large"
        case .systemExtraLarge: return "xl"
        case .accessoryCircular: return "circular"
        case .accessoryRectangular: return "rectangular"
        case .accessoryInline: return "inline"
        default: return "other"
        }
    }

    private static func note(_ line: String) {
        log.notice("WidgetSelfTest \(line, privacy: .public)")
        lines.append(line)
    }

    private static func finish(passed: Bool) {
        note("done passed=\(passed)")
        let text = lines.map { "WidgetSelfTest \($0)" }.joined(separator: "\n") + "\n"
        if let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {
            try? text.write(to: docs.appendingPathComponent("widget-selftest.txt"), atomically: true, encoding: .utf8)
        }
    }
}
#endif
