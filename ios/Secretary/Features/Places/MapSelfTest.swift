#if SCREENSHOTS
import CoreLocation
import Observation
import SwiftUI
import os

private let log = Logger(subsystem: "tech.getsecretary.app", category: "MapSelfTest")

/// `-SecretaryMapSelfTest place|card|deals|route`: the maps on the iPhone without UI tests (XCUITest can't
/// drive iOS 27 from Xcode 26.6). A full-screen cover runs one check and stays up for a screenshot:
/// - `card`: a proactive message's `meta.place` through TimelineMeta, its PlaceCard geocoded, then
///   opened like a tap; checks the camera, the distance and the travel times;
/// - `place`: the place screen for a place with coordinates (home);
/// - `deals`: the deals map, its pin count once loaded;
/// - `timeline`: the newest real chat message with a place (a `place` block or a proactive `meta.place`)
///   from the server's timeline, its card opened like a tap, then the same checks as `card`;
/// - `navigate`: the route with `navigate=1` (a Navigate action): the in-app map navigating;
/// - `naviga`: an agenda question to Lucina in the self-test conversation (never the owner's
///   chat), her first "Naviga" button performed like a tap: the in-app map navigating.
/// Every mode also checks that nothing opened another maps app (Apple Maps, Google Maps).
/// Each step writes `PASS …` or `FAIL <step> <reason>` to the log and to Documents/map-selftest.txt.
enum MapSelfTest {
    static var mode: String? {
        UserDefaults.standard.string(forKey: "SecretaryMapSelfTest").flatMap { ["place", "card", "deals", "route", "timeline", "navigate", "naviga"].contains($0) ? $0 : nil }
    }

    /// A proactive message's meta (a calendar reminder with the event's place).
    static var sampleMeta: String {
        let calendar = Calendar.current
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: Date())!
        let when = calendar.date(bySettingHour: 18, minute: 0, second: 0, of: tomorrow)!
        let timestamp = ISO8601DateFormatter().string(from: when)
        return """
        {"kind":"proactive","place":{"title":"Hack Night: Rust meets AI","address":"Folkehuset Møllestien, Aarhus","when":"\(timestamp)","kind":"event"}}
        """
    }

    static var sampleRoute: String {
        var components = URLComponents()
        components.path = "/map/place"
        let meta = try? JSONDecoder().decode(TimelineMeta.self, from: Data(sampleMeta.utf8))
        components.queryItems = [URLQueryItem(name: "title", value: "Hack Night: Rust meets AI"),
                                 URLQueryItem(name: "address", value: "Folkehuset Møllestien, Aarhus"),
                                 URLQueryItem(name: "when", value: meta?.place?.when.map { ISO8601DateFormatter().string(from: $0) })]
        return components.string!
    }

    static let homeTest = Place(title: "Home test", coordinate: CLLocationCoordinate2D(latitude: 56.19633, longitude: 10.2011))

    @MainActor
    static func run(mode: String, openRoute: (String) -> Void, timeline: (() async throws -> TimelinePage)? = nil,
                    chat: ((ChatRequest) async throws -> ChatResponse)? = nil, show: (Place) -> Void) async {
        let report = Report()
        report.line("START \(mode)")
        switch mode {
        case "card": await runCard(report, show: show)
        case "timeline": await runTimeline(report, timeline: timeline, show: show)
        case "route":
            openRoute(sampleRoute)
            if await wait(45, until: { MapSelfTestProbe.shared.mapShown }) {
                report.pass("route opened")
            } else {
                report.fail("route", "the routed place screen did not appear")
            }
        case "naviga":
            guard let chat else {
                report.fail("naviga", "no chat API")
                break
            }
            let request = ChatRequest(message: "Cosa ho in calendario nei prossimi 7 giorni?", sessionId: "selftest-maps",
                                      clientMessageId: "selftest-maps-\(Int(Date().timeIntervalSince1970))")
            let reply: ChatResponse
            do {
                reply = try await chat(request)
            } catch {
                report.fail("naviga", "chat: \(error.localizedDescription)")
                break
            }
            guard let rich = reply.rich else {
                report.fail("naviga", "the agenda reply has no cards")
                break
            }
            report.pass("agenda \(rich.bubbles.count) bubble(s), \(rich.blocks.count) card(s): \(rich.bubbles.first ?? "")")
            guard let naviga = rich.actions.first(where: { $0.kind == .route && $0.path.map(PlaceLinks.navigates) == true }),
                  let path = naviga.path else {
                report.fail("naviga", "no Naviga button among \(rich.actions.map(\.label))")
                break
            }
            report.pass("button \(naviga.label) -> \(Place.fromRoute(path)?.title ?? "?")")
            openRoute(path)
            guard await wait(45, until: { MapSelfTestProbe.shared.mapShown }) else {
                report.fail("naviga", "the in-app map did not appear")
                break
            }
            if await wait(15, until: { MapSelfTestProbe.shared.navigating }) {
                report.pass("navigating in the app")
            } else {
                report.fail("naviga", "the map did not start navigating")
            }
            if await wait(30, until: { MapSelfTestProbe.shared.navigationSteps != nil }) {
                report.line("INFO turn-by-turn steps \(MapSelfTestProbe.shared.navigationSteps ?? 0)")
            }
        case "navigate":
            openRoute(sampleRoute + "&navigate=1")
            guard await wait(45, until: { MapSelfTestProbe.shared.mapShown }) else {
                report.fail("navigate", "the routed place screen did not appear")
                break
            }
            if await wait(10, until: { MapSelfTestProbe.shared.navigating }) {
                report.pass("navigating in the app")
            } else {
                report.fail("navigate", "the place screen did not start navigating")
            }
            if await wait(30, until: { MapSelfTestProbe.shared.navigationSteps != nil }) {
                report.line("INFO turn-by-turn steps \(MapSelfTestProbe.shared.navigationSteps ?? 0)")
            }
        case "place":
            guard await wait(15, until: { MapSelfTestProbe.shared.mapShown }) else {
                return report.fail("open map", "the place screen did not appear")
            }
            report.pass("open map")
            await checkMap(report, target: homeTest.coordinate!)
        default:
            if await wait(60, until: { MapSelfTestProbe.shared.dealsPins != nil }), let pins = MapSelfTestProbe.shared.dealsPins {
                report.pass("deals pins \(pins)")
            } else {
                report.fail("deals", "the deals map did not load in 60 s")
            }
        }
        if MapSelfTestProbe.shared.externalOpens == 0 {
            report.pass("stayed in the app")
        } else {
            report.fail("in-app", "another maps app was opened \(MapSelfTestProbe.shared.externalOpens) times")
        }
        report.line("DONE")
    }

    @MainActor
    private static func runCard(_ report: Report, show: (Place) -> Void) async {
        let meta: TimelineMeta
        do {
            meta = try JSONDecoder().decode(TimelineMeta.self, from: Data(sampleMeta.utf8))
        } catch {
            return report.fail("parse", "TimelineMeta: \(error.localizedDescription)")
        }
        guard let place = meta.place else { return report.fail("parse", "meta.place is nil") }
        guard place.kind == .event, place.when != nil, place.address != nil, place.coordinate == nil else {
            return report.fail("parse", "kind=\(place.kind.rawValue) when=\(place.when != nil) address=\(place.address ?? "-")")
        }
        report.pass("parse \(place.title) | \(place.address ?? "") | \(place.timeText ?? "")")
        await runPlace(report, place: place, requiresLeaveAt: true, show: show)
    }

    /// The newest message in the real timeline that carries a place, opened like a tap on its card.
    @MainActor
    private static func runTimeline(_ report: Report, timeline: (() async throws -> TimelinePage)?, show: (Place) -> Void) async {
        guard let timeline else { return report.fail("timeline", "no API") }
        let page: TimelinePage
        do {
            page = try await timeline()
        } catch {
            return report.fail("timeline", error.localizedDescription)
        }
        for item in page.items {
            let block = item.rich?.blocks.lazy.compactMap { block -> Place? in
                if case let .place(place) = block { return place }
                return nil
            }.first
            guard let place = block ?? item.meta?.place else { continue }
            let source = block != nil ? "block" : "meta.place"
            report.pass("timeline \(source) \(item.ts.formatted(date: .abbreviated, time: .shortened)) \(place.title) | \(place.address ?? "-") | \(place.timeText ?? "-")")
            let future = place.when.map { $0 > Date() } ?? false
            return await runPlace(report, place: place, requiresLeaveAt: future, show: show)
        }
        report.fail("timeline", "no place in the newest \(page.items.count) messages")
    }

    /// A place's card: geocoded, opened like a tap, then the place screen's checks.
    @MainActor
    private static func runPlace(_ report: Report, place: Place, requiresLeaveAt: Bool, show: (Place) -> Void) async {
        let probe = MapSelfTestProbe.shared
        probe.requiresLeaveAt = requiresLeaveAt
        probe.targetTitle = place.title
        show(place)

        guard await wait(60, until: { probe.resolved != nil || probe.notFound }), let resolved = probe.resolved else {
            return report.fail("geocode", probe.notFound ? "not found" : "no answer in 60 s")
        }
        let found = String(format: "%.4f,%.4f", resolved.lat, resolved.lng)
        if (56.10...56.25).contains(resolved.lat) && (10.05...10.30).contains(resolved.lng) {
            report.pass("geocode \(found)")
        } else {
            report.fail("geocode", "\(found) is outside Aarhus")
        }
        report.pass("resolved \(resolved.name ?? "-") \(resolved.address ?? "-")")

        probe.openTick += 1
        guard await wait(15, until: { probe.mapShown }) else { return report.fail("open map", "the place screen did not appear") }
        report.pass("open map")
        await checkMap(report, target: resolved.coordinate)
    }

    /// The camera, the distance line and the travel times of the place screen.
    @MainActor
    private static func checkMap(_ report: Report, target: CLLocationCoordinate2D) async {
        let probe = MapSelfTestProbe.shared
        guard await wait(15, until: { probe.camera != nil }) else { return report.fail("centred", "the camera never settled") }
        // Routes can arrive after the first camera fit; wait before checking the final line fit.
        _ = await wait(45, until: { probe.etaDone })
        // Time for the position to arrive and the camera to refit to the place and the owner.
        try? await Task.sleep(for: .seconds(4))
        if let camera = probe.camera {
            let expected = probe.expected ?? target
            let off = meters(camera, expected)
            let what = probe.routesText != nil ? "on the selected route" : (probe.fitsUser ? "on the place and you" : "on the place")
            let place = meters(expected, target)
            if off <= 150 {
                report.pass("centred \(Int(off.rounded())) m \(what) (fit centre \(Int(place.rounded())) m from the place)")
            } else {
                report.fail("centred", "\(Int(off.rounded())) m from the fit centre \(what)")
            }
        }
        guard await wait(45, until: { probe.etaDone }) else {
            if let distance = probe.distanceText { report.pass("distance \(distance)") }
            return report.fail("eta", probe.distanceText == nil ? "no position and no home" : "no answer in 45 s")
        }
        if let distance = probe.distanceText { report.pass("distance \(distance)") } else { report.fail("distance", "no text") }
        if let eta = probe.etaText, !eta.isEmpty { report.pass("eta \(eta)") } else { report.fail("eta", "no route for any mode") }
        if let reason = probe.fallbackReason {
            report.line("INFO routes fallback mapkit \(reason)")
        } else if let modes = probe.routesText {
            report.pass("routes \(modes)")
            if let leaveAt = probe.leaveAt {
                report.pass("leaveAt \(leaveAt.formatted(date: .omitted, time: .shortened))")
            } else if probe.requiresLeaveAt {
                report.fail("leaveAt", "appointment route has no departure time")
            }
        }
    }

    private static func meters(_ a: CLLocationCoordinate2D, _ b: CLLocationCoordinate2D) -> Double {
        CLLocation(latitude: a.latitude, longitude: a.longitude).distance(from: CLLocation(latitude: b.latitude, longitude: b.longitude))
    }

    @MainActor
    private static func wait(_ seconds: Double, until done: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(seconds)
        while Date() < deadline {
            if done() { return true }
            try? await Task.sleep(for: .milliseconds(200))
        }
        return done()
    }

    /// The log and Documents/map-selftest.txt (overwritten per run, synced after each line).
    private final class Report {
        private let handle: FileHandle?

        init() {
            let url = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("map-selftest.txt")
            try? Data().write(to: url, options: .atomic)
            handle = try? FileHandle(forWritingTo: url)
        }

        deinit { try? handle?.close() }

        func pass(_ text: String) { line("PASS \(text)") }

        func fail(_ step: String, _ reason: String) { line("FAIL \(step) \(reason)") }

        func line(_ text: String) {
            log.notice("MapSelfTest \(text, privacy: .public)")
            guard let handle else { return }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: Data((text + "\n").utf8))
            try? handle.synchronize()
        }
    }
}

/// What the map views report to the self-test.
@MainActor
@Observable
final class MapSelfTestProbe {
    static let shared = MapSelfTestProbe()

    /// The PlaceCard under test (other cards in the chat ignore the probe).
    var targetTitle: String?
    var resolved: PlaceResolver.Resolved?
    var notFound = false
    /// Bumped to open the target card's map, as a tap does.
    var openTick = 0
    var mapShown = false
    var camera: CLLocationCoordinate2D?
    /// Where LucinaMap's fit puts the camera centre.
    var expected: CLLocationCoordinate2D?
    var fitsUser = false
    var distanceText: String?
    var etaText: String?
    var etaDone = false
    var routesText: String?
    var fallbackReason: String?
    var leaveAt: Date?
    var requiresLeaveAt = false
    var dealsPins: Int?
    var navigating = false
    var navigationSteps: Int?
    /// ExternalMaps.open calls (the explicit "Open in Apple Maps"); a self-test never makes one.
    var externalOpens = 0
}

/// The cover the self-test runs in (MainTabs, when `-SecretaryMapSelfTest` is set).
struct MapSelfTestHost: ViewModifier {
    @Environment(AppModel.self) private var app
    @State private var mode = MapSelfTest.mode

    func body(content: Content) -> some View {
        if mode == "route" || mode == "navigate" || mode == "naviga" {
            content.task {
                let api = app.api
                await MapSelfTest.run(mode: mode ?? "route", openRoute: { app.open(route: $0) }, chat: { try await api.chat($0) }) { _ in }
            }
        } else {
            content.fullScreenCover(isPresented: Binding(get: { mode != nil }, set: { if !$0 { mode = nil } })) {
                if let mode { MapSelfTestScreen(mode: mode) }
            }
        }
    }
}

private struct MapSelfTestScreen: View {
    @Environment(AppModel.self) private var app
    let mode: String
    @State private var place: Place?

    var body: some View {
        NavigationStack {
            switch mode {
            case "deals":
                DealsMapView(api: app.api)
            case "place":
                PlaceMapView(place: MapSelfTest.homeTest, coordinate: MapSelfTest.homeTest.coordinate!)
            default:
                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(verbatim: place.map { "\($0.title) · \($0.timeText ?? "")" } ?? "…")
                            .padding(.horizontal, 14)
                            .padding(.vertical, 10)
                            .background(Theme.card, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
                        if let place { PlaceCard(place: place) }
                    }
                    .frame(maxWidth: 520, alignment: .leading)
                    .padding(LucinaTokens.Space.gutter)
                }
                .background(Theme.page.ignoresSafeArea())
                .navigationTitle(Text(verbatim: "Map self-test"))
                .navigationBarTitleDisplayMode(.inline)
            }
        }
        .tint(Theme.accent)
        .task {
            let api = app.api
            await MapSelfTest.run(mode: mode, openRoute: { app.open(route: $0) }, timeline: { try await api.timeline(limit: 50) }) { place = $0 }
        }
    }
}
#endif
