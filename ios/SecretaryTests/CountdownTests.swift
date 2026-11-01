import XCTest
@testable import Secretary

/// The countdown widget's arithmetic: days to the event's day, Today/Tomorrow, past events gone.
final class CountdownTests: XCTestCase {
    private var calendar: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "Europe/Copenhagen")!
        return calendar
    }()

    private func date(_ day: Int, _ hour: Int, _ minute: Int = 0) -> Date {
        calendar.date(from: DateComponents(year: 2026, month: 10, day: day, hour: hour, minute: minute))!
    }

    func testDaysCountCalendarDays() {
        let now = date(26, 23, 30)
        XCTAssertEqual(Countdown.days(until: date(26, 8), from: now, calendar: calendar), 0, "earlier today is still today")
        XCTAssertEqual(Countdown.days(until: date(27, 0, 10), from: now, calendar: calendar), 1, "tomorrow, though 40 minutes away")
        XCTAssertEqual(Countdown.days(until: date(31, 9), from: date(16, 18), calendar: calendar), 15)
        XCTAssertEqual(Countdown.days(until: date(25, 9), from: now, calendar: calendar), -1)
    }

    func testLabels() {
        XCTAssertEqual(Countdown.label(days: 0), String(localized: "Today"))
        XCTAssertEqual(Countdown.label(days: 1), String(localized: "Tomorrow"))
        XCTAssertTrue(Countdown.label(days: 15).contains("15"))
        XCTAssertNotEqual(Countdown.label(days: 2), Countdown.label(days: 1))
    }

    func testPastEventsDisappear() {
        let now = date(26, 12)
        let exam = CountdownEvent(id: "exam", title: "Esame di Reti", start: date(28, 9), end: date(28, 12))
        let morning = CountdownEvent(id: "morning", title: "Dentist", start: date(26, 9), end: date(26, 10))
        let afternoon = CountdownEvent(id: "afternoon", title: "Call", start: date(26, 15), end: date(26, 16))
        let holiday = CountdownEvent(id: "holiday", title: "Holiday", start: date(26, 0), allDay: true)
        let yesterday = CountdownEvent(id: "old", title: "Old", start: date(25, 0), allDay: true)
        let shown = Countdown.visible([exam, morning, afternoon, holiday, yesterday, exam], fresher: [], now: now, calendar: calendar)
        XCTAssertEqual(shown.map(\.id), ["holiday", "afternoon", "exam"], "soonest first, past ones and duplicates gone")
    }

    func testAMovedEventShowsItsNewTime() {
        let pinned = CountdownEvent(id: "exam", title: "Esame", start: date(28, 9))
        let moved = CountdownEvent(id: "exam", title: "Esame di Reti", start: date(30, 9))
        let shown = Countdown.visible([pinned], fresher: [moved], now: date(26, 12), calendar: calendar)
        XCTAssertEqual(shown, [moved])
    }

    func testTheWidgetRedrawsAtMidnightAndWhenEventsEnd() {
        let now = date(26, 12)
        let call = CountdownEvent(id: "call", title: "Call", start: date(26, 15), end: date(26, 16))
        let changes = Countdown.changes(after: now, events: [call], count: 3, calendar: calendar)
        XCTAssertEqual(changes, [date(26, 16), date(27, 0), date(28, 0), date(29, 0)])
    }

    func testAnEventPlaceCanBePinned() {
        let place = Place(title: "Esame di Reti", address: "Åbogade 34", when: date(28, 9), eventId: "ev-1", kind: .event)
        XCTAssertEqual(place.countdownEvent?.id, "ev-1")
        XCTAssertEqual(place.countdownEvent?.start, date(28, 9))
        XCTAssertNil(Place(title: "Dokk1", address: "Hack Kampmanns Plads 2").countdownEvent)
    }
}
