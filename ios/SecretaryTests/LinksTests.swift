import XCTest
@testable import Secretary

@MainActor
final class LinksTests: XCTestCase {
    static let overview = #"""
    {"timeZone": "Europe/Rome",
     "google": {"configured": true, "status": "reconnect", "account": "owner@example.com", "services": ["calendar"]},
     "groups": [
       {"id": "calendars", "cards": [
         {"id": "google-calendar", "kind": "google", "service": "calendar", "name": "Google Calendar", "status": "reconnect",
          "account": "owner@example.com", "scopes": ["calendar.events", "calendar.readonly"],
          "lastSyncAt": "2026-10-22T09:00:00.000Z", "count": 57, "actions": ["reconnect", "disconnect"]},
         {"id": "ics:ics_1", "kind": "ics", "name": "Serie A", "status": "connected", "count": 38, "actions": ["sync", "remove"]},
         {"kind": "broken card without id"}
       ]},
       {"id": "custom", "cards": [
         {"id": "custom:lnk_1", "kind": "bookmark", "name": "Bank", "status": "connected", "url": "https://bank.example",
          "icon": "🏦", "tags": ["money"], "actions": ["edit", "remove"], "newField": 1}
       ]}
     ],
     "proposals": [{"id": "prop_1", "event": {"title": "Padel", "start": "2026-11-07T10:00:00+01:00"}, "from": "mcp:claude-code",
                    "createdAt": "2026-10-22T10:00:00Z", "expiresAtMs": 1}]}
    """#

    override func setUp() {
        StubURLProtocol.requests = []
    }

    func testOverviewDecodesDefensively() throws {
        let o = try Fixtures.decode(LinksOverview.self, Self.overview)
        XCTAssertTrue(o.googleConfigured)
        XCTAssertEqual(o.groups.map(\.id), ["calendars", "custom"])
        XCTAssertEqual(o.groups[0].cards.count, 2, "a malformed card is skipped, not fatal")
        let google = o.groups[0].cards[0]
        XCTAssertEqual(google.status, "reconnect")
        XCTAssertEqual(google.actions, ["reconnect", "disconnect"])
        XCTAssertEqual(google.lastSyncAt, LinkDates.parse("2026-10-22T09:00:00Z"))
        let bank = o.groups[1].cards[0]
        XCTAssertEqual(bank.url, URL(string: "https://bank.example"))
        XCTAssertEqual(bank.actions, ["remove"], "edit is not offered in the app yet")
        XCTAssertEqual(o.proposals.first?.from, "mcp:claude-code")
        XCTAssertEqual(LinkRow.displayName(google), "Google Calendar")
    }

    func testGoogleReturnURLs() {
        XCTAssertEqual(GoogleReturn(url: URL(string: "secretary://links?google=connected")!), .connected)
        XCTAssertEqual(GoogleReturn(url: URL(string: "secretary://links?google=error&reason=access_denied")!), .failed("access_denied"))
        XCTAssertNil(GoogleReturn(url: URL(string: "secretary://other?google=connected")!))
        XCTAssertNil(GoogleReturn(url: URL(string: "https://links?google=connected")!))
    }

    func testStoreLoadsAndSendsConfirmedRequests() async throws {
        StubURLProtocol.handler = { request in
            switch (request.httpMethod ?? "GET", request.url?.path ?? "") {
            case ("GET", "/v1/links"): return .init(body: Self.overview)
            case ("GET", "/v1/calendar/events"):
                return .init(body: #"{"events": [{"id": "google:a", "source": "google", "title": "Dentist", "start": "2026-10-23T07:30:00.000Z", "allDay": false, "startMs": 1792740600000}]}"#)
            default: return .init(status: 201, body: #"{"ok": true}"#)
            }
        }
        let store = LinksStore(client: StubURLProtocol.makeClient())
        await store.load()
        XCTAssertEqual(store.overview?.groups.count, 2)
        XCTAssertEqual(store.events?.first?.title, "Dentist")
        XCTAssertNil(store.error)

        let proposal = try XCTUnwrap(store.overview?.proposals.first)
        await store.answer(proposal, accept: true)
        let accept = try XCTUnwrap(StubURLProtocol.requests.first { $0.url?.path == "/v1/calendar/proposals/prop_1/accept" })
        XCTAssertEqual(accept.httpMethod, "POST")
        let body = try JSONSerialization.jsonObject(with: accept.httpBody ?? Data()) as? [String: Any]
        XCTAssertEqual(body?["confirm"] as? Bool, true)

        let ics = try XCTUnwrap(store.overview?.groups[0].cards[1])
        await store.remove(ics)
        XCTAssertTrue(StubURLProtocol.requests.contains { $0.httpMethod == "DELETE" && $0.url?.path == "/v1/links/ics/ics_1" })

        let added = await store.addSubscription(label: "Calcio", url: "webcal://cal.example/x.ics")
        XCTAssertTrue(added)
        let post = try XCTUnwrap(StubURLProtocol.requests.last { $0.url?.path == "/v1/links/ics" })
        let sent = try JSONSerialization.jsonObject(with: post.httpBody ?? Data()) as? [String: Any]
        XCTAssertEqual(sent?["url"] as? String, "webcal://cal.example/x.ics")
    }

    func testErrorsSurface() async {
        StubURLProtocol.handler = { _ in .init(status: 409, body: #"{"error": {"code": "google_reconnect_needed", "message": "reconnect it in Links"}}"#) }
        let store = LinksStore(client: StubURLProtocol.makeClient())
        await store.load()
        XCTAssertNil(store.overview)
        XCTAssertNotNil(store.error)
    }
}
