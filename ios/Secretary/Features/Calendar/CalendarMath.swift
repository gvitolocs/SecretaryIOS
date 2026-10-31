import Foundation
import SwiftUI

struct CalendarLink: Equatable, Hashable, Sendable {
    var eventId: String?
    var at: Date?

    static func parse(_ route: String) -> CalendarLink? {
        guard route.count <= 4096, !route.contains("\\"), let c = URLComponents(string: route), c.fragment == nil else { return nil }
        if c.scheme != nil {
            guard c.scheme == "https", c.host == "getsecretary.tech", c.port == nil, c.user == nil, c.password == nil else { return nil }
        } else if c.host != nil { return nil }
        guard c.path == "/calendar" || (c.scheme == nil && c.path == "calendar") else { return nil }
        let id = c.queryItems?.first(where: { $0.name == "event" })?.value
        guard id == nil || (!id!.isEmpty && id!.count <= 1100 && !id!.contains(where: { $0.isNewline })) else { return nil }
        let raw = c.queryItems?.first(where: { $0.name == "at" })?.value
        var at: Date?
        if let raw {
            guard let ms = Double(raw), ms.isFinite, abs(ms) < 8.64e15 else { return nil }
            at = Date(timeIntervalSince1970: ms / 1000)
        }
        return CalendarLink(eventId: id, at: at)
    }
}

extension AppModel {
    func openCalendar(_ link: CalendarLink?) {
        selectedTab = .plugins
        openCalendarEvent = link
        showCalendar = true
    }
}

enum CalendarMath {
    static func instant(_ date: Date) -> String { ISO8601DateFormatter().string(from: date) }
    static func parse(_ text: String) -> Date? {
        let f = ISO8601DateFormatter(); f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.date(from: text) ?? ISO8601DateFormatter().date(from: text)
    }
    static func ymd(_ date: Date, calendar: Calendar = .current) -> String {
        let c = calendar.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 1, c.day ?? 1)
    }
    static func day(_ text: String, calendar: Calendar = .current) -> Date? {
        let raw = String(text.prefix(10))
        guard raw.range(of: "^[0-9]{4}-[0-9]{2}-[0-9]{2}$", options: .regularExpression) != nil else { return nil }
        let parts = raw.split(separator: "-").compactMap { Int($0) }
        guard parts.count == 3, let date = calendar.date(from: DateComponents(year: parts[0], month: parts[1], day: parts[2])) else { return nil }
        return ymd(date, calendar: calendar) == raw ? date : nil
    }
    static func bounds(_ event: CalendarEntry, calendar: Calendar = .current) -> (Date, Date) {
        guard event.allDay, let start = day(event.start, calendar: calendar) else { return (event.startDate, event.endDate) }
        return (start, day(event.end, calendar: calendar) ?? calendar.date(byAdding: .day, value: 1, to: start)!)
    }
    static func events(_ events: [CalendarEntry], on date: Date, calendar: Calendar = .current) -> [CalendarEntry] {
        let start = calendar.startOfDay(for: date), end = calendar.date(byAdding: .day, value: 1, to: start)!
        return events.filter {
            let (s, e) = bounds($0, calendar: calendar)
            return s < end && e > start
        }.sorted {
            if $0.allDay != $1.allDay { return $0.allDay }
            if $0.startMs != $1.startMs { return $0.startMs < $1.startMs }
            return $0.title == $1.title ? $0.id < $1.id : $0.title < $1.title
        }
    }
    static func monthDays(_ date: Date, calendar: Calendar = .current) -> [Date] {
        let month = calendar.dateInterval(of: .month, for: date)!.start
        let offset = (calendar.component(.weekday, from: month) - calendar.firstWeekday + 7) % 7
        let first = calendar.date(byAdding: .day, value: -offset, to: month)!
        return (0..<42).map { calendar.date(byAdding: .day, value: $0, to: first)! }
    }
    struct DayGroup: Identifiable {
        var day: Date
        var events: [CalendarEntry]
        var id: Date { day }
    }
    static func agenda(_ events: [CalendarEntry], from: Date, to: Date, calendar: Calendar = .current) -> [DayGroup] {
        var date = calendar.startOfDay(for: from)
        var groups: [DayGroup] = []
        while date < to {
            let rows = self.events(events, on: date, calendar: calendar)
            if !rows.isEmpty { groups.append(DayGroup(day: date, events: rows)) }
            date = calendar.date(byAdding: .day, value: 1, to: date)!
        }
        return groups
    }
    static func defaultStart(_ day: Date, now: Date = Date(), calendar: Calendar = .current) -> Date {
        if calendar.isDate(day, inSameDayAs: now) {
            return calendar.date(byAdding: .hour, value: 1, to: calendar.dateInterval(of: .hour, for: now)!.start)!
        }
        return calendar.date(bySettingHour: 9, minute: 0, second: 0, of: day)!
    }
    static func shiftEnd(start: Date, end: Date, next: Date, allDay: Bool, calendar: Calendar = .current) -> Date {
        if allDay {
            let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: start), to: calendar.startOfDay(for: end)).day ?? 0
            return calendar.date(byAdding: .day, value: max(0, days), to: next)!
        }
        return next.addingTimeInterval(max(900, end.timeIntervalSince(start)))
    }
    static func validEnd(start: Date, end: Date, allDay: Bool, calendar: Calendar = .current) -> Bool {
        allDay ? calendar.startOfDay(for: end) >= calendar.startOfDay(for: start) : end > start
    }
    static func idCore(_ id: String) -> String {
        id.replacingOccurrences(of: "^(lucina|google|iphone|ics):", with: "", options: .regularExpression)
            .replacingOccurrences(of: "@[0-9]{10,}$", with: "", options: .regularExpression)
    }
    static func matchDeepLink(_ events: [CalendarEntry], id: String, at: Date?) -> CalendarEntry? {
        // Prefer the requested occurrence when an id is shared across a recurring series.
        let same = at.map { at in events.filter { abs($0.startDate.timeIntervalSince(at)) < 0.001 } } ?? []
        if let exact = same.first(where: { $0.id == id }) { return exact }
        if let exact = events.first(where: { $0.id == id }) { return exact }
        let core = idCore(id)
        guard at != nil else { return events.first { idCore($0.id) == core } }
        if let found = same.first(where: { let c = idCore($0.id); return c == core || core.contains(c) || c.contains(core) }) { return found }
        return same.count == 1 ? same[0] : nil
    }
    static func plainNotes(_ text: String?) -> String {
        var s = (text ?? "").replacingOccurrences(of: "<br\\s*/?>|</(?:p|div|li|h[0-9])>", with: "\n", options: [.regularExpression, .caseInsensitive])
            .replacingOccurrences(of: "<[^>]+>", with: "", options: .regularExpression)
        for (entity, value) in [("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&apos;", "'"), ("&nbsp;", " "), ("&amp;", "&")] { s = s.replacingOccurrences(of: entity, with: value) }
        return s.replacingOccurrences(of: "\n{3,}", with: "\n\n", options: .regularExpression).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    static func meetingURL(_ event: CalendarEntry) -> URL? {
        if let raw = event.conferenceUrl, let url = URL(string: raw), url.scheme == "https" { return url }
        let hosts = ["meet.google.com", "zoom.us", "teams.microsoft.com", "teams.live.com", "webex.com", "whereby.com"]
        let regex = try! NSRegularExpression(pattern: "https://[^\\s<>\"')\\]]+")
        for text in [event.location, event.description, event.url].compactMap({ $0 }) {
            for match in regex.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
                guard let range = Range(match.range, in: text) else { continue }
                let raw = String(text[range]).trimmingCharacters(in: CharacterSet(charactersIn: ".,;:!?"))
                if let url = URL(string: raw), let host = url.host?.lowercased(), hosts.contains(where: { host == $0 || host.hasSuffix("." + $0) }) { return url }
            }
        }
        return nil
    }
    static func colorIndex(_ name: String) -> Int {
        var hash: UInt32 = 2166136261
        for unit in name.utf16 { hash = (hash ^ UInt32(unit)) &* 16777619 }
        return Int(hash % 10)
    }
    static func color(name: String, source: String) -> Color {
        if source == "lucina" { return Theme.accent }
        if source == "google" && (name == "Google" || name.isEmpty) { return Color(red: 79/255, green: 124/255, blue: 240/255) }
        let palette: [UInt32] = [0xe5534b, 0xe8912d, 0xc9a227, 0x3fb950, 0x14a3a3, 0x1f9bcf, 0x4f7cf0, 0xa371f7, 0xdb61a2, 0xa0785a]
        let hex = palette[colorIndex(name)]
        return Color(red: Double((hex >> 16) & 255)/255, green: Double((hex >> 8) & 255)/255, blue: Double(hex & 255)/255)
    }
    static func dayLabel(_ date: Date, now: Date = Date(), calendar: Calendar = .current, locale: Locale = .current) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return String(localized: "Today") }
        if calendar.isDate(date, inSameDayAs: calendar.date(byAdding: .day, value: 1, to: now)!) { return String(localized: "Tomorrow") }
        return format(date, template: "EEE d MMM", calendar: calendar, locale: locale)
    }
    static func format(_ date: Date, template: String, calendar: Calendar = .current, locale: Locale = .current) -> String {
        let f = DateFormatter(); f.locale = locale; f.calendar = calendar; f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate(template)
        return f.string(from: date)
    }
    static func proposalWhen(_ proposal: CalendarPendingProposal, calendar: Calendar = .current, locale: Locale = .current) -> String {
        let draft = proposal.event
        guard let raw = draft.start else { return "" }
        var event = CalendarEntry()
        event.allDay = draft.allDay ?? (raw.count == 10)
        event.start = raw
        if event.allDay {
            guard let day = day(raw, calendar: calendar) else { return raw }
            event.end = draft.end ?? ymd(calendar.date(byAdding: .day, value: 1, to: day)!, calendar: calendar)
        } else {
            guard let start = parse(raw) else { return raw }
            event.startMs = start.timeIntervalSince1970 * 1000
            event.endMs = (draft.end.flatMap(parse) ?? start.addingTimeInterval(3600)).timeIntervalSince1970 * 1000
        }
        return whenLabel(event, calendar: calendar, locale: locale)
    }
    static func whenLabel(_ event: CalendarEntry, now: Date = Date(), calendar: Calendar = .current, locale: Locale = .current) -> String {
        let (start, end) = bounds(event, calendar: calendar)
        let first = dayLabel(start, now: now, calendar: calendar, locale: locale)
        if event.allDay {
            let last = calendar.date(byAdding: .day, value: -1, to: end)!
            return last > start ? first + " – " + dayLabel(last, now: now, calendar: calendar, locale: locale) : first
        }
        let s = format(start, template: "HHmm", calendar: calendar, locale: locale)
        let e = format(end, template: "HHmm", calendar: calendar, locale: locale)
        return calendar.isDate(start, inSameDayAs: end) ? first + " · " + s + "–" + e
            : first + ", " + s + " – " + dayLabel(end, now: now, calendar: calendar, locale: locale) + ", " + e
    }
}

struct CalendarForm {
    var title = ""
    var allDay = false
    var start: Date
    /// Inclusive last day in the all-day editor.
    var end: Date
    var location = ""
    var notes = ""
    var meeting = ""
    init(day: Date, event: CalendarEntry? = nil, calendar: Calendar = .current) {
        start = CalendarMath.defaultStart(day, calendar: calendar); end = start.addingTimeInterval(3600)
        if let event {
            title = event.title == "(no title)" ? "" : event.title; allDay = event.allDay
            (start, end) = CalendarMath.bounds(event, calendar: calendar)
            if allDay { end = calendar.date(byAdding: .day, value: -1, to: end)! }
            location = event.location ?? ""; notes = CalendarMath.plainNotes(event.description); meeting = event.conferenceUrl ?? ""
        }
    }
    var error: String? {
        if title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return String(localized: "Enter a title.") }
        if !CalendarMath.validEnd(start: start, end: end, allDay: allDay) { return String(localized: "The end must be after the start.") }
        return nil
    }
    func draft(editing: Bool, calendar: Calendar = .current) -> CalendarEventDraft {
        let trim: (String) -> String = { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
        return CalendarEventDraft(title: trim(title), start: allDay ? CalendarMath.ymd(start, calendar: calendar) : CalendarMath.instant(start),
            end: allDay ? CalendarMath.ymd(calendar.date(byAdding: .day, value: 1, to: end)!, calendar: calendar) : CalendarMath.instant(end),
            allDay: allDay, location: trim(location), description: trim(notes), conferenceUrl: editing || !trim(meeting).isEmpty ? trim(meeting) : nil,
            confirm: editing ? true : nil)
    }
}
