import CoreLocation
import XCTest
@testable import Secretary

final class MapsRoutesTests: XCTestCase {
    func testPartialResponseKeepsGoodModesAndOptionalFields() throws {
        let response = try JSONDecoder().decode(MapsRoutes.self, from: Data("""
        {"provider":"google","routes":[
          {"mode":"drive","durationS":540,"staticDurationS":480,"distanceM":4200,
           "polyline":[[56.19633,10.2011],[56.1611,10.2006],[91,0],[56],null],
           "leaveAt":"2026-10-27T16:46:00Z"},
          {"mode":"transit","durationS":900,"transit":[null,
           {"line":"2A","vehicle":"BUS","from":"Skejby","to":"Aarhus",
            "departAt":"2026-10-27T16:40:00Z","arriveAt":"2026-10-27T16:55:00Z"}]},
          {"mode":"walk","durationS":1800,"polyline":"bad","leaveAt":"bad","transit":false},
          {"mode":"drive","durationS":999},
          {"mode":"bike","durationS":-1},
          {"mode":"future-mode","durationS":300},null],"failed":["bike",null]}
        """.utf8))
        XCTAssertEqual(response.provider, "google")
        XCTAssertEqual(response.routes.map(\.mode), [.drive, .transit, .walk])
        XCTAssertEqual(response.failed, ["bike"])
        let drive = try XCTUnwrap(response.routes.first)
        XCTAssertEqual(drive.durationS, 540)
        XCTAssertEqual(drive.staticDurationS, 480)
        XCTAssertEqual(drive.distanceM, 4200)
        XCTAssertEqual(drive.polyline.count, 2)
        XCTAssertNotNil(drive.leaveAt)
        let transit = response.routes[1]
        XCTAssertEqual(transit.transit.count, 1)
        XCTAssertEqual(transit.transit[0].line, "2A")
        XCTAssertNotNil(transit.transit[0].departAt)
        XCTAssertNotNil(transit.transit[0].arriveAt)
        XCTAssertTrue(response.routes[2].polyline.isEmpty)
        XCTAssertNil(response.routes[2].leaveAt)
    }

    func testEmptySuccessDiffersFromInvalidResponse() throws {
        let empty = try JSONDecoder().decode(MapsRoutes.self, from: Data(#"{"routes":[],"failed":["transit"]}"#.utf8))
        XCTAssertTrue(empty.routes.isEmpty, "a valid empty answer should not trigger MapKit fallback")
        XCTAssertThrowsError(try JSONDecoder().decode(MapsRoutes.self, from: Data(#"{"error":"provider_unavailable"}"#.utf8)))
        let geocode = try JSONDecoder().decode(MapsGeocode.self, from: Data("""
        {"results":[{"lat":91,"lng":10},{"name":"Møllestien","address":"Aarhus","lat":56.16,"lng":10.2},null]}
        """.utf8))
        XCTAssertEqual(geocode.results.count, 1)
        XCTAssertEqual(geocode.results[0].name, "Møllestien")
    }

    func testPlaceRoutesDecodeQueryAndReuseValidation() throws {
        var components = URLComponents()
        components.path = "/map/place"
        components.queryItems = [URLQueryItem(name: "title", value: "Hack Night: Rust & AI"),
                                 URLQueryItem(name: "address", value: "Folkehuset Møllestien, Aarhus"),
                                 URLQueryItem(name: "lat", value: "56.1611"), URLQueryItem(name: "lng", value: "10.2006"),
                                 URLQueryItem(name: "when", value: "2026-10-27T18:00:00+01:00"),
                                 URLQueryItem(name: "end", value: "2026-10-27T20:00:00+01:00"),
                                 URLQueryItem(name: "eventId", value: "iphone:a&b")]
        let route = try XCTUnwrap(components.string)
        XCTAssertTrue(NotificationRoute.isSafe(route), "encoded place routes can exceed the old 200-character limit")
        let place = try XCTUnwrap(Place.fromRoute(route))
        XCTAssertEqual(place.title, "Hack Night: Rust & AI")
        XCTAssertEqual(place.address, "Folkehuset Møllestien, Aarhus")
        XCTAssertEqual(place.lat, 56.1611)
        XCTAssertEqual(place.eventId, "iphone:a&b")
        XCTAssertEqual(place.when, Place.date(.string("2026-10-27T17:00:00Z")))
        XCTAssertNotNil(place.end)
        XCTAssertEqual(Place.fromRoute("secretary://map/place?" + (components.percentEncodedQuery ?? "")), place)
        XCTAssertNil(Place.fromRoute("https://example.com" + route))
        XCTAssertNil(Place.fromRoute("/map/place?title=Missing&lat=91&lng=10"))
        XCTAssertNil(Place.fromRoute("/map/place?title=Missing&lat=56"))
        XCTAssertNil(Place.fromRoute("/map/place?address=Aarhus"))
        XCTAssertNil(Place.fromRoute("//map/place?title=X&address=Aarhus"))
        XCTAssertNotNil(Place.fromRoute("/map/place?title=X&address=Aarhus&lat=91&lng=10"))
    }

    func testAPIEncodesQueriesAndPropagatesProviderFailure() async throws {
        StubURLProtocol.requests = []
        StubURLProtocol.handler = { request in
            if request.url?.path == "/v1/maps/geocode" { return .init(body: #"{"results":[]}"#) }
            return .init(body: #"{"routes":[],"failed":["bike"]}"#)
        }
        defer { StubURLProtocol.handler = nil }
        let api = SecretaryAPI(client: StubURLProtocol.makeClient())
        let from = CLLocationCoordinate2D(latitude: 56.19633, longitude: 10.2011)
        let to = CLLocationCoordinate2D(latitude: 56.1611, longitude: 10.2006)
        let arrive = try XCTUnwrap(Place.date(.string("2026-10-27T17:00:00Z")))
        _ = try await api.mapsRoutes(from: from, to: to, arrive: arrive, modes: [.transit, .walk])
        _ = try await api.mapsGeocode(q: "Møllestien & Aarhus", near: from)
        let routes = try XCTUnwrap(StubURLProtocol.requests.first)
        let items = URLComponents(url: try XCTUnwrap(routes.url), resolvingAgainstBaseURL: false)?.queryItems ?? []
        XCTAssertEqual(routes.httpMethod, "GET")
        XCTAssertEqual(routes.value(forHTTPHeaderField: "Authorization"), "Bearer sec_test")
        XCTAssertEqual(items.first { $0.name == "from" }?.value, "56.19633,10.2011")
        XCTAssertEqual(items.first { $0.name == "to" }?.value, "56.1611,10.2006")
        XCTAssertEqual(items.first { $0.name == "modes" }?.value, "transit,walk")
        XCTAssertEqual(items.first { $0.name == "arrive" }?.value, "2026-10-27T17:00:00Z")
        let geocode = try XCTUnwrap(StubURLProtocol.requests.last?.url)
        let query = URLComponents(url: geocode, resolvingAgainstBaseURL: false)?.queryItems ?? []
        XCTAssertEqual(query.first { $0.name == "q" }?.value, "Møllestien & Aarhus")
        XCTAssertEqual(query.first { $0.name == "lat" }?.value, "56.19633")
        XCTAssertEqual(query.first { $0.name == "lng" }?.value, "10.2011")
        StubURLProtocol.handler = { _ in .init(status: 503, body: #"{"error":"provider_unavailable"}"#) }
        do {
            _ = try await api.mapsRoutes(from: from, to: to, arrive: nil)
            XCTFail("provider failure must reach the view's MapKit fallback")
        } catch let error as APIError {
            XCTAssertEqual(error.status, 503)
        }
    }

    @MainActor
    func testRoutePolylineParticipatesInCameraFit() throws {
        let pin = MapPin(id: "place", coordinate: CLLocationCoordinate2D(latitude: 56.16, longitude: 10.2), title: "Place")
        let center = try XCTUnwrap(LucinaMap.fitCenter([pin], polyline: [[56.16, 10.2], [56.20, 10.24]]))
        XCTAssertEqual(center.latitude, 56.18, accuracy: 0.001)
        XCTAssertEqual(center.longitude, 10.22, accuracy: 0.001)
    }
}
