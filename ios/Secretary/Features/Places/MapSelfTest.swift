#if SCREENSHOTS
import CoreLocation
import Observation
import SwiftUI
import os

private let log = Logger(subsystem: "tech.getsecretary.app", category: "MapSelfTest")

/// `-SecretaryMapSelfTest place|card|deals`: the maps on the iPhone without UI tests (XCUITest can't
/// drive iOS 27 from Xcode 26.6). A full-screen cover runs one check and stays up for a screenshot:
/// - `card`: a proactive message's `meta.place` through TimelineMeta, its PlaceCard geocoded, then
///   opened like a tap; checks the camera, the distance and the travel times;
/// - `place`: the place screen for a place with coordinates (home);
/// - `deals`: the deals map, its pin count once loaded.
/// Each step writes `PASS …` or `FAIL <step> <reason>` to the log and to Documents/map-selftest.txt.
enum MapSelfTest {
    static var mode: String? {
        UserDefaults.standard.string(forKey: "SecretaryMapSelfTest").flatMap { ["place", "card", "deals"].contains($0) ? $0 : nil }
    }

    /// A proactive message's meta (a calendar reminder with the event's place).
    static let sampleMeta = """
    {"kind":"proactive","place":{"title":"Hack Night: Rust meets AI","address":"Folkehuset Møllestien, Aarhus","when":"2026-10-27T17:00:00Z","kind":"event"}}
    """

    static let homeTest = Place(title: "Home test", coordinate: CLLocationCoordinate2D(latitude: 56.19633, longitude: 10.2011))

    @MainActor
    static func run(mode: String, show: (Place) -> Void) async {
        let report = Report()
        report.line("START \(mode)")
        switch mode {
        case "card": await runCard(report, show: show)
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
        report.line("DONE")
    }

    @MainActor
    private static func runCard(_ report: Report, show: (Place) -> Void) async {
        let probe = MapSelfTestProbe.shared
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
        probe.targetTitle = place.title
        show(place)

        guard await wait(30, until: { probe.resolved != nil || probe.notFound }), let resolved = probe.resolved else {
            return report.fail("geocode", probe.notFound ? "not found" : "no answer in 30 s")
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
        // Time for the position to arrive and the camera to refit to the place and the owner.
        try? await Task.sleep(for: .seconds(4))
        if let camera = probe.camera {
            let expected = probe.expected ?? target
            let off = meters(camera, expected)
            let what = probe.fitsUser ? "on the place and you" : "on the place"
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
    var dealsPins: Int?
}

/// The cover the self-test runs in (MainTabs, when `-SecretaryMapSelfTest` is set).
struct MapSelfTestHost: ViewModifier {
    @State private var mode = MapSelfTest.mode

    func body(content: Content) -> some View {
        content.fullScreenCover(isPresented: Binding(get: { mode != nil }, set: { if !$0 { mode = nil } })) {
            if let mode { MapSelfTestScreen(mode: mode) }
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
                        Text(verbatim: "Hack Night: Rust meets AI is tomorrow at 18:00.")
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
        .task { await MapSelfTest.run(mode: mode) { place = $0 } }
    }
}
#endif
