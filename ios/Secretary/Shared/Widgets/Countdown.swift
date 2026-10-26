import Foundation

// The countdown widget ("15 days — Network exam"): calendar events the owner pins in the app
// (an event's map screen, a long-press, the Countdown plugin) or picks in the widget's own
// configuration. Compiled into the app and the widget extension.

/// One event to count down to.
struct CountdownEvent: Codable, Equatable, Hashable, Sendable, Identifiable {
    var id: String
    var title: String
    var start: Date
    var end: Date?
    var allDay: Bool

    init(id: String, title: String, start: Date, end: Date? = nil, allDay: Bool = false) {
        self.id = id
        self.title = title
        self.start = start
        self.end = end
        self.allDay = allDay
    }
}

/// The events pinned in the app. Written only by the app (pin and unpin).
struct CountdownPinsSection: WidgetSection {
    static let key = "countdownPins"
    var fetchedAt: Date
    var events: [CountdownEvent]
}

/// The coming year's events as last fetched: the widget's configuration list, and the latest
/// title and time of a pinned event that was moved.
struct CountdownUpcomingSection: WidgetSection {
    static let key = "countdownUpcoming"
    var fetchedAt: Date
    var events: [CountdownEvent]
}

enum Countdown {
    /// How far ahead the event lists look (the server allows up to 400 days).
    static let horizonDays = 365
    /// The upcoming list is fetched again after this long.
    static let upcomingFreshFor: TimeInterval = 6 * 3600

    /// Calendar days from `now`'s day to the event's day: 0 today, 1 tomorrow, negative once past.
    static func days(until start: Date, from now: Date, calendar: Calendar = .current) -> Int {
        calendar.dateComponents([.day], from: calendar.startOfDay(for: now), to: calendar.startOfDay(for: start)).day ?? 0
    }

    /// "Today", "Tomorrow", "15 days".
    static func label(days: Int) -> String {
        switch days {
        case ...0: return String(localized: "Today")
        case 1: return String(localized: "Tomorrow")
        default: return String(localized: "\(days) days")
        }
    }

    static func label(_ event: CountdownEvent, now: Date, calendar: Calendar = .current) -> String {
        label(days: days(until: event.start, from: now, calendar: calendar))
    }

    /// The event's date under the countdown: "Tue 10 Nov 2026", with the time when it has one.
    static func dateText(_ event: CountdownEvent) -> String {
        let day = event.start.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated).year())
        return event.allDay ? day : "\(day), \(event.start.formatted(date: .omitted, time: .shortened))"
    }

    /// Still to come on `now`: an all-day event through its day, a timed one until it ends
    /// (until it starts when the end is unknown).
    static func isUpcoming(_ event: CountdownEvent, now: Date, calendar: Calendar = .current) -> Bool {
        if event.allDay { return days(until: event.start, from: now, calendar: calendar) >= 0 }
        return (event.end ?? event.start) > now
    }

    /// What the widget shows on `now`: the events still to come, soonest first, each with its
    /// latest title and time from `fresher` (the upcoming list), without duplicates.
    static func visible(_ events: [CountdownEvent], fresher: [CountdownEvent], now: Date,
                        calendar: Calendar = .current) -> [CountdownEvent] {
        let latest = Dictionary(fresher.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var seen = Set<String>()
        return events.compactMap { event -> CountdownEvent? in
            guard seen.insert(event.id).inserted else { return nil }
            let current = latest[event.id] ?? event
            return isUpcoming(current, now: now, calendar: calendar) ? current : nil
        }
        .sorted { ($0.start, $0.title) < ($1.start, $1.title) }
    }

    /// When the widget must redraw: the next `count` midnights and every event's end before the last.
    static func changes(after now: Date, events: [CountdownEvent], count: Int = 7, calendar: Calendar = .current) -> [Date] {
        let today = calendar.startOfDay(for: now)
        let midnights = (1...max(1, count)).compactMap { calendar.date(byAdding: .day, value: $0, to: today) }
        let last = midnights.last ?? now
        let ends = events.filter { !$0.allDay }.map { $0.end ?? $0.start }.filter { $0 > now && $0 < last }
        return Array(Set(midnights + ends)).sorted()
    }

    // MARK: Pins (the app)

    static var pinned: [CountdownEvent] { WidgetStore.read(CountdownPinsSection.self)?.events ?? [] }

    static func isPinned(_ id: String) -> Bool { pinned.contains { $0.id == id } }

    /// Pins or unpins an event, drops the past ones, and redraws the widget.
    static func setPinned(_ event: CountdownEvent, _ pin: Bool, now: Date = Date()) {
        var events = pinned.filter { $0.id != event.id && isUpcoming($0, now: now) }
        if pin { events.append(event) }
        WidgetStore.write(CountdownPinsSection(fetchedAt: now, events: events.sorted { $0.start < $1.start }))
        WidgetStore.reload([.countdown])
    }

    // MARK: The upcoming list

    static var upcoming: [CountdownEvent] { WidgetStore.read(CountdownUpcomingSection.self)?.events ?? [] }

    /// The coming year's events from the server (cached for `upcomingFreshFor` unless `force`);
    /// the cached copy when offline or signed out.
    static func refreshUpcoming(fetcher: WidgetFetcher? = WidgetFetcher.fromBridge(), force: Bool = false) async -> [CountdownEvent] {
        let cached = WidgetStore.read(CountdownUpcomingSection.self)
        if !force, let cached, Date().timeIntervalSince(cached.fetchedAt) < upcomingFreshFor { return cached.events }
        guard let fetcher, let fresh = try? await fetcher.upcomingEvents(days: horizonDays) else { return cached?.events ?? [] }
        WidgetStore.write(CountdownUpcomingSection(fetchedAt: Date(), events: fresh))
        return fresh
    }
}

extension WidgetFetcher {
    /// The next `days` days' events, soonest first (`GET /v1/calendar/events`).
    func upcomingEvents(days: Int, now: Date = Date()) async throws -> [CountdownEvent] {
        let page: JSONValue = try await client.get("/v1/calendar/events", query: [URLQueryItem(name: "days", value: String(days))], timeout: 20)
        return (page["events"]?.arrayValue ?? []).compactMap(Self.event)
            .map { CountdownEvent(id: $0.id, title: $0.title, start: $0.start, end: $0.end, allDay: $0.allDay) }
            .filter { Countdown.isUpcoming($0, now: now) }
            .sorted { $0.start < $1.start }
    }
}
