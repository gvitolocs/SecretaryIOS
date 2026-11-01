import XCTest
@testable import Secretary

final class CalendarDeviceEventTests: XCTestCase {
    func testTheEventKitIdComesOutOfTheMergedId() {
        XCTAssertEqual(CalendarMath.deviceEventIdentifier("iphone:6F79594D:ooae68g3@google.com@1793624400000"), "6F79594D:ooae68g3@google.com")
        XCTAssertEqual(CalendarMath.deviceEventIdentifier("iphone:317472A7:2020!261176-15@mytimetable.au.dk@1793602800000"), "317472A7:2020!261176-15@mytimetable.au.dk")
        XCTAssertEqual(CalendarMath.deviceEventIdentifier("iphone:plain-id"), "plain-id")
        XCTAssertNil(CalendarMath.deviceEventIdentifier("google:abc"))
        XCTAssertNil(CalendarMath.deviceEventIdentifier("iphone:"))
    }

    func testAlarmOffsets() {
        XCTAssertEqual(CalendarMath.alarmMinutes(offsets: [-3600, -86_400, 0, -3600, 300]), [0, 60, 1440])
        XCTAssertEqual(CalendarMath.alarmOffsets(minutes: [60, 0, 60]), [-0, -3600])
        XCTAssertEqual(CalendarMath.alarmOffsets(minutes: [1, 2, 3, 4, 5, 6]).count, 5)
    }
}

final class CalendarDetailTests: XCTestCase {
    private var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "Europe/Copenhagen")!
        c.locale = Locale(identifier: "en_GB")
        return c
    }
    private func event(_ id: String, _ start: String, _ end: String, title: String = "x") -> CalendarEntry {
        var e = CalendarEntry()
        e.id = id; e.title = title; e.source = "google"; e.start = start; e.end = end
        e.startMs = CalendarMath.parse(start)!.timeIntervalSince1970 * 1000
        e.endMs = CalendarMath.parse(end)!.timeIntervalSince1970 * 1000
        return e
    }
    private func tels(_ text: String) -> [String] { CalendarMath.phoneMatches(in: text).map(\.phone.tel) }

    func testPhoneNumbers() {
        XCTAssertEqual(tels("Ring 52 20 01 44 for the key"), ["tel:+4552200144"])
        XCTAssertEqual(tels("Tlf. 5220 0144"), ["tel:+4552200144"])
        XCTAssertEqual(tels("Call 52200144."), ["tel:+4552200144"])
        XCTAssertEqual(tels("Milano: +39 333 123 4567"), ["tel:+393331234567"])
        XCTAssertEqual(tels("+45 52 20 01 44 / 0039 02 1234 5678"), ["tel:+4552200144", "tel:+390212345678"])
        XCTAssertEqual(tels("+1 (415) 555-0100"), ["tel:+14155550100"])
        // Rooms, dates, times, postcodes, ids, links and emails are not phone numbers.
        XCTAssertEqual(tels("5220-014 Seminarlokale"), [])
        XCTAssertEqual(tels("52 20 01 4"), [])
        XCTAssertEqual(tels("2026-11-02 12:00–16:00, Universitetsbyen 76, 8000 Aarhus C"), [])
        XCTAssertEqual(tels("12.11.2026 kl. 14.00"), [])
        XCTAssertEqual(tels("Order 123456789 and 12345678x"), [])
        XCTAssertEqual(tels("https://teams.microsoft.com/l/meetup-join/19%3a52200144 or 52200144@example.com"), [])
        XCTAssertEqual(tels("Code 1234 5678"), [], "a Danish number starts with 2-9")
        XCTAssertEqual(tels("+45 1234"), [], "too short after +")
    }

    func testPhonesOfAnEventDedupeAndCap() {
        var e = event("google:a", "2026-11-02T13:00:00Z", "2026-11-02T14:30:00Z", title: "Call +45 52 20 01 44")
        e.location = "Reception 52200144"
        e.description = "<p>Or 22 33 44 55, 33 44 55 66, 44 55 66 77</p>"
        let phones = CalendarMath.phones(e)
        XCTAssertEqual(phones.map(\.tel), ["tel:+4552200144", "tel:+4522334455", "tel:+4533445566"])
        XCTAssertEqual(phones[0].display, "52200144")
    }

    func testNotesLinksAndFold() {
        let notes = "Join https://teams.microsoft.com/l/meetup-join/abc, mail anna@example.com, ring 22 33 44 55, Universitetsbyen 76, 8000 Aarhus C."
        let links = CalendarMath.noteLinks(notes)
        XCTAssertEqual(links.map(\.kind), ["link", "email", "phone", "address"])
        XCTAssertEqual(links[2].url.absoluteString, "tel:+4522334455")
        XCTAssertEqual(links[3].url.host, "maps.apple.com")
        let live = CalendarMath.liveNotes(notes)
        XCTAssertEqual(String(live.characters), notes)
        XCTAssertEqual(live.runs.filter { $0.link != nil }.count, 4)

        XCTAssertFalse(CalendarMath.notesFold("Short note"))
        XCTAssertTrue(CalendarMath.notesFold(String(repeating: "a", count: 281)))
        XCTAssertFalse(CalendarMath.notesFold(String(repeating: "a", count: 280)))
        XCTAssertTrue(CalendarMath.notesFold("1\n2\n3\n4\n5\n6"))
        XCTAssertFalse(CalendarMath.notesFold("1\n2\n3\n4\n5"))
    }

    func testDayStripLayout() throws {
        // Founders' Entry 14:00-15:30 (CET) beside the 12:00-16:00 seminar, an 08:00 class and an all-day event.
        let kitchen = event("google:k", "2026-11-02T13:00:00Z", "2026-11-02T14:30:00Z")
        let seminar = event("ics:s", "2026-11-02T11:00:00Z", "2026-11-02T15:00:00Z")
        let morning = event("ics:m", "2026-11-02T07:00:00Z", "2026-11-02T08:00:00Z")
        var holiday = event("ics:h", "2026-11-02T00:00:00Z", "2026-11-03T00:00:00Z"); holiday.allDay = true
        let strip = try XCTUnwrap(CalendarMath.dayStrip(for: kitchen, among: [seminar, morning, holiday, kitchen], calendar: calendar))
        XCTAssertEqual(strip.start, CalendarMath.parse("2026-11-02T12:00:00Z")) // 13:00 local
        XCTAssertEqual(strip.end, CalendarMath.parse("2026-11-02T16:00:00Z"))   // 17:00 local
        XCTAssertEqual(strip.hours.count, 5)
        XCTAssertEqual(strip.items.map(\.event.id), ["ics:s", "google:k"])
        XCTAssertEqual(strip.items.map(\.column), [0, 1])
        XCTAssertEqual(strip.items.map(\.columns), [2, 2])
        XCTAssertEqual(strip.items.map(\.current), [false, true])
        XCTAssertEqual(strip.items[0].start, strip.start, "clipped to the strip")

        // A short event alone: at least three hours, one column.
        let short = event("lucina:a", "2026-11-02T09:15:00Z", "2026-11-02T09:30:00Z")
        let alone = try XCTUnwrap(CalendarMath.dayStrip(for: short, among: [], calendar: calendar))
        XCTAssertEqual(alone.end.timeIntervalSince(alone.start), 3 * 3600)
        XCTAssertEqual(alone.items.map(\.columns), [1])
        XCTAssertNil(CalendarMath.dayStrip(for: holiday, among: [], calendar: calendar))
    }

    func testSettingsDecodeEncodeAndApply() throws {
        let json = #"{"id":"google:a","source":"google","title":"x","startMs":0,"endMs":1,"alarms":[1440,60,-5,"x"],"alarmsDefault":false,"free":true,"travelMin":25,"leaveAlert":true}"#
        let e = try Fixtures.decode(CalendarEntry.self, json)
        XCTAssertEqual(e.alarms, [60, 1440])
        XCTAssertTrue(e.free && e.leaveAlert)
        XCTAssertEqual(e.travelMin, 25)
        let plain = try Fixtures.decode(CalendarEntry.self, #"{"id":"a","travelMin":0}"#)
        XCTAssertEqual(plain.alarms, [])
        XCTAssertNil(plain.travelMin)

        func body(_ p: CalendarSettingsPatch) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(p)) as? [String: Any])
        }
        XCTAssertEqual(try body(CalendarSettingsPatch(free: true)).keys.sorted(), ["confirm", "free"])
        let defaults = try body(CalendarSettingsPatch(alarms: .some(nil)))
        XCTAssertTrue(defaults["alarms"] is NSNull)
        XCTAssertEqual(try body(CalendarSettingsPatch(alarms: .some([15]), travelMin: 0))["alarms"] as? [Int], [15])

        let applied = CalendarSettingsPatch(alarms: .some([60, 15, 60]), travelMin: 0, leaveAlert: false).applied(to: e)
        XCTAssertEqual(applied.alarms, [15, 60])
        XCTAssertNil(applied.travelMin)
        XCTAssertFalse(applied.leaveAlert)
        XCTAssertTrue(CalendarSettingsPatch(alarms: .some(nil)).applied(to: e).alarmsDefault)
    }

    func testLabels() {
        XCTAssertEqual(CalendarMath.travelMinutes(seconds: 17 * 60 + 5), 20)
        XCTAssertEqual(CalendarMath.travelMinutes(seconds: 30), 5)
        XCTAssertNil(CalendarMath.travelMinutes(seconds: 0))
        let day = CalendarMath.longDay(CalendarMath.parse("2026-11-02T13:00:00Z")!, calendar: calendar, locale: Locale(identifier: "en_GB"))
        XCTAssertTrue(day.contains("Monday") && day.contains("2 November") && day.contains("2026"), day)
        XCTAssertFalse(CalendarMath.alarmLabel(60).isEmpty)
        XCTAssertNotEqual(CalendarMath.alarmLabel(0), CalendarMath.alarmLabel(5))
    }
}
