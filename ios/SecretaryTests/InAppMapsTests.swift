import CoreLocation
import XCTest
@testable import Secretary

/// Navigation stays in the app: every default place action (Navigate, cards, deal stores, widgets,
/// `secretary://open/map/place`, chat buttons and maps links, Shortcuts, the self-tests) opens
/// Lucina's own map. Apple Maps exists only behind the explicit "Open in Apple Maps" menu item
/// (ExternalMaps.swift). These tests fail when a default action builds an external maps URL.
final class InAppMapsTests: XCTestCase {
    private let event = Place(title: "Esame di Reti", address: "Åbogade 34, Aarhus",
                              when: Date(timeIntervalSince1970: 1_800_000_000), eventId: "ev-1", kind: .event)
    private let store = Place(title: "Netto Vesterbro", coordinate: CLLocationCoordinate2D(latitude: 56.19, longitude: 10.19))

    /// The URL a route takes when it leaves the app (widgets link to `secretary://open/<route>`).
    private func assertInApp(_ route: String, file: StaticString = #filePath, line: UInt = #line) {
        let url = WidgetRoute.url(route)
        XCTAssertEqual(url.scheme, "secretary", "\(route) leaves the app", file: file, line: line)
        XCTAssertFalse(ExternalMaps.isExternal(url), "\(route) builds an external maps URL", file: file, line: line)
        XCTAssertNotNil(Place.fromRoute(route), "\(route) is not the in-app map", file: file, line: line)
    }

    func testDefaultActionsBuildInAppRoutes() throws {
        for place in [event, store] {
            for navigate in [false, true] {
                let route = PlaceLinks.route(place, navigate: navigate)
                XCTAssertTrue(route.hasPrefix("/map/place?"), route)
                XCTAssertEqual(PlaceLinks.navigates(route), navigate)
                assertInApp(route)
                // The widget/deep-link form of the same route lands on the same place.
                let deepLink = WidgetRoute.url(route)
                let back = try XCTUnwrap(WidgetRoute.route(from: deepLink))
                XCTAssertEqual(Place.fromRoute("/" + back), Place.fromRoute(route))
                XCTAssertEqual(PlaceLinks.navigates(back), navigate)
            }
        }
        // The appointment widget's link (AppointmentWidgetView.link builds this shape).
        assertInApp("/map/place?title=Dentist&address=Vesterbrogade%2012&when=2026-10-27T09:00:00Z&eventId=e1")
        XCTAssertEqual(Place.fromRoute(PlaceLinks.route(event))?.kind, .event)
    }

    func testMapsLinksInTheChatOpenTheAppsMap() throws {
        let apple = try XCTUnwrap(URL(string: "https://maps.apple.com/?ll=56.1572,10.2107&q=Aarhus%20Domkirke"))
        XCTAssertEqual(PlaceLinks.place(fromMapsLink: apple)?.coordinate?.latitude ?? 0, 56.1572, accuracy: 0.0001)
        let google = try XCTUnwrap(URL(string: "https://www.google.com/maps/dir/?api=1&destination=Dokk1%2C%20Aarhus"))
        XCTAssertEqual(PlaceLinks.place(fromMapsLink: google)?.address, "Dokk1, Aarhus")
        let scheme = try XCTUnwrap(URL(string: "maps://?daddr=Folkehuset%20M%C3%B8llestien"))
        XCTAssertEqual(PlaceLinks.place(fromMapsLink: scheme)?.title, "Folkehuset Møllestien")
        XCTAssertNil(PlaceLinks.place(fromMapsLink: try XCTUnwrap(URL(string: "https://example.com/maps?q=x"))))
    }

    func testOnlyTheExplicitMenuBuildsAppleMapsURLs() throws {
        let url = try XCTUnwrap(ExternalMaps.appleMapsURL(to: store.coordinate!, name: store.title))
        XCTAssertTrue(ExternalMaps.isExternal(url))
        XCTAssertTrue(ExternalMaps.isExternal(try XCTUnwrap(ExternalMaps.appleMapsSearchURL("Dokk1"))))
        for external in ["comgooglemaps://?daddr=1,2", "maps://?q=x", "https://www.google.com/maps/dir/?api=1", "https://maps.google.dk/?q=x"] {
            XCTAssertTrue(ExternalMaps.isExternal(try XCTUnwrap(URL(string: external))), external)
        }
        XCTAssertFalse(ExternalMaps.isExternal(WidgetRoute.url("map/place?title=x&address=y")))
    }

    /// No source file but ExternalMaps.swift builds or opens a maps app (Place.swift may only
    /// recognise maps links, to open them in the app). Runs where the sources are readable (the
    /// simulator); skipped on a device.
    func testNoDefaultPathOpensAnotherMapsApp() throws {
        let ios = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
        let roots = ["Secretary", "Extensions"].map { ios.appendingPathComponent($0) }
        guard roots.allSatisfy({ FileManager.default.fileExists(atPath: $0.path) }) else {
            throw XCTSkip("The app's sources are not readable here (a device run).")
        }
        let forbidden = ["openInMaps", "MKLaunchOptions", "comgooglemaps", "maps://", "maps.apple.com", "google.com/maps",
                         "maps.google.", "googleMapsURL"]
        let allowed: [String: Set<String>] = ["ExternalMaps.swift": Set(forbidden), "Place.swift": ["maps.apple.com", "maps.google."]]
        var found: [String] = []
        for root in roots {
            let files = FileManager.default.enumerator(at: root, includingPropertiesForKeys: nil)?
                .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" } ?? []
            for file in files {
                let text = try String(contentsOf: file, encoding: .utf8)
                for pattern in forbidden where text.contains(pattern) && !(allowed[file.lastPathComponent]?.contains(pattern) ?? false) {
                    found.append("\(file.lastPathComponent): \(pattern)")
                }
            }
        }
        XCTAssertEqual(found, [], "external maps outside ExternalMaps.swift")
    }
}
