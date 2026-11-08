import Foundation

// The pure parts of the event detail (CalendarDetail): phone numbers in the text, the notes' live data, when notes
// fold, the day strip's layout and the alert / travel-time labels. Same rules as the web's event sheet.

/// A phone number found in an event's place, title or notes.
struct CalendarPhone: Equatable, Hashable, Sendable {
    /// As written ("52 20 01 44").
    let display: String
    /// `tel:` with the country code ("tel:+4552200144").
    let tel: String
    var url: URL? { URL(string: tel) }
}

extension CalendarMath {
    // MARK: Phone numbers

    /// `+` then 8-15 digits; `00` then 10-15 digits; a Danish number: 8 digits from 2-9, as 8, 4+4 or 2+2+2+2.
    private static let phonePatterns: [(NSRegularExpression, kind: Int)] = [
        (try! NSRegularExpression(pattern: "\\+\\d+(?:(?:[ .\\-]|[ ]?\\(|\\)[ ]?)\\d+)*"), 0),
        (try! NSRegularExpression(pattern: "00\\d+(?:[ .\\-]\\d+)*"), 1),
        (try! NSRegularExpression(pattern: "[2-9]\\d \\d{2} \\d{2} \\d{2}|[2-9]\\d{3} \\d{4}|[2-9]\\d{7}"), 2),
    ]
    private static let urlOrEmail = try! NSRegularExpression(
        pattern: "(?:https?://|www\\.)[^\\s<>\"]+|[A-Za-z0-9._%+\\-]+@[A-Za-z0-9\\-]+(?:\\.[A-Za-z0-9\\-]+)+", options: [.caseInsensitive])

    /// Where a phone number may not be: next to a letter, a digit, `/`, `-`, `.`, `:` or `@` (a room "5220-014", a date,
    /// a time, an id). A sentence's full stop after the number is fine.
    private static func phoneBoundary(_ s: NSString, _ r: NSRange) -> Bool {
        func blocked(_ c: unichar) -> Bool {
            guard let scalar = Unicode.Scalar(c) else { return false }
            return CharacterSet.letters.contains(scalar) || CharacterSet.decimalDigits.contains(scalar) || "/-.:@+".unicodeScalars.contains(scalar)
        }
        if r.location > 0, blocked(s.character(at: r.location - 1)) { return false }
        let end = r.location + r.length
        if end < s.length {
            let next = s.character(at: end)
            if next == 46 /* . */ {
                let after = end + 1 < s.length ? Unicode.Scalar(s.character(at: end + 1)) : nil
                if let after, !CharacterSet.whitespacesAndNewlines.contains(after) { return false }
            } else if blocked(next) { return false }
        }
        return true
    }

    /// The phone numbers of one text with their ranges, never inside a link or an email address (nor `excluding`).
    static func phoneMatches(in text: String, excluding: [NSRange] = []) -> [(range: NSRange, phone: CalendarPhone)] {
        let s = text as NSString
        let all = NSRange(location: 0, length: s.length)
        var taken = excluding + urlOrEmail.matches(in: text, range: all).map(\.range)
        var out: [(NSRange, CalendarPhone)] = []
        for (regex, kind) in phonePatterns {
            for m in regex.matches(in: text, range: all) {
                let r = m.range
                guard phoneBoundary(s, r), !taken.contains(where: { NSIntersectionRange($0, r).length > 0 }) else { continue }
                let display = s.substring(with: r)
                let digits = display.filter(\.isASCII).filter(\.isNumber)
                let tel: String
                switch kind {
                case 0:
                    guard (8...15).contains(digits.count) else { continue }
                    tel = "tel:+" + digits
                case 1:
                    guard (10...15).contains(digits.count) else { continue }
                    tel = "tel:+" + digits.dropFirst(2)
                default:
                    tel = "tel:+45" + digits
                }
                taken.append(r)
                out.append((r, CalendarPhone(display: display, tel: tel)))
            }
        }
        return out.sorted { $0.0.location < $1.0.location }
    }

    /// The numbers to call: the place's, then the title's, then the notes' (no repeats, at most three).
    static func phones(_ event: CalendarEntry) -> [CalendarPhone] {
        var seen = Set<String>()
        var out: [CalendarPhone] = []
        for text in [event.location ?? "", event.title, plainNotes(event.description)] {
            for (_, phone) in phoneMatches(in: text) where seen.insert(phone.tel).inserted {
                out.append(phone)
                if out.count == 3 { return out }
            }
        }
        return out
    }

    // MARK: Notes

    /// Notes fold to five lines (with "Show more") past 280 characters or five lines.
    static let notesFoldLines = 5
    static func notesFold(_ notes: String) -> Bool {
        notes.count > 280 || notes.split(separator: "\n", omittingEmptySubsequences: false).count > notesFoldLines
    }

    /// "Universitetsbyen 76, 8000 Aarhus C", "Via dei Mille 5, 00185 Roma": a street (capitalised words and the small
    /// ones between them), a number, maybe a floor, then a 4-5 digit postcode and a town. The web's pattern
    /// (lib/eventDetail.ts); NSDataDetector misses many Danish addresses.
    private static let danishAddress = try! NSRegularExpression(
        pattern: "\\p{Lu}[\\p{L}'.\\-]*(?: (?:\\p{Lu}[\\p{L}'.\\-]*|(?:di|da|de|del|della|delle|dei|degli|dello|la|le|lo|van|von|der|den|ved|på|i)(?= ))){0,4} \\d{1,4}[A-Za-z]?(?:, ?[\\p{L}\\d. ]{1,20})?, ?\\d{4,5} \\p{Lu}[\\p{L}\\-]+(?: (?:\\p{Lu}\\b|\\p{Lu}[\\p{L}\\-]+))?")

    /// Apple Maps for an address found in the notes.
    static func mapsURL(_ address: String) -> URL? {
        var c = URLComponents(string: "https://maps.apple.com/")!
        c.queryItems = [URLQueryItem(name: "q", value: address)]
        return c.url
    }

    /// The live parts of the notes: links and email addresses, phone numbers (by the rule above, not
    /// NSDataDetector's, which takes a room like "5220-014" for one) and addresses (to Apple Maps).
    static func noteLinks(_ text: String) -> [(range: NSRange, url: URL, kind: String)] {
        let s = text as NSString
        let all = NSRange(location: 0, length: s.length)
        var out: [(NSRange, URL, String)] = []
        func free(_ r: NSRange) -> Bool { !out.contains { NSIntersectionRange($0.0, r).length > 0 } }
        if let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) {
            for m in detector.matches(in: text, range: all) {
                guard let url = m.url, let scheme = url.scheme?.lowercased(), ["http", "https", "mailto"].contains(scheme) else { continue }
                out.append((m.range, url, scheme == "mailto" ? "email" : "link"))
            }
        }
        for (r, phone) in phoneMatches(in: text, excluding: out.map(\.0)) {
            if let url = phone.url, free(r) { out.append((r, url, "phone")) }
        }
        var addresses = danishAddress.matches(in: text, range: all).map(\.range)
        if let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.address.rawValue) {
            addresses += detector.matches(in: text, range: all).map(\.range)
        }
        for r in addresses where free(r) {
            if let url = mapsURL(s.substring(with: r)) { out.append((r, url, "address")) }
        }
        return out.sorted { $0.0.location < $1.0.location }
    }

    /// Google Maps' search for a place.
    static func googleMapsURL(_ place: String) -> URL? {
        var c = URLComponents(string: "https://www.google.com/maps/search/")!
        c.queryItems = [URLQueryItem(name: "api", value: "1"), URLQueryItem(name: "query", value: place)]
        return c.url
    }

    /// The notes with their live parts as links.
    static func liveNotes(_ text: String) -> AttributedString {
        let s = text as NSString
        var result = AttributedString()
        var at = 0
        for (r, url, _) in noteLinks(text) where r.location >= at {
            if r.location > at { result += AttributedString(s.substring(with: NSRange(location: at, length: r.location - at))) }
            var link = AttributedString(s.substring(with: r))
            link.link = url
            result += link
            at = r.location + r.length
        }
        if at < s.length { result += AttributedString(s.substring(from: at)) }
        return result
    }

    // MARK: Day strip

    struct DayStrip: Equatable {
        struct Item: Identifiable, Equatable {
            let event: CalendarEntry
            /// Clipped to the strip.
            let start: Date
            let end: Date
            let column: Int
            let columns: Int
            let current: Bool
            var id: String { event.id + "@" + String(event.startMs) }
        }
        let start: Date
        let end: Date
        /// The full hours shown, from `start` to `end`.
        let hours: [Date]
        let items: [Item]
    }

    /// The hours around a timed event (an hour either side, at least three) and the timed events in them,
    /// overlapping ones side by side. Nil for all-day events.
    static func dayStrip(for event: CalendarEntry, among events: [CalendarEntry], calendar: Calendar = .current) -> DayStrip? {
        guard !event.allDay, event.endMs > event.startMs else { return nil }
        let hour = { (d: Date) in calendar.dateInterval(of: .hour, for: d)!.start }
        let start = hour(event.startDate.addingTimeInterval(-3600))
        let lastHour = hour(event.endDate.addingTimeInterval(3600 - 1))
        var end = calendar.date(byAdding: .hour, value: 1, to: lastHour)!
        if end.timeIntervalSince(start) < 3 * 3600 { end = start.addingTimeInterval(3 * 3600) }
        var hours: [Date] = []
        var h = start
        while h <= end { hours.append(h); h = calendar.date(byAdding: .hour, value: 1, to: h)! }
        let isCurrent = { (e: CalendarEntry) in e.id == event.id && e.startMs == event.startMs }
        var shown = events.filter { !$0.allDay && $0.endMs > $0.startMs && $0.startDate < end && $0.endDate > start && !isCurrent($0) }
        shown.append(event)
        shown.sort { $0.startMs != $1.startMs ? $0.startMs < $1.startMs : ($0.endMs != $1.endMs ? $0.endMs > $1.endMs : $0.id < $1.id) }
        // Clusters of overlapping events; inside one, each event takes the first free column.
        var items: [DayStrip.Item] = []
        var cluster: [(CalendarEntry, Int)] = []
        var clusterEnd: Double = -.infinity
        var columnEnds: [Double] = []
        func flush() {
            let columns = max(1, columnEnds.count)
            for (e, column) in cluster {
                items.append(.init(event: e, start: max(e.startDate, start), end: min(e.endDate, end), column: column, columns: columns, current: isCurrent(e)))
            }
            cluster = []; columnEnds = []
        }
        for e in shown {
            if e.startMs >= clusterEnd { flush() }
            let column = columnEnds.firstIndex { $0 <= e.startMs } ?? columnEnds.count
            if column == columnEnds.count { columnEnds.append(e.endMs) } else { columnEnds[column] = e.endMs }
            cluster.append((e, column))
            clusterEnd = max(clusterEnd == -.infinity ? e.endMs : clusterEnd, e.endMs)
        }
        flush()
        return DayStrip(start: start, end: end, hours: hours, items: items)
    }

    // MARK: The iPhone's own events

    /// `iphone:<EventKit id>@<start ms>` → the EventKit id (itself full of `:` and `@`); nil for other sources.
    static func deviceEventIdentifier(_ id: String) -> String? {
        guard id.hasPrefix("iphone:") else { return nil }
        var rest = id.dropFirst("iphone:".count)
        if let at = rest.lastIndex(of: "@"), rest[rest.index(after: at)...].allSatisfy(\.isNumber), rest.index(after: at) < rest.endIndex {
            rest = rest[..<at]
        }
        return rest.isEmpty ? nil : String(rest)
    }

    /// EKAlarm offsets (seconds, negative before the start) → minutes before, ascending; alerts after the start are left out.
    static func alarmMinutes(offsets: [TimeInterval]) -> [Int] {
        Array(Set(offsets.filter { $0 <= 0 }.map { Int((-$0 / 60).rounded()) })).sorted()
    }

    /// Minutes before the start → EKAlarm offsets.
    static func alarmOffsets(minutes: [Int]) -> [TimeInterval] {
        Array(Set(minutes)).sorted().prefix(maxAlarms).map { -TimeInterval($0) * 60 }
    }

    // MARK: Settings labels

    /// Alert choices, minutes before the start.
    static let alarmChoices = [0, 5, 10, 15, 30, 60, 120, 1440, 2880, 10080]
    /// Travel time choices, minutes (0: none).
    static let travelChoices = [0, 5, 15, 30, 60, 90, 120]
    static let maxAlarms = 5

    /// "15 minutes", "1 hour 30 minutes", "1 day", "1 week".
    static func minutesText(_ minutes: Int) -> String {
        Duration.seconds(minutes * 60).formatted(.units(allowed: [.weeks, .days, .hours, .minutes], width: .wide))
    }
    /// "At time of event", "1 hour before".
    static func alarmLabel(_ minutes: Int) -> String {
        minutes == 0 ? String(localized: "At time of event") : String(localized: "\(minutesText(minutes)) before")
    }
    static func travelLabel(_ minutes: Int?) -> String {
        guard let minutes, minutes > 0 else { return String(localized: "None") }
        return minutesText(minutes)
    }

    /// A computed travel time rounded up to five minutes (at least five, at most a day).
    static func travelMinutes(seconds: Double) -> Int? {
        guard seconds.isFinite, seconds > 0 else { return nil }
        return min(1440, max(5, Int((seconds / 60 / 5).rounded(.up)) * 5))
    }

    /// "mandag den 2. november 2026" / "Monday 2 November 2026".
    static func longDay(_ date: Date, calendar: Calendar = .current, locale: Locale = .current) -> String {
        format(date, template: "EEEEdMMMMyyyy", calendar: calendar, locale: locale)
    }
}
