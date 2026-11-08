import XCTest
@testable import Secretary

/// Answers captured from secretary-server itself (main 8e92fac, a fresh data dir), so the
/// decoders are checked against the real server and not only against the web mock.
final class LiveShapesTests: XCTestCase {
    func testNotificationRoutes() throws {
        let page = try Fixtures.decode(NotificationPage.self, #"{"items":[],"nextBefore":null,"unread":0}"#)
        XCTAssertTrue(page.items.isEmpty)
        XCTAssertNil(page.nextBefore)
        let read = try Fixtures.decode(ReadResult.self, #"{"unread":0,"updated":0}"#)
        XCTAssertEqual(read.updated, 0)

        let state = try Fixtures.decode(NotificationSettingsState.self, #"""
        {"apns":{"enabled":false,"environment":null},"categories":[{"defaultOn":true,"id":"lucina_message"},{"defaultOn":false,"id":"system"}],
         "currentDevice":{"id":"dev_LahTP5pEqxg","name":"probe"},"present":false,
         "settings":{"calendarLeadMinutes":10,"categories":{"calendar":true,"device_confirm":true,"lucina_message":true,"presence":false,"reminder":true,"shopping":true,"system":false},
                     "devices":{},"digest":true,"dndUntil":null,"language":"en","previews":true,"quietHours":{"enabled":false,"end":"07:30","start":"22:30"},
                     "remindersBypassQuiet":true,"timeZone":"Europe/Rome"},
         "subscriptions":[{"id":"ps_1","kind":"webpush","deviceId":"dev_x","deviceName":"Mac","label":"Safari","service":"web.push.apple.com",
                           "createdAt":1792654200000,"lastSuccessAt":null,"failures":0,"lastError":null}],
         "webPush":{"enabled":true,"publicKey":"BCGM"}}
        """#)
        XCTAssertEqual(state.currentDevice?.id, "dev_LahTP5pEqxg")
        XCTAssertEqual(state.apns?.enabled, false)
        XCTAssertEqual(state.subscriptions.map(\.service), ["web.push.apple.com"])
        XCTAssertFalse(state.settings.isOn(.system))
        XCTAssertNil(state.settings.dndUntil)

        // The PUT body is the whole settings object, `dndUntil: null` included (the server accepted this shape).
        let body = try JSONSerialization.jsonObject(with: APIClient.encoder.encode(state.settings)) as? [String: Any]
        XCTAssertEqual(Set(body?.keys.map { $0 } ?? []), ["categories", "quietHours", "timeZone", "dndUntil", "calendarLeadMinutes",
                                                           "remindersBypassQuiet", "digest", "previews", "language", "devices"])
        XCTAssertTrue(body?["dndUntil"] is NSNull)
    }

    func testLinksAndCalendar() throws {
        let links = try Fixtures.decode(LinksOverview.self, #"""
        {"google":{"account":null,"configured":false,"services":[],"status":"disconnected"},
         "groups":[{"cards":[{"actions":[],"detail":"Google sign-in is not configured on the server yet","id":"google-calendar","kind":"google","name":"Google Calendar","service":"calendar","status":"unavailable"},
                             {"account":null,"actions":[],"count":0,"detail":"Open the Secretary app on the iPhone and allow Calendar access","id":"iphone-calendar","kind":"iphone","lastSyncAt":null,"name":"iPhone Calendar","scopes":["eventkit.read","eventkit.write"],"status":"disconnected"}],"id":"calendars"},
                   {"cards":[{"actions":[],"detail":"Lucina's Telegram bot; reminders are delivered here","id":"channel:telegram","kind":"channel","name":"Telegram","status":"connected"}],"id":"channels"},
                   {"cards":[{"actions":[],"detail":"Lucina MCP status is not available","id":"ai:claude-code","kind":"ai","name":"Claude Code","status":"unknown"}],"id":"ai"},
                   {"cards":[],"id":"custom"}],
         "proposals":[{"id":"prop_1","event":{"title":"Dentist","start":"2026-10-23T09:00:00+02:00"},"from":"mcp:claude-code","createdAt":"2026-10-22T10:00:00.000Z","expiresAtMs":1793259000000}],
         "timeZone":"Europe/Copenhagen"}
        """#)
        XCTAssertFalse(links.googleConfigured)
        XCTAssertEqual(links.groups.map(\.id), ["calendars", "channels", "ai", "custom"])
        XCTAssertEqual(links.groups[0].cards.map(\.status), ["unavailable", "disconnected"])
        XCTAssertNil(links.groups[0].cards[1].account)
        XCTAssertNil(links.groups[0].cards[1].lastSyncAt)
        XCTAssertEqual(links.proposals.map(\.event.title), ["Dentist"])

        let page = try Fixtures.decode(EventsPage.self, #"""
        {"events":[{"id":"google:abc_20261022T070000Z","source":"google","sourceId":"abc","calendar":"Google","title":"Gym",
                    "start":"2026-10-22T07:00:00.000Z","end":"2026-10-22T08:00:00.000Z","allDay":false,"startMs":1792652400000,"endMs":1792656000000,
                    "recurring":true,"editable":true},
                   {"id":"ics:s:1","source":"ics","title":"Holiday","start":"2026-10-24","end":"2026-10-25","allDay":true,"startMs":1792792800000,
                    "endMs":1792879200000,"recurring":false,"editable":false}],
         "from":"2026-10-21T22:00:00.000Z","sources":{"google":null,"ics":[],"iphone":{"device":null,"reportedAt":null}},"timeZone":"Europe/Copenhagen","to":"2026-10-28T22:00:00.000Z"}
        """#)
        XCTAssertEqual(page.events.map(\.title), ["Gym", "Holiday"])
        XCTAssertNil(page.events[1].calendar)
        XCTAssertTrue(page.events[1].allDay)
    }

    func testAvatarAndWardrobe() throws {
        let settings = AvatarSettings(json: try Fixtures.decode(JSONValue.self, #"""
        {"outfit":null,"hair":null,"hairColor":null,"auto":true,"rules":{"gym":{"outfit":"gym","hair":"ponytail"},"shopping":{"outfit":"chef","hair":"natural"},
         "default":{"outfit":"uniform","hair":"natural"}},"updatedMs":0,"updatedBy":""}
        """#))
        XCTAssertNil(settings.outfit)
        XCTAssertTrue(settings.auto)
        XCTAssertEqual(settings.rules.gym.outfit, "gym")
        XCTAssertEqual(settings.rules.shopping.outfit, "chef")

        let wardrobe = WardrobeFile(json: try Fixtures.decode(JSONValue.self, #"""
        {"colors":[{"hex":"#ff8ac8","id":"pink","name":{"en":"Pink"}}],"hairstyles":[{"id":"natural"},{"glb":"p.glb","hideTiara":true,"id":"ponytail","name":{"en":"Ponytail"}}],
         "outfits":[{"glb":"lucina.glb","id":"uniform","name":{"en":"Uniform"},"tags":["uniform"]},{"glb":"lucina-gym.glb","id":"gym","name":{"en":"Gym"},"tags":["sport"]}]}
        """#))
        XCTAssertEqual(wardrobe.outfits.map(\.id), ["uniform", "gym"])
        XCTAssertEqual(wardrobe.hairstyles.map(\.id), ["natural", "ponytail"])
        XCTAssertEqual(wardrobe.colors.map(\.id), ["pink"])
    }

    func testNotes() async throws {
        let note = try Fixtures.decode(Note.self, #"""
        {"id":"n_109acc87c4cd5af12caa","slug":"probe-note","title":"Probe note","content":"# Hi\n\nbody","tags":["work"],"source":"owner","pinned":true,
         "createdAt":"2026-10-22T13:14:37.264Z","updatedAt":"2026-10-22T13:14:37.264Z"}
        """#)
        XCTAssertEqual(note.slug, "probe-note")
        XCTAssertNotNil(note.updated)

        StubURLProtocol.handler = { _ in
            .init(body: ##"""
            {"days":30,"notes":[{"createdAt":"2026-10-22T13:14:37.264Z","deletedAt":"2026-10-22T13:14:37.295Z","excerpt":"# Hi body","id":"n_109acc87c4cd5af12caa",
             "pinned":true,"purgeAfter":"2026-11-21T13:14:37.295Z","size":10,"slug":"probe-note","source":"owner","tags":["work"],"title":"Probe note",
             "updatedAt":"2026-10-22T13:14:37.264Z"}]}
            """##)
        }
        let trash = try await NotesAPI(client: StubURLProtocol.makeClient()).trash()
        XCTAssertEqual(trash.map(\.slug), ["probe-note"])
        XCTAssertNotNil(trash.first?.purgeDate)
        XCTAssertEqual(trash.first?.size, 10)
    }
}
