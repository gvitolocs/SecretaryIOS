import XCTest
@testable import Secretary

final class CalendarTests: XCTestCase {
    private var calendar: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "Europe/Copenhagen")!
        c.locale = Locale(identifier: "en_GB")
        c.firstWeekday = 2
        return c
    }
    private func date(_ value: String) -> Date { CalendarMath.parse(value)! }
    private func event(id: String = "lucina:ev_1", start: String = "2026-10-30T13:30:00Z", end: String = "2026-10-30T14:30:00Z", allDay: Bool = false) -> CalendarEntry {
        var e = CalendarEntry()
        e.id = id; e.title = "Mentor"; e.source = "lucina"; e.editable = true
        e.start = start; e.end = end; e.allDay = allDay
        e.startMs = (CalendarMath.parse(start) ?? CalendarMath.day(start, calendar: calendar)!).timeIntervalSince1970 * 1000
        e.endMs = (CalendarMath.parse(end) ?? CalendarMath.day(end, calendar: calendar)!).timeIntervalSince1970 * 1000
        return e
    }
    func testRealShapedPageAndEnvelope() throws {
        let json = #"""
        {"from":"2026-10-01T00:00:00Z","to":"2026-11-01T00:00:00Z","timeZone":"Europe/Rome",
         "sources":{"lucina":{"count":1},"google":{"status":"connected"},"ics":[{"id":"school","label":"School"}]},
         "events":[
          {"id":"lucina:ev_1","source":"lucina","sourceId":"ev_1","calendar":"Lucina","title":"Mentor","start":"2026-10-30T13:30:00.000Z","end":"2026-10-30T14:30:00.000Z","startMs":1793367000000,"endMs":1793370600000,"allDay":false,"editable":true,"addedBy":"Lucina","conferenceUrl":"https://meet.google.com/abc","attendees":["guest@example.com"],"future":42},
          {"id":"ics:school:abc","source":"ics","title":"Holiday","start":"2026-10-30","end":"2026-11-02","allDay":true},
          {"id":"google:abc_20261022T070000Z","source":"google","sourceId":"abc","calendar":"Google","title":"Gym","start":"2026-10-22T07:00:00.000Z","end":"2026-10-22T08:00:00.000Z","startMs":1792652400000,"endMs":1792656000000,"recurring":true,"editable":true},
          {"id":"iphone:raw@1793367000000","source":"iphone","calendar":"Home","title":"Dentist","startMs":1793367000000,"endMs":1793370600000,"editable":false},
          {"id":42,"source":"future","title":null,"allDay":"unknown","editable":"unknown"}
         ]}
        """#
        let page = try Fixtures.decode(CalendarEventsPage.self, json)
        XCTAssertEqual(page.events.count, 5)
        XCTAssertEqual(page.timeZone, "Europe/Rome")
        XCTAssertEqual(page.events[0].startDate, Date(timeIntervalSince1970: 1793367000))
        XCTAssertEqual(page.events[0].attendees, ["guest@example.com"])
        XCTAssertEqual(page.events[0].addedBy, "Lucina")
        XCTAssertEqual(page.events[1].start, "2026-10-30")
        XCTAssertTrue(page.events[2].recurring)
        XCTAssertFalse(page.events[3].editable)
        XCTAssertEqual(page.events[4].id, "42")
        XCTAssertFalse(page.events[4].allDay)
        XCTAssertEqual(page.sources?["ics"]?.arrayValue?.first?["label"]?.stringValue, "School")
        let wrapped = try Fixtures.decode(CalendarEnvelope<CalendarEventsPage>.self, "{\"ok\":true,\"data\":" + json + "}")
        XCTAssertEqual(wrapped.value.events, page.events)
        XCTAssertThrowsError(try Fixtures.decode(CalendarEnvelope<CalendarEventsPage>.self, #"{"ok":false,"data":{}}"#))
    }
    func testDraftEncodingOmitsUnsetAndClearsEditingFields() throws {
        let draft = CalendarEventDraft(title: "Gym", start: "2026-10-30T13:00:00Z")
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(draft)) as? [String: Any])
        XCTAssertEqual(Set(json.keys), ["title", "start"])
        var form = CalendarForm(day: date("2026-10-30T12:00:00Z"))
        form.title = " Gym "; form.location = " "; form.notes = ""; form.meeting = ""
        let edit = form.draft(editing: true, calendar: calendar)
        let edited = try XCTUnwrap(JSONSerialization.jsonObject(with: JSONEncoder().encode(edit)) as? [String: Any])
        XCTAssertEqual(edited["confirm"] as? Bool, true)
        XCTAssertEqual(edited["location"] as? String, "")
        XCTAssertEqual(edited["description"] as? String, "")
        XCTAssertEqual(edited["conferenceUrl"] as? String, "")
        XCTAssertEqual(edit.title, "Gym")
        XCTAssertNil(form.draft(editing: false).confirm)
        XCTAssertNil(form.draft(editing: false).conferenceUrl)
    }
    func testProposalsDecode() throws {
        let p = try Fixtures.decode(CalendarPendingProposal.self, #"{"id":"p_1","event":{"title":"Lunch","start":"2026-10-30T12:00:00Z","location":"Cafe"},"from":"Lucina","createdAt":"2026-10-29T10:00:00Z","expiresAtMs":1794000000000,"reason":"Suggested time","unknown":true}"#)
        XCTAssertEqual(p.event.title, "Lunch")
        XCTAssertEqual(p.from, "Lucina")
        XCTAssertEqual(p.reason, "Suggested time")
        XCTAssertFalse(CalendarMath.proposalWhen(p, calendar: calendar, locale: Locale(identifier: "en_GB")).isEmpty)
        let allDay = try Fixtures.decode(CalendarPendingProposal.self, #"{"id":"all","event":{"title":"Holiday","start":"2026-10-30"}}"#)
        XCTAssertFalse(CalendarMath.proposalWhen(allDay, calendar: calendar).isEmpty)
        let missing = try Fixtures.decode(CalendarPendingProposal.self, #"{"id":7}"#)
        XCTAssertEqual(missing.id, "7")
        XCTAssertNil(missing.event.start)
        XCTAssertEqual(CalendarMath.proposalWhen(missing), "")
    }
    func testAllDayUsesLocalDatesAndExclusiveEndAcrossDST() {
        let e = event(start: "2026-10-24", end: "2026-10-27", allDay: true)
        for day in ["2026-10-24", "2026-10-25", "2026-10-26"] {
            XCTAssertEqual(CalendarMath.events([e], on: CalendarMath.day(day, calendar: calendar)!, calendar: calendar).count, 1)
        }
        XCTAssertTrue(CalendarMath.events([e], on: CalendarMath.day("2026-10-27", calendar: calendar)!, calendar: calendar).isEmpty)
        XCTAssertTrue(CalendarMath.events([e], on: CalendarMath.day("2026-10-23", calendar: calendar)!, calendar: calendar).isEmpty)
        XCTAssertEqual(CalendarMath.ymd(CalendarMath.bounds(e, calendar: calendar).0, calendar: calendar), "2026-10-24")
        var form = CalendarForm(day: e.startDate, event: e, calendar: calendar)
        XCTAssertEqual(CalendarMath.ymd(form.end, calendar: calendar), "2026-10-26")
        form.title = "Holiday"
        XCTAssertEqual(form.draft(editing: true, calendar: calendar).end, "2026-10-27")
    }
    func testMonthGridHasSixWeeksAndLocaleFirstWeekday() {
        let days = CalendarMath.monthDays(date("2026-10-15T12:00:00Z"), calendar: calendar)
        XCTAssertEqual(days.count, 42)
        XCTAssertEqual(CalendarMath.ymd(days.first!, calendar: calendar), "2026-09-28")
        XCTAssertEqual(CalendarMath.ymd(days.last!, calendar: calendar), "2026-11-08")
        XCTAssertEqual(calendar.component(.weekday, from: days[0]), 2)
        var sunday = calendar; sunday.firstWeekday = 1
        XCTAssertEqual(CalendarMath.ymd(CalendarMath.monthDays(days[17], calendar: sunday)[0], calendar: sunday), "2026-09-27")
    }
    func testAgendaGroupingAllDayFirstAndTimedOrder() {
        let later = event(id: "late", start: "2026-10-30T15:00:00Z", end: "2026-10-30T16:00:00Z")
        let early = event(id: "early")
        let all = event(id: "all", start: "2026-10-30", end: "2026-11-01", allDay: true)
        var alpha = early; alpha.id = "alpha"; alpha.title = "Alpha"
        var sameTitle = alpha; sameTitle.id = "a"
        let groups = CalendarMath.agenda([later, early, all, alpha, sameTitle], from: date("2026-10-30T00:00:00Z"), to: date("2026-11-01T00:00:00Z"), calendar: calendar)
        XCTAssertEqual(groups.count, 2)
        XCTAssertEqual(groups[0].events.map(\.id), ["all", "a", "alpha", "early", "late"])
        XCTAssertEqual(groups[1].events.map(\.id), ["all"])
    }
    func testLabelsArePinnedToCalendarAndLocale() {
        let locale = Locale(identifier: "en_GB")
        let now = date("2026-10-30T09:00:00Z")
        let e = event()
        let label = CalendarMath.whenLabel(e, now: now, calendar: calendar, locale: locale)
        let time = CalendarMath.format(e.startDate, template: "HHmm", calendar: calendar, locale: locale)
        let end = CalendarMath.format(e.endDate, template: "HHmm", calendar: calendar, locale: locale)
        XCTAssertEqual(label, String(localized: "Today") + " · " + time + "–" + end)
        XCTAssertEqual(CalendarMath.dayLabel(date("2026-10-31T12:00:00Z"), now: now, calendar: calendar), String(localized: "Tomorrow"))
        let multi = event(start: "2026-10-30", end: "2026-11-02", allDay: true)
        XCTAssertEqual(CalendarMath.whenLabel(multi, now: now, calendar: calendar, locale: locale), String(localized: "Today") + " – " + CalendarMath.dayLabel(CalendarMath.day("2026-11-01", calendar: calendar)!, now: now, calendar: calendar, locale: locale))
        let overnight = event(start: "2026-10-30T22:30:00Z", end: "2026-10-31T01:00:00Z")
        XCTAssertTrue(CalendarMath.whenLabel(overnight, now: now, calendar: calendar, locale: locale).contains(String(localized: "Tomorrow")))
    }
    func testDefaultStartShiftAndValidation() {
        let start = date("2026-10-30T13:30:00Z"), end = start.addingTimeInterval(3600), next = start.addingTimeInterval(7200)
        XCTAssertEqual(CalendarMath.shiftEnd(start: start, end: end, next: next, allDay: false), next.addingTimeInterval(3600))
        XCTAssertEqual(CalendarMath.shiftEnd(start: start, end: start, next: next, allDay: false), next.addingTimeInterval(900))
        XCTAssertFalse(CalendarMath.validEnd(start: start, end: start, allDay: false))
        XCTAssertFalse(CalendarMath.validEnd(start: start, end: start.addingTimeInterval(-1), allDay: false))
        XCTAssertTrue(CalendarMath.validEnd(start: start, end: start, allDay: true, calendar: calendar))
        XCTAssertEqual(CalendarMath.defaultStart(start, now: start, calendar: calendar), date("2026-10-30T14:00:00Z"))
        let another = date("2026-10-31T12:00:00Z")
        XCTAssertEqual(calendar.component(.hour, from: CalendarMath.defaultStart(another, now: start, calendar: calendar)), 9)
        let d1 = CalendarMath.day("2026-10-24", calendar: calendar)!, d2 = CalendarMath.day("2026-10-26", calendar: calendar)!, d3 = CalendarMath.day("2026-10-31", calendar: calendar)!
        XCTAssertEqual(CalendarMath.ymd(CalendarMath.shiftEnd(start: d1, end: d2, next: d3, allDay: true, calendar: calendar), calendar: calendar), "2026-11-02")
        var form = CalendarForm(day: start); XCTAssertNotNil(form.error)
        form.title = "Title"; form.start = start; form.end = start
        XCTAssertNotNil(form.error)
        form.end = end; XCTAssertNil(form.error)
    }
    func testCalendarLinksAndPublicLink() throws {
        for route in ["/calendar?event=lucina%3Aev_abc&at=1793367000000", "calendar?event=lucina%3Aev_abc&at=1793367000000", "https://getsecretary.tech/calendar?event=lucina%3Aev_abc&at=1793367000000"] {
            let link = try XCTUnwrap(CalendarLink.parse(route))
            XCTAssertEqual(link.eventId, "lucina:ev_abc")
            XCTAssertEqual(link.at, Date(timeIntervalSince1970: 1793367000))
        }
        XCTAssertNotNil(CalendarLink.parse("/calendar"))
        XCTAssertNotNil(CalendarLink.parse("calendar?at=1"))
        for junk in ["/calendar/extra", "//calendar", "https://evil.example/calendar", "https://getsecretary.tech.evil/calendar", "https://getsecretary.tech/calendar#junk", "/calendar?at=nan", "/calendar?event=", "junk", "http://getsecretary.tech/calendar"] {
            XCTAssertNil(CalendarLink.parse(junk), junk)
        }
        let e = event(id: "iphone:A:B@1793367000000")
        XCTAssertTrue(e.publicLink.absoluteString.contains("event=iphone%3AA%3AB%40"))
        XCTAssertEqual(CalendarLink.parse(e.publicLink.absoluteString)?.eventId, e.id)
        XCTAssertEqual(CalendarLink.parse(e.publicLink.absoluteString)?.at, e.startDate)
        XCTAssertEqual(CalendarAPI.segment("lucina:a/b?"), "lucina%3Aa%2Fb%3F")
        XCTAssertEqual(CalendarMath.parse(CalendarMath.instant(e.startDate)), e.startDate)
    }
    func testDeepLinkMatchesExactCoreAndOccurrence() {
        let e = event(id: "google:abc")
        XCTAssertEqual(CalendarMath.idCore("iphone:abc@1793367000000"), "abc")
        XCTAssertEqual(CalendarMath.matchDeepLink([e], id: e.id, at: nil), e)
        XCTAssertEqual(CalendarMath.matchDeepLink([e], id: "iphone:abc@1793367000000", at: e.startDate), e)
        XCTAssertEqual(CalendarMath.matchDeepLink([e], id: "iphone:abc", at: nil), e)
        XCTAssertEqual(CalendarMath.matchDeepLink([e], id: "unknown", at: e.startDate), e)
        var other = e; other.id = "other"
        XCTAssertNil(CalendarMath.matchDeepLink([e, other], id: "unknown", at: e.startDate))
        var occurrence = e; occurrence.startMs += 86_400_000
        XCTAssertEqual(CalendarMath.matchDeepLink([e, occurrence], id: e.id, at: occurrence.startDate), occurrence)
        XCTAssertNil(CalendarMath.matchDeepLink([e], id: "absent", at: nil))
    }
    func testNotesMeetingAndCalendarNames() {
        XCTAssertEqual(CalendarMath.plainNotes("<p>A &amp; B</p><br><div>&lt;Hi&gt; &quot;yes&quot; &#39;x&#39;&nbsp;&apos;</div>"), "A & B\n\n<Hi> \"yes\" 'x' '")
        XCTAssertEqual(CalendarMath.plainNotes(nil), "")
        XCTAssertNil(CalendarMath.day("2026-02-30", calendar: calendar))
        XCTAssertNil(CalendarMath.day("junk", calendar: calendar))
        XCTAssertNil(CalendarMath.parse("junk"))
        XCTAssertEqual(CalendarMath.color(name: "Lucina", source: "lucina"), Theme.accent)
        XCTAssertEqual(CalendarMath.color(name: "", source: "google"), CalendarMath.color(name: "Google", source: "google"))
        XCTAssertEqual(CalendarMath.color(name: "Home", source: "iphone"), CalendarMath.color(name: "Home", source: "ics"))
        var e = event(); e.description = "Join https://acme.zoom.us/j/123."
        XCTAssertEqual(CalendarMath.meetingURL(e)?.absoluteString, "https://acme.zoom.us/j/123")
        e.conferenceUrl = "https://meet.google.com/abc"
        XCTAssertEqual(CalendarMath.meetingURL(e)?.host, "meet.google.com")
        e.conferenceUrl = nil; e.description = "https://zoom.us.evil.example/j"; XCTAssertNil(CalendarMath.meetingURL(e))
        XCTAssertEqual(e.calendarName, "Lucina")
        e.source = "google"; XCTAssertEqual(e.calendarName, "Google")
        e.source = "iphone"; e.calendar = " Home "; XCTAssertEqual(e.calendarName, "Home")
        e.calendar = nil; XCTAssertEqual(e.calendarName, "iPhone")
        XCTAssertEqual(CalendarMath.colorIndex("Home"), CalendarMath.colorIndex("Home"))
        XCTAssertTrue((0..<10).contains(CalendarMath.colorIndex("Aarhus Universitet")))
    }
    @MainActor func testHiddenCalendarsPersistInIsolatedDefaults() throws {
        let suite = "CalendarTests." + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = CalendarStore(api: { nil }, defaults: defaults)
        XCTAssertTrue(store.hidden.isEmpty)
        store.toggle("Home")
        XCTAssertEqual(store.hidden, ["Home"])
        let restored = CalendarStore(api: { nil }, defaults: defaults)
        XCTAssertTrue(restored.hidden.contains("Home"))
        restored.toggle("Home")
        XCTAssertTrue(restored.hidden.isEmpty)
    }
    @MainActor func testNotificationRouting() {
        XCTAssertEqual(NotificationRoute.tab(for: "/calendar?event=x&at=1"), .plugins)
        XCTAssertEqual(NotificationRoute.calendar("/calendar?event=x&at=1")?.eventId, "x")
    }
}
