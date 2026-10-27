import XCTest
@testable import Secretary

/// The `place` block and `meta.place` (docs/lucina-message-format.md, the maps contract section 1).
final class PlaceTests: XCTestCase {
    private func json(_ s: String) throws -> JSONValue {
        try JSONDecoder().decode(JSONValue.self, from: Data(s.utf8))
    }

    func testFullBlock() throws {
        let place = try XCTUnwrap(Place.parse(json("""
        {"type":"place","title":"Hack Night: Rust meets AI","address":"Folkehuset Møllestien, Aarhus","lat":56.1611,"lng":10.2006,
         "when":"2026-10-27T17:00:00.000Z","end":"2026-10-27T20:00:00Z","eventId":"iphone:abc","kind":"event"}
        """)))
        XCTAssertEqual(place.title, "Hack Night: Rust meets AI")
        XCTAssertEqual(place.address, "Folkehuset Møllestien, Aarhus")
        XCTAssertEqual(place.lat, 56.1611)
        XCTAssertEqual(place.lng, 10.2006)
        XCTAssertEqual(place.when, Date(timeIntervalSince1970: 1_793_120_400))
        XCTAssertEqual(place.end, Date(timeIntervalSince1970: 1_793_131_200))
        XCTAssertEqual(place.eventId, "iphone:abc")
        XCTAssertEqual(place.kind, .event)
        XCTAssertEqual(place.plainText, "📍 Hack Night: Rust meets AI — Folkehuset Møllestien, Aarhus")
    }

    func testDropRules() throws {
        XCTAssertNil(Place.parse(try json(#"{"title":"","address":"Aarhus"}"#)), "a title is required")
        XCTAssertNil(Place.parse(try json(#"{"title":"Somewhere"}"#)), "an address or coordinates are required")
        XCTAssertNil(Place.parse(try json(#"{"title":"Somewhere","lat":56.1}"#)), "one coordinate is not a pair")
        XCTAssertNil(Place.parse(try json(#"{"title":"Null Island","lat":0,"lng":0}"#)), "(0,0) is dropped")
        XCTAssertNil(Place.parse(try json(#"{"title":"Far","lat":91,"lng":10}"#)), "out of range")
        XCTAssertNil(Place.parse(try json(#"["title"]"#)))

        let kept = try XCTUnwrap(Place.parse(json(#"{"title":"Aarhus C","address":"Banegårdspladsen","lat":0,"lng":0,"kind":"x","when":"tomorrow"}"#)))
        XCTAssertNil(kept.coordinate, "a bad pair is dropped, the address kept")
        XCTAssertEqual(kept.kind, .place, "an unknown kind is a place")
        XCTAssertNil(kept.when, "not RFC 3339")

        let long = try XCTUnwrap(Place.parse(json("""
        {"title":"\(String(repeating: "a", count: 200))","address":"line one\\nline two","lat":56.1,"lng":10.2,"eventId":"\(String(repeating: "e", count: 201))"}
        """)))
        XCTAssertEqual(long.title.count, 120)
        XCTAssertEqual(long.address, "line one line two", "one line")
        XCTAssertNil(long.eventId, "an event id over 200 chars is dropped")
    }

    func testRichMessageKeepsThreePlaces() throws {
        let block = #"{"type":"place","title":"P","address":"Aarhus"}"#
        let message = try XCTUnwrap(RichMessage.parse(json("""
        {"format":"v1","bubbles":["Here"],"blocks":[\(Array(repeating: block, count: 5).joined(separator: ",")),{"type":"quote","text":"q"}]}
        """)))
        XCTAssertEqual(message.blocks.filter { if case .place = $0 { return true } else { return false } }.count, 3)
        XCTAssertEqual(message.blocks.count, 4)
    }

    func testProactiveMetaPlace() throws {
        let meta = try JSONDecoder().decode(TimelineMeta.self, from: Data("""
        {"kind":"proactive","place":{"title":"Hack Night: Rust meets AI","address":"Folkehuset Møllestien, Aarhus","when":"2026-10-27T17:00:00Z","kind":"event"}}
        """.utf8))
        let place = try XCTUnwrap(meta.place)
        XCTAssertEqual(place.kind, .event)
        XCTAssertNil(place.coordinate)

        let bad = try JSONDecoder().decode(TimelineMeta.self, from: Data(#"{"kind":"proactive","place":{"title":"x"}}"#.utf8))
        XCTAssertNil(bad.place, "an invalid place does not fail the meta")

        let again = try JSONDecoder().decode(TimelineMeta.self, from: JSONEncoder().encode(meta))
        XCTAssertEqual(again.place, place, "the cached copy reads back the same")
    }

    func testMapsLinks() throws {
        let apple = try XCTUnwrap(Place.fromMapsURL(XCTUnwrap(URL(string: "https://maps.apple.com/?ll=56.1572,10.2107&q=Aarhus%20Domkirke")), name: nil))
        XCTAssertEqual(apple.title, "Aarhus Domkirke")
        XCTAssertEqual(apple.lat, 56.1572)

        let google = try XCTUnwrap(Place.fromMapsURL(XCTUnwrap(URL(string: "https://www.google.com/maps/place/Dokk1/@56.1532,10.2143,17z")), name: nil))
        XCTAssertEqual(google.title, "Dokk1")
        XCTAssertEqual(google.lng, 10.2143)

        XCTAssertNil(Place.fromMapsURL(XCTUnwrap(URL(string: "https://maps.app.goo.gl/abc")), name: nil), "no coordinates in a short link")
        XCTAssertNil(Place.fromMapsURL(XCTUnwrap(URL(string: "https://example.com/@56.1,10.2")), name: nil))
    }
}
